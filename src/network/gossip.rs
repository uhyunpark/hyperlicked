//! Gossip Protocol for Epidemic Message Propagation
//!
//! Implements a push-based gossip protocol for reliable message propagation
//! across the validator network. This provides resilience when direct TCP
//! connections fail.
//!
//! ## Algorithm
//!
//! ```text
//! on send(msg):
//!   msg_id = hash(msg)
//!   if msg_id in seen_cache: return
//!   seen_cache.insert(msg_id)
//!   wrapped = GossipMessage { msg, ttl, origin, msg_id }
//!   peers = sample(all_peers, fanout)
//!   for peer in peers: send_to(peer, wrapped)
//!
//! on receive(gossip_msg):
//!   if gossip_msg.msg_id in seen_cache: return
//!   seen_cache.insert(gossip_msg.msg_id)
//!   deliver(gossip_msg.msg)  // to consensus
//!   if gossip_msg.ttl > 1:
//!     relay(GossipMessage { ..gossip_msg, ttl: ttl - 1 })
//! ```
//!
//! ## Configuration
//!
//! - `fanout`: Number of peers to forward to (default: 3)
//! - `ttl`: Initial time-to-live for messages (default: 5 hops)
//! - `cache_size`: Maximum seen message IDs to track (default: 10000)

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::consensus::EquivocationProof;
use crate::types::{Hash, Message, NodeId};

/// Maximum number of authenticated equivocation keys retained for fast
/// duplicate suppression.  This is intentionally bounded independently of
/// the payload seen cache: an attacker must not be able to fill the stable
/// evidence set with arbitrary message IDs or proof variants.
const MAX_EQUIVOCATION_KEYS: usize = 256;

/// Configuration for the gossip protocol
#[derive(Debug, Clone)]
pub struct GossipConfig {
    /// Number of peers to forward each message to
    pub fanout: usize,
    /// Initial TTL for new messages (decremented on each hop)
    pub ttl: u8,
    /// Maximum number of message IDs to track in seen cache
    pub cache_size: usize,
    /// Whether gossip is enabled
    pub enabled: bool,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            fanout: 3,
            ttl: 5,
            cache_size: 10_000,
            enabled: true,
        }
    }
}

impl GossipConfig {
    /// Validate resource-sensitive gossip settings before constructing state.
    pub fn validate(&self) -> Result<(), String> {
        if self.cache_size == 0 {
            return Err("gossip cache_size must be greater than zero".to_string());
        }
        Ok(())
    }

    /// Load gossip config from environment variables
    pub fn from_env() -> Self {
        Self {
            fanout: std::env::var("GOSSIP_FANOUT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3),
            ttl: std::env::var("GOSSIP_TTL")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5),
            cache_size: std::env::var("GOSSIP_CACHE_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10_000),
            enabled: std::env::var("GOSSIP_ENABLED")
                .map(|s| s == "true" || s == "1")
                .unwrap_or(true), // Enabled by default
        }
    }
}

/// Unique identifier for a gossip message
pub type MessageId = Hash;

/// A message wrapped with gossip metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipMessage {
    /// The original message
    pub message: Message,
    /// Time-to-live (hops remaining)
    pub ttl: u8,
    /// Original sender
    pub origin: NodeId,
    /// Unique message identifier (hash of message content)
    pub msg_id: MessageId,
}

impl GossipMessage {
    /// Create a new gossip message
    pub fn new(message: Message, ttl: u8, origin: NodeId) -> Self {
        let msg_id = compute_message_id(&message);
        Self {
            message,
            ttl,
            origin,
            msg_id,
        }
    }

    /// Create a relayed copy with decremented TTL
    pub fn relay(&self) -> Option<Self> {
        if self.ttl <= 1 {
            return None;
        }
        Some(Self {
            message: self.message.clone(),
            ttl: self.ttl - 1,
            origin: self.origin,
            msg_id: self.msg_id,
        })
    }

