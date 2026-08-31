//! Network Layer for Multi-Node Consensus
//!
//! Provides TCP-based networking for HotStuff-2 consensus:
//! - Broadcast proposals to all validators
//! - Send votes to leader
//! - Broadcast prepare certificates
//!
//! ## Security (CRITICAL-6)
//!
//! The network supports BLS-authenticated handshakes to prevent impersonation:
//! - In production mode (testnet/mainnet), `require_authenticated_peers = true`
//! - In dev mode, authentication can be disabled for local testing
//!
//! See `HandshakeConfig::authenticated()` for setup.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │                  TcpNetwork                     │
//! │  ┌─────────────┐      ┌─────────────────────┐  │
//! │  │   Listener  │      │   Peer Connections  │  │
//! │  │  (accept)   │      │  HashMap<NodeId,    │  │
//! │  │             │      │    TcpStream>       │  │
//! │  └─────────────┘      └─────────────────────┘  │
//! │         │                       │              │
//! │         ▼                       ▼              │
//! │  ┌─────────────────────────────────────────┐   │
//! │  │           Message Channels              │   │
//! │  │  propose_rx, vote_rx, prepare_rx        │   │
//! │  └─────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────┘
//! ```

pub mod active_sync;
pub mod gossip;
pub mod handshake;
mod mock;
pub mod sync;
mod transport;

use std::collections::HashMap;

use anyhow::{anyhow, Result};

pub use active_sync::{
    ActiveSyncClient, ActiveSyncConfig, PeerFinalityProofExport, SyncResult, VerifiedFinalityProof,
    VerifiedFinalizedBatch,
};
pub use gossip::{
    compute_message_id, select_gossip_peers, validate_gossip_envelope, GossipConfig, GossipMessage,
    GossipState, GossipStats,
};
pub use handshake::{HandshakeConfig, HandshakeResult};
pub use mock::MockNetwork;
pub use sync::{SyncClient, SyncHandler};
pub use transport::{TcpNetwork, TransactionBroadcaster};

use crate::crypto::bls::{BlsPublicKey, BlsSecretKey};

use async_trait::async_trait;

use crate::app::SignedEnvelope;
use crate::types::{
    Committee, ConsensusContext, Message, NewView, NodeId, Prepare, Propose, ViewChange, Vote,
};

/// Secret-free validation material used by the transport admission gate.
///
/// This is deliberately separate from the local BLS secret.  Every validator
/// may validate a relayed message using the trusted genesis context and active
/// committee, but no transport task receives signing authority.
#[derive(Clone)]
pub struct GossipValidationConfig {
    pub context: ConsensusContext,
    pub committee: Committee,
    /// Development-only envelope marker accepted by the canonical app when
    /// the runtime is explicitly in `MODE=dev`. Production modes leave this
    /// disabled so `SignatureScheme::Dev` cannot cross validator links.
    pub allow_dev_envelopes: bool,
}

impl std::fmt::Debug for GossipValidationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GossipValidationConfig")
            .field("epoch", &self.context.epoch)
            .field(
                "committee_hash",
                &crate::types::hash_short(&self.context.committee_hash),
            )
            .field(
                "genesis_hash",
                &crate::types::hash_short(&self.context.genesis_hash),
            )
            .field("committee_members", &self.committee.members().len())
            .field("allow_dev_envelopes", &self.allow_dev_envelopes)
            .finish()
    }
}

/// Network abstraction for consensus
///
/// Implementations handle the actual message transport (TCP, libp2p, etc.)
#[async_trait]
pub trait Network: Send + Sync {
    /// Broadcast a proposal to all validators
    async fn broadcast_propose(&self, propose: Propose) -> anyhow::Result<()>;

    /// Send a vote to the leader
    async fn send_vote(&self, to: NodeId, vote: Vote) -> anyhow::Result<()>;

    /// Broadcast a prepare certificate to all validators
    async fn broadcast_prepare(&self, prepare: Prepare) -> anyhow::Result<()>;

    /// Broadcast a view change message to all validators
    async fn broadcast_view_change(&self, vc: ViewChange) -> anyhow::Result<()>;

    /// Broadcast a new view message to all validators (sent by new leader)
    async fn broadcast_new_view(&self, nv: NewView) -> anyhow::Result<()>;

    /// Broadcast any message to all validators (generic)
    async fn broadcast(&self, msg: &Message) -> anyhow::Result<()>;

    /// Send a message to a specific peer
    async fn send_to(&self, to: NodeId, msg: &Message) -> anyhow::Result<()>;

    /// Receive the next incoming message (blocking)
    async fn recv(&self) -> anyhow::Result<(NodeId, Message)>;

    /// Get our own node ID
    fn node_id(&self) -> NodeId;
}