    /// Check if this message should continue to propagate
    pub fn should_relay(&self) -> bool {
        self.ttl > 1
    }
}

/// Compute a unique ID for a message based on its content.
///
/// The ID intentionally excludes gossip metadata.  Relays therefore retain
/// one stable identity while the TTL changes on each hop.
pub fn compute_message_id(msg: &Message) -> MessageId {
    let mut hasher = Sha256::new();
    // Serialize message to bytes for hashing
    if let Ok(bytes) = bincode::serialize(msg) {
        hasher.update(&bytes);
    }
    let hash = hasher.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&hash);
    result
}

/// Validate the untrusted gossip envelope without changing the seen cache.
/// Cryptographic/committee validation of the inner consensus message belongs
/// to the transport admission gate, which calls this check first.
pub fn validate_gossip_envelope(gossip_msg: &GossipMessage, initial_ttl: u8) -> Result<(), String> {
    if gossip_msg.ttl == 0 {
        return Err("gossip TTL must be greater than zero".to_string());
    }
    if initial_ttl == 0 {
        return Err("gossip is configured with an invalid zero initial TTL".to_string());
    }
    if gossip_msg.ttl > initial_ttl {
        return Err("gossip TTL exceeds the configured initial TTL".to_string());
    }
    if gossip_msg.msg_id != compute_message_id(&gossip_msg.message) {
        return Err("gossip message ID does not match its payload".to_string());
    }
    match &gossip_msg.message {
        Message::Gossip(_) => Err("nested gossip envelopes are forbidden".to_string()),
        Message::SyncRequest(_)
        | Message::SyncResponse(_)
        | Message::SnapshotRequest(_)
        | Message::SnapshotResponse(_) => {
            Err("sync messages cannot be propagated through gossip".to_string())
        }
        _ => Ok(()),
    }
}

/// State for the gossip protocol
///
/// Thread-safe tracking of seen messages to prevent duplicate processing
/// and infinite loops.
pub struct GossipState {
    /// Configuration
    config: GossipConfig,
    /// Set of recently seen message IDs
    seen: Arc<RwLock<SeenCache>>,
    /// Authenticated `(context, offender)` keys for evidence already
    /// admitted.  This cache is populated only after committee/BLS
    /// validation, unlike the payload cache which tracks message IDs.
    equivocation_keys: Arc<RwLock<EquivocationKeyCache>>,
}

impl GossipState {
    /// Create new gossip state with given config
    pub fn new(config: GossipConfig) -> Self {
        Self::try_new(config).expect("invalid gossip configuration")
    }