/// Outbound-only publisher used by the API ingress path.
///
/// Keeping this capability separate from [`Network`] means the API never owns
/// or drives the consensus receive loop.  A live node gives the API a clone of
/// the transport's outbound handle while the consensus runner retains the
/// `TcpNetwork` receiver.
#[async_trait]
pub trait UserTransactionPublisher: Send + Sync {
    async fn publish_user_transaction(&self, envelope: SignedEnvelope) -> anyhow::Result<()>;

    /// Retry one still-pending envelope as a raw direct message to every
    /// currently connected validator.  Retries intentionally bypass the
    /// probabilistic gossip fanout so a peer that missed the original send
    /// gets another chance without requiring a new protocol message type.
    async fn rebroadcast_user_transaction(&self, envelope: SignedEnvelope) -> anyhow::Result<()>;
}

/// Network configuration
#[derive(Clone)]
pub struct NetworkConfig {
    /// Our node ID
    pub node_id: NodeId,
    /// Our listen address (e.g., "0.0.0.0:9000")
    pub listen_addr: String,
    /// Peer addresses: (NodeId, "host:port")
    pub peers: Vec<(NodeId, String)>,
    /// CRITICAL-6: Whether to require authenticated peers
    /// Default: true in testnet/mainnet, false in dev mode
    pub require_authenticated_peers: bool,
    /// Our BLS secret key for authentication (optional in dev mode)
    pub bls_secret_key: Option<BlsSecretKey>,
    /// Known validator BLS public keys for authentication
    pub validator_pubkeys: HashMap<NodeId, BlsPublicKey>,
    /// Trusted, secret-free context used to validate gossip before relay.
    /// `None` is valid only for development/direct-broadcast mode; gossip is
    /// disabled automatically in that case.
    pub gossip_validation: Option<GossipValidationConfig>,
}

impl std::fmt::Debug for NetworkConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkConfig")
            .field("node_id", &crate::types::hash_short(&self.node_id))
            .field("listen_addr", &self.listen_addr)
            .field("peers", &self.peers.len())
            .field(
                "require_authenticated_peers",
                &self.require_authenticated_peers,
            )
            .field("has_bls_key", &self.bls_secret_key.is_some())
            .field("validator_pubkeys", &self.validator_pubkeys.len())
            .field("gossip_validation", &self.gossip_validation)
            .finish()
    }
}

impl NetworkConfig {
    /// Create config for local 3-node testing.
    ///
    /// The loopback addresses and node IDs are development-only fixtures. A
    /// caller that runs validators over TCP should enable authentication with
    /// [`NetworkConfig::with_authentication`].
    pub fn local_three_nodes(node_index: usize) -> Self {
        let node_ids: [NodeId; 3] = [
            [1u8; 32], // Node 0
            [2u8; 32], // Node 1
            [3u8; 32], // Node 2
        ];

        let ports = [9000, 9001, 9002];

        let node_id = node_ids[node_index];
        let listen_addr = format!("127.0.0.1:{}", ports[node_index]);

        // Connect to all other nodes
        let peers: Vec<(NodeId, String)> = node_ids
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != node_index)
            .map(|(i, &id)| (id, format!("127.0.0.1:{}", ports[i])))
            .collect();

        Self {
            node_id,
            listen_addr,
            peers,
            require_authenticated_peers: false, // Dev mode: no auth
            bls_secret_key: None,
            validator_pubkeys: HashMap::new(),
            gossip_validation: None,
        }
    }

    /// Create config with BLS authentication enabled (CRITICAL-6)
    pub fn with_authentication(
        mut self,
        bls_sk: BlsSecretKey,
        validator_pubkeys: HashMap<NodeId, BlsPublicKey>,
    ) -> Self {
        self.bls_secret_key = Some(bls_sk);
        self.validator_pubkeys = validator_pubkeys;
        self.require_authenticated_peers = true;
        self
    }

    /// Attach trusted consensus material for authenticated gossip admission.
    pub fn with_gossip_validation(
        mut self,
        context: ConsensusContext,
        committee: Committee,
    ) -> Self {
        self.gossip_validation = Some(GossipValidationConfig {
            context,
            committee,
            allow_dev_envelopes: false,
        });
        self
    }

    /// Enable or disable development-only signed envelopes for the trusted
    /// gossip admission gate.  Runtime mode is the caller's authority; this
    /// flag is never inferred from an inbound transaction.
    pub fn with_dev_envelopes(mut self, allow: bool) -> Self {
        if let Some(validation) = self.gossip_validation.as_mut() {
            validation.allow_dev_envelopes = allow;
        }
        self
    }

    /// Create handshake config from network config
    pub fn handshake_config(&self) -> Result<HandshakeConfig> {
        if !self.require_authenticated_peers {
            return Ok(HandshakeConfig::unauthenticated(self.node_id));
        }

        let bls_sk = self.bls_secret_key.as_ref().ok_or_else(|| {
            anyhow!("authenticated peers are required but no local BLS secret key is configured")
        })?;

        if self.validator_pubkeys.is_empty() {
            return Err(anyhow!(
                "authenticated peers are required but the validator BLS committee is empty"
            ));
        }

        let configured_local_pubkey = self.validator_pubkeys.get(&self.node_id).ok_or_else(|| {
            anyhow!(
                "authenticated peers are required but local node {} is not in the validator BLS committee",
                crate::types::hash_short(&self.node_id)
            )
        })?;

        if bls_sk.public_key().to_bytes() != configured_local_pubkey.to_bytes() {
            return Err(anyhow!(
                "authenticated peers are required but the local BLS secret key does not match the configured committee key for node {}",
                crate::types::hash_short(&self.node_id)
            ));
        }

        for (peer_id, _) in &self.peers {
            if !self.validator_pubkeys.contains_key(peer_id) {
                return Err(anyhow!(
                    "authenticated peers are required but validator {} has no BLS public key",
                    crate::types::hash_short(peer_id)
                ));
            }
        }

        Ok(HandshakeConfig::authenticated(
            self.node_id,
            bls_sk.clone(),
            self.validator_pubkeys.clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ConsensusConfig;

    fn authenticated_config() -> NetworkConfig {
        let node_id = [1u8; 32];
        let peer_id = [2u8; 32];
        let local_sk = BlsSecretKey::from_seed(&[7u8; 32]);
        let peer_sk = BlsSecretKey::from_seed(&[8u8; 32]);
        let consensus = ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id,
            validators: vec![node_id, peer_id],
            voting_powers: vec![1, 1],
            view_timeout_ms: 1000,
            bls_pubkeys: vec![
                local_sk.public_key().to_bytes().to_vec(),
                peer_sk.public_key().to_bytes().to_vec(),
            ],
            bls_secret_key: Some(local_sk.to_bytes()),
        };

        NetworkConfig {
            node_id,
            listen_addr: "127.0.0.1:0".to_string(),
            peers: vec![(peer_id, "127.0.0.1:1".to_string())],
            require_authenticated_peers: true,
            bls_secret_key: Some(local_sk),
            validator_pubkeys: HashMap::from([
                ([1u8; 32], BlsSecretKey::from_seed(&[7u8; 32]).public_key()),
                (peer_id, peer_sk.public_key()),
            ]),
            gossip_validation: Some(GossipValidationConfig {
                context: consensus.context().unwrap(),
                committee: consensus.committee().unwrap(),
                allow_dev_envelopes: false,
            }),
        }
    }

    #[test]
    fn authenticated_handshake_requires_local_bls_key() {
        let mut config = authenticated_config();
        config.bls_secret_key = None;

        let error = config
            .handshake_config()
            .err()
            .expect("handshake must fail");
        assert!(error.to_string().contains("local BLS secret key"));
    }

    #[test]
    fn authenticated_handshake_requires_usable_committee_keys() {
        let mut config = authenticated_config();
        config.validator_pubkeys.clear();

        let error = config
            .handshake_config()
            .err()
            .expect("handshake must fail");
        assert!(error.to_string().contains("committee is empty"));
    }

    #[test]
    fn authenticated_handshake_requires_local_membership() {
        let mut config = authenticated_config();
        config.validator_pubkeys.remove(&config.node_id);

        let error = config
            .handshake_config()
            .err()
            .expect("handshake must fail");
        assert!(error.to_string().contains("local node"));
        assert!(error
            .to_string()
            .contains("not in the validator BLS committee"));
    }

    #[test]
    fn authenticated_handshake_requires_local_key_match() {
        let mut config = authenticated_config();
        config.validator_pubkeys.insert(
            config.node_id,
            BlsSecretKey::from_seed(&[9u8; 32]).public_key(),
        );

        let error = config
            .handshake_config()
            .err()
            .expect("handshake must fail");
        assert!(error
            .to_string()
            .contains("local BLS secret key does not match"));
    }

    #[test]
    fn authenticated_handshake_requires_key_for_each_peer() {
        let mut config = authenticated_config();
        config.validator_pubkeys.remove(&[2u8; 32]);

        let error = config
            .handshake_config()
            .err()
            .expect("handshake must fail");
        assert!(error.to_string().contains("has no BLS public key"));
    }

    #[test]
    fn authenticated_handshake_accepts_complete_config() {
        let config = authenticated_config();

        let handshake = config
            .handshake_config()
            .expect("complete authenticated config must succeed");
        assert!(handshake.require_auth);
        assert!(handshake.bls_sk.is_some());
        assert_eq!(handshake.validator_pubkeys.len(), 2);
    }

    #[test]
    fn dev_handshake_allows_missing_authentication_material() {
        let config = NetworkConfig::local_three_nodes(0);

        let handshake = config.handshake_config().unwrap();
        assert!(!handshake.require_auth);
        assert!(handshake.bls_sk.is_none());
    }
}