    /// Fallible constructor used by network initialization so invalid
    /// resource limits fail closed instead of panicking a node task.
    pub fn try_new(config: GossipConfig) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            seen: Arc::new(RwLock::new(SeenCache::new(config.cache_size))),
            equivocation_keys: Arc::new(RwLock::new(EquivocationKeyCache::new(
                MAX_EQUIVOCATION_KEYS,
            ))),
            config,
        })
    }

    /// Check if we've seen this message before, and mark it as seen
    ///
    /// Returns `true` if this is a new message, `false` if already seen.
    pub fn mark_seen(&self, msg_id: &MessageId) -> bool {
        let mut seen = self.seen.write().expect("lock poisoned");
        seen.insert(*msg_id)
    }

    /// Check if we've seen a message without marking it
    pub fn has_seen(&self, msg_id: &MessageId) -> bool {
        self.seen.read().expect("lock poisoned").contains(msg_id)
    }

    /// Get the gossip configuration
    pub fn config(&self) -> &GossipConfig {
        &self.config
    }

    /// Check if gossip is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get the fanout count
    pub fn fanout(&self) -> usize {
        self.config.fanout
    }

    /// Get the initial TTL
    pub fn initial_ttl(&self) -> u8 {
        self.config.ttl
    }

    /// Create a new gossip message for broadcasting
    pub fn wrap_message(&self, message: Message, origin: NodeId) -> GossipMessage {
        GossipMessage::new(message, self.config.ttl, origin)
    }

    /// Process a received gossip message
    ///
    /// Returns `Some(message)` if this is a new message that should be delivered,
    /// `None` if it's a duplicate.
    pub fn receive(&self, gossip_msg: &GossipMessage) -> Option<Message> {
        // Never mark a claimed ID before validating the bytes that produced
        // it.  This is the important cache-poisoning boundary: malformed
        // envelopes must have zero seen-cache side effects.
        if validate_gossip_envelope(gossip_msg, self.config.ttl).is_err() {
            return None;
        }
        if self.mark_seen(&gossip_msg.msg_id) {
            Some(gossip_msg.message.clone())
        } else {
            None
        }
    }

    /// Get cache statistics for monitoring
    pub fn stats(&self) -> GossipStats {
        let seen = self.seen.read().expect("lock poisoned");
        GossipStats {
            seen_count: seen.len(),
            cache_capacity: self.config.cache_size,
        }
    }

    /// Return whether a proof's authenticated context/offender key was
    /// already admitted.  This check is safe before expensive BLS work: a
    /// key only enters the cache through `validate_and_mark_equivocation`.
    pub(crate) fn has_equivocation_key(&self, proof: &EquivocationProof) -> bool {
        let key = EquivocationKey::from(proof);
        self.equivocation_keys
            .read()
            .expect("lock poisoned")
            .contains(&key)
    }

    /// Validate one evidence proof and atomically mark its stable key.  The
    /// validation callback runs while the write lock is held so concurrent
    /// alternate payloads cannot all repeat BLS validation and delivery.
    /// Invalid proofs never mutate the cache.
    pub(crate) fn validate_and_mark_equivocation<F, E>(
        &self,
        proof: &EquivocationProof,
        validate: F,
    ) -> Result<bool, E>
    where
        F: FnOnce() -> Result<(), E>,
    {
        let key = EquivocationKey::from(proof);
        let mut keys = self.equivocation_keys.write().expect("lock poisoned");
        if keys.contains(&key) {
            return Ok(false);
        }
        validate()?;
        Ok(keys.insert(key))
    }
}

impl Default for GossipState {
    fn default() -> Self {
        Self::new(GossipConfig::default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EquivocationKey {
    epoch: u64,
    committee_hash: Hash,
    genesis_hash: Hash,
    offender: NodeId,
}

impl From<&EquivocationProof> for EquivocationKey {
    fn from(proof: &EquivocationProof) -> Self {
        Self {
            epoch: proof.context.epoch,
            committee_hash: proof.context.committee_hash,
            genesis_hash: proof.context.genesis_hash,
            offender: proof.offender,
        }
    }
}

/// Bounded FIFO cache for authenticated evidence keys.
struct EquivocationKeyCache {
    set: HashSet<EquivocationKey>,
    queue: VecDeque<EquivocationKey>,
    capacity: usize,
}

impl EquivocationKeyCache {
    fn new(capacity: usize) -> Self {
        Self {
            set: HashSet::with_capacity(capacity),
            queue: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn contains(&self, key: &EquivocationKey) -> bool {
        self.set.contains(key)
    }

    fn insert(&mut self, key: EquivocationKey) -> bool {
        if self.set.contains(&key) {
            return false;
        }
        if self.queue.len() >= self.capacity {
            if let Some(old) = self.queue.pop_front() {
                self.set.remove(&old);
            }
        }
        self.set.insert(key);
        self.queue.push_back(key);
        true
    }
}

/// Statistics for monitoring gossip performance
#[derive(Debug, Clone)]
pub struct GossipStats {
    /// Number of message IDs currently tracked
    pub seen_count: usize,
    /// Maximum cache capacity
    pub cache_capacity: usize,
}

/// Bounded cache of seen message IDs
///
/// Uses a simple FIFO eviction when the cache is full.
struct SeenCache {
    /// Set for O(1) lookup
    set: HashSet<MessageId>,
    /// Queue for FIFO eviction
    queue: Vec<MessageId>,
    /// Maximum size
    capacity: usize,
}

impl SeenCache {
    fn new(capacity: usize) -> Self {
        Self {
            set: HashSet::with_capacity(capacity),
            queue: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Insert a message ID, returning true if it was new
    fn insert(&mut self, msg_id: MessageId) -> bool {
        if self.set.contains(&msg_id) {
            return false;
        }

        // Evict oldest if at capacity
        if self.queue.len() >= self.capacity {
            if let Some(old) = self.queue.first().copied() {
                self.set.remove(&old);
                self.queue.remove(0);
            }
        }

        self.set.insert(msg_id);
        self.queue.push(msg_id);
        true
    }

    fn contains(&self, msg_id: &MessageId) -> bool {
        self.set.contains(msg_id)
    }

    fn len(&self) -> usize {
        self.set.len()
    }
}

/// Select random peers for gossip propagation
///
/// Uses a simple deterministic selection based on message ID to ensure
/// consistent peer selection across nodes (helps with testing).
pub fn select_gossip_peers(
    all_peers: &[NodeId],
    msg_id: &MessageId,
    fanout: usize,
    exclude: Option<&NodeId>,
) -> Vec<NodeId> {
    if all_peers.is_empty() || fanout == 0 {
        return Vec::new();
    }

    // Filter out excluded peer
    let available: Vec<&NodeId> = all_peers
        .iter()
        .filter(|p| exclude.map(|e| *p != e).unwrap_or(true))
        .collect();

    if available.is_empty() {
        return Vec::new();
    }

    // Use msg_id as seed for deterministic but distributed selection
    let seed = u64::from_le_bytes(msg_id[..8].try_into().unwrap_or([0u8; 8]));

    // Select up to fanout peers using a simple hash-based selection
    let mut selected = Vec::with_capacity(fanout.min(available.len()));
    for i in 0..fanout.min(available.len()) {
        // Simple hash to select index
        let idx = ((seed.wrapping_add(i as u64)) as usize) % available.len();

        // Find an unselected peer starting from idx
        for j in 0..available.len() {
            let check_idx = (idx + j) % available.len();
            let peer = available[check_idx];
            if !selected.contains(peer) {
                selected.push(*peer);
                break;
            }
        }
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Block, ConsensusContext};

    fn test_message() -> Message {
        let context = ConsensusContext::new(0, [7u8; 32]);
        Message::Propose(crate::types::Propose {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            block: Block::genesis(context),
            justify: None,
            proposer_signature: vec![],
        })
    }

    fn test_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn test_gossip_message_creation() {
        let msg = test_message();
        let origin = test_node_id(1);
        let gossip_msg = GossipMessage::new(msg.clone(), 5, origin);

        assert_eq!(gossip_msg.ttl, 5);
        assert_eq!(gossip_msg.origin, origin);
        assert!(gossip_msg.should_relay());
    }

    #[test]
    fn test_gossip_message_relay() {
        let msg = test_message();
        let origin = test_node_id(1);
        let gossip_msg = GossipMessage::new(msg, 2, origin);

        // Can relay with TTL > 1
        let relayed = gossip_msg.relay().expect("should relay");
        assert_eq!(relayed.ttl, 1);
        assert_eq!(relayed.msg_id, gossip_msg.msg_id);

        // Cannot relay with TTL = 1
        assert!(relayed.relay().is_none());
    }

    #[test]
    fn test_gossip_state_deduplication() {
        let state = GossipState::default();
        let msg = test_message();
        let origin = test_node_id(1);
        let gossip_msg = GossipMessage::new(msg, 5, origin);

        // First receive should succeed
        assert!(state.receive(&gossip_msg).is_some());

        // Second receive should be deduplicated
        assert!(state.receive(&gossip_msg).is_none());
    }

    #[test]
    fn test_seen_cache_eviction() {
        let config = GossipConfig {
            cache_size: 3,
            ..Default::default()
        };
        let state = GossipState::new(config);

        // Insert 3 messages
        for i in 0..3 {
            let msg_id = [i as u8; 32];
            assert!(state.mark_seen(&msg_id));
        }

        // All 3 should be seen
        assert!(state.has_seen(&[0u8; 32]));
        assert!(state.has_seen(&[1u8; 32]));
        assert!(state.has_seen(&[2u8; 32]));

        // Insert a 4th message, should evict first
        let msg_id = [3u8; 32];
        assert!(state.mark_seen(&msg_id));

        // First should be evicted
        assert!(!state.has_seen(&[0u8; 32]));
        assert!(state.has_seen(&[1u8; 32]));
        assert!(state.has_seen(&[2u8; 32]));
        assert!(state.has_seen(&[3u8; 32]));
    }

    #[test]
    fn test_peer_selection() {
        let peers: Vec<NodeId> = (1..=5).map(test_node_id).collect();
        let msg_id = [42u8; 32];

        // Select 3 peers
        let selected = select_gossip_peers(&peers, &msg_id, 3, None);
        assert_eq!(selected.len(), 3);

        // All selected should be unique
        let unique: HashSet<_> = selected.iter().collect();
        assert_eq!(unique.len(), 3);

        // Same msg_id should select same peers (deterministic)
        let selected2 = select_gossip_peers(&peers, &msg_id, 3, None);
        assert_eq!(selected, selected2);
    }

    #[test]
    fn test_peer_selection_with_exclusion() {
        let peers: Vec<NodeId> = (1..=3).map(test_node_id).collect();
        let msg_id = [42u8; 32];
        let exclude = test_node_id(2);

        let selected = select_gossip_peers(&peers, &msg_id, 3, Some(&exclude));

        // Should not include excluded peer
        assert!(!selected.contains(&exclude));
        assert_eq!(selected.len(), 2); // Only 2 available
    }

    #[test]
    fn test_gossip_config_from_env() {
        // Default values when env vars not set
        let config = GossipConfig::from_env();
        assert_eq!(config.fanout, 3);
        assert_eq!(config.ttl, 5);
        assert_eq!(config.cache_size, 10_000);
    }

    #[test]
    fn zero_cache_capacity_is_rejected_before_state_creation() {
        let config = GossipConfig {
            cache_size: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
        assert!(GossipState::try_new(config).is_err());
    }

    #[test]
    fn test_message_id_deterministic() {
        let msg = test_message();
        let id1 = compute_message_id(&msg);
        let id2 = compute_message_id(&msg);
        assert_eq!(id1, id2);
    }

    #[test]
    fn malformed_envelopes_do_not_poison_seen_cache() {
        let state = GossipState::default();
        let message = test_message();
        let origin = test_node_id(1);

        let mut forged_id = GossipMessage::new(message.clone(), 5, origin);
        forged_id.msg_id = [0xabu8; 32];
        assert!(state.receive(&forged_id).is_none());
        assert_eq!(state.stats().seen_count, 0);

        let zero_ttl = GossipMessage::new(message.clone(), 0, origin);
        assert!(state.receive(&zero_ttl).is_none());
        assert_eq!(state.stats().seen_count, 0);

        let excessive_ttl = GossipMessage::new(message, 6, origin);
        assert!(state.receive(&excessive_ttl).is_none());
        assert_eq!(state.stats().seen_count, 0);
    }

    #[test]
    fn gossip_rejects_nested_and_sync_messages() {
        let state = GossipState::default();
        let origin = test_node_id(1);
        let nested = GossipMessage::new(
            Message::Gossip(Box::new(GossipMessage::new(test_message(), 5, origin))),
            5,
            origin,
        );
        assert!(state.receive(&nested).is_none());

        let sync = GossipMessage::new(
            Message::SyncRequest(crate::types::SyncRequest {
                from_height: 0,
                to_height: None,
                max_blocks: 1,
                request_id: 1,
            }),
            5,
            origin,
        );
        assert!(state.receive(&sync).is_none());
        assert_eq!(state.stats().seen_count, 0);
    }
}
