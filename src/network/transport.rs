//! TCP Transport Implementation
//!
//! TCP-based networking for consensus messages with BLS-authenticated handshakes.
//! Uses length-prefixed bincode for efficient message framing.
//!
//! ## Security (CRITICAL-6)
//!
//! This module uses authenticated handshakes via BLS signatures to prevent
//! impersonation attacks. When `require_authenticated_peers` is true in
//! NetworkConfig, connections without valid BLS signatures are rejected.
//!
//! See `HandshakeConfig::authenticated()` for setup.
//!
//! ## Serialization
//!
//! Consensus messages use bincode for performance (2-5x faster, 30-50% smaller
//! than JSON). A magic byte prefix identifies the format for forward compatibility.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Read timeout for TCP connections (30 seconds)
/// Prevents resource exhaustion from slow/stalled connections
const TCP_READ_TIMEOUT: Duration = Duration::from_secs(30);
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);
const STABLE_CONNECTION_THRESHOLD: Duration = Duration::from_secs(1);

use super::gossip::{
    compute_message_id, select_gossip_peers, validate_gossip_envelope, GossipConfig, GossipMessage,
    GossipState,
};
use super::handshake::{handshake_inbound, handshake_outbound, HandshakeConfig};
use super::{GossipValidationConfig, Network, NetworkConfig, UserTransactionPublisher};
use crate::app::{SignedEnvelope, Transaction};
use crate::consensus::{verify_certificate, verify_equivocation_proof, verify_vote};
use crate::crypto::bls::{BlsPublicKey, BlsSignature};
use crate::types::{
    hash_short, Block, Certificate, Committee, ConsensusContext, Message, NewView, NodeId, Prepare,
    Propose, View, ViewChange, ViewChangeCertificate, Vote, MAX_SYNC_RESPONSE_BYTES,
};

/// Maximum length-prefixed wire message.  This is shared with block sync so a
/// valid maximum-size block plus its protocol envelope can cross TCP without
/// allowing an unbounded allocation on the receive path.
const MAX_NETWORK_MESSAGE_BYTES: usize = MAX_SYNC_RESPONSE_BYTES;

fn message_size_allowed(len: usize) -> bool {
    len <= MAX_NETWORK_MESSAGE_BYTES
}

/// TCP-based network implementation with BLS authentication
pub struct TcpNetwork {
    /// Our node ID
    node_id: NodeId,

    /// Connected peers: NodeId -> outbound sender and connection identity
    peers: SharedPeers,

    /// Channel for received messages
    incoming_rx: mpsc::Receiver<(NodeId, Message)>,

    /// Sender for incoming messages (used by listener tasks)
    incoming_tx: mpsc::Sender<(NodeId, Message)>,

    /// Network config
    config: NetworkConfig,

    /// Handshake config for BLS authentication (CRITICAL-6)
    handshake_config: HandshakeConfig,

    /// Gossip state for epidemic message propagation
    gossip_state: Arc<GossipState>,

    /// Trusted, secret-free material for pre-relay message admission.
    gossip_validation: Option<GossipValidationConfig>,
}

/// Cloneable outbound-only view of a [`TcpNetwork`].
///
/// The API uses this handle to publish an already admitted signed envelope.
/// It shares only peer senders and validation state; the consensus runner
/// remains the sole owner of the transport receive channel.
#[derive(Clone)]
pub struct TransactionBroadcaster {
    node_id: NodeId,
    peers: SharedPeers,
    gossip_state: Arc<GossipState>,
    gossip_validation: Option<GossipValidationConfig>,
}

type SharedPeers = Arc<RwLock<HashMap<NodeId, PeerConnection>>>;

struct PeerConnection {
    sender: mpsc::Sender<Vec<u8>>,
    token: Arc<()>,
}

impl PeerConnection {
    fn new(sender: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            sender,
            token: Arc::new(()),
        }
    }
}

impl TcpNetwork {
    fn validate_peer_config(config: &NetworkConfig) -> Result<()> {
        let mut peer_ids = HashSet::with_capacity(config.peers.len());
        for (peer_id, _) in &config.peers {
            if *peer_id == config.node_id {
                return Err(anyhow!("network peer list contains the local node"));
            }
            if !peer_ids.insert(*peer_id) {
                return Err(anyhow!(
                    "network peer list contains duplicate node {}",
                    hash_short(peer_id)
                ));
            }
        }
        Ok(())
    }

    /// Create and start a new TCP network with BLS authentication support
    pub async fn new(config: NetworkConfig) -> Result<Self> {
        Self::validate_peer_config(&config)?;
        let (incoming_tx, incoming_rx) = mpsc::channel(1000);
        let peers = Arc::new(RwLock::new(HashMap::new()));

        // Create handshake config from network config (CRITICAL-6)
        let handshake_config = config
            .handshake_config()
            .context("Invalid authenticated peer configuration")?;

        // Create gossip config from environment.  A validator without a
        // trusted context/committee cannot safely relay authenticated
        // consensus messages, so development configs fall back to direct
        // broadcast.  Authenticated live configs fail closed instead of
        // silently enabling an unauthenticated relay path.
        let mut gossip_config = GossipConfig::from_env();
        if let Some(validation) = &config.gossip_validation {
            validation
                .committee
                .validate_context(validation.context)
                .map_err(|error| anyhow!("invalid gossip validation context: {error}"))?;
            if gossip_config.enabled && gossip_config.ttl == 0 {
                return Err(anyhow!("gossip cannot be enabled with a zero TTL"));
            }
        } else if gossip_config.enabled && config.require_authenticated_peers {
            return Err(anyhow!(
                "authenticated gossip requires a trusted consensus context and committee"
            ));
        } else {
            gossip_config.enabled = false;
        }
        let gossip_state = Arc::new(
            GossipState::try_new(gossip_config.clone())
                .map_err(|error| anyhow!("invalid gossip configuration: {error}"))?,
        );
        let gossip_validation = config.gossip_validation.clone();

        if handshake_config.require_auth {
            info!(
                node = %hash_short(&config.node_id),
                validators = handshake_config.validator_pubkeys.len(),
                "Network authentication ENABLED"
            );
        } else {
            warn!(
                node = %hash_short(&config.node_id),
                "Network authentication DISABLED (dev mode only!)"
            );
        }

        if gossip_state.is_enabled() {
            info!(
                node = %hash_short(&config.node_id),
                fanout = gossip_config.fanout,
                ttl = gossip_config.ttl,
                "Gossip protocol ENABLED"
            );
        } else {
            info!(
                node = %hash_short(&config.node_id),
                "Gossip protocol DISABLED (direct broadcast)"
            );
        }

        let network = Self {
            node_id: config.node_id,
            peers,
            incoming_rx,
            incoming_tx,
            config,
            handshake_config,
            gossip_state,
            gossip_validation,
        };

        Ok(network)
    }

    /// Return an outbound-only handle that can be shared with API ingress.
    pub fn transaction_broadcaster(&self) -> TransactionBroadcaster {
        TransactionBroadcaster {
            node_id: self.node_id,
            peers: self.peers.clone(),
            gossip_state: self.gossip_state.clone(),
            gossip_validation: self.gossip_validation.clone(),
        }
    }

    /// Start listening and connect to peers
    pub async fn start(&self) -> Result<()> {
        // Start listener
        let listener = TcpListener::bind(&self.config.listen_addr)
            .await
            .context("Failed to bind listener")?;

        info!(
            addr = %self.config.listen_addr,
            node = %hash_short(&self.node_id),
            authenticated = self.handshake_config.require_auth,
            "Network listening"
        );

        // Spawn listener task
        let incoming_tx = self.incoming_tx.clone();
        let peers = self.peers.clone();
        let handshake_config = self.handshake_config.clone();
        let gossip_state = self.gossip_state.clone();
        let gossip_validation = self.gossip_validation.clone();

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        debug!(%addr, "Accepted connection");
                        let tx = incoming_tx.clone();
                        let peers = peers.clone();
                        let hs_config = handshake_config.clone();
                        let gs = gossip_state.clone();
                        let validation = gossip_validation.clone();
                        tokio::spawn(handle_connection(
                            stream, tx, peers, hs_config, gs, validation,
                        ));
                    }
                    Err(e) => {
                        error!(error = %e, "Accept failed");
                    }
                }
            }
        });

        // Connect to peers with LOWER IDs only (to avoid duplicate connections)
        // Nodes with higher IDs will connect to us
        let our_node_id = self.node_id;
        for (peer_id, addr) in &self.config.peers {
            // Only connect if peer has lower ID than us
            if *peer_id >= our_node_id {
                debug!(
                    peer = %hash_short(peer_id),
                    "Skipping outbound connection (peer will connect to us)"
                );
                continue;
            }

            let peers = self.peers.clone();
            let incoming_tx = self.incoming_tx.clone();
            let peer_id = *peer_id;
            let addr = addr.clone();
            let handshake_config = self.handshake_config.clone();
            let gossip_state = self.gossip_state.clone();
            let gossip_validation = self.gossip_validation.clone();

            tokio::spawn(async move {
                connect_to_peer(
                    peer_id,
                    addr,
                    peers,
                    incoming_tx,
                    handshake_config,
                    gossip_state,
                    gossip_validation,
                )
                .await;
            });
        }

        Ok(())
    }

    /// Send a message to a specific peer (internal impl)
    async fn send_to_internal(&self, to: NodeId, msg: &Message) -> Result<()> {
        if let Some(validation) = self.gossip_validation.as_ref() {
            validate_authenticated_message(msg, self.node_id, validation)
                .context("local message failed network admission")?;
        }
        let data = serialize_message(msg)?;

        let sender = {
            let peers = self.peers.read().await;
            peers.get(&to).map(|connection| connection.sender.clone())
        };
        match sender {
            Some(sender) => sender
                .send(data)
                .await
                .map_err(|_| anyhow!("Failed to send to peer")),
            None => Err(anyhow!("Peer {} not connected", hash_short(&to))),
        }
    }

    /// Broadcast a message to all connected peers (internal impl)
    ///
    /// If gossip is enabled, wraps the message and selects fanout peers.
    /// Otherwise, broadcasts directly to all peers.
    async fn broadcast_internal(&self, msg: &Message) -> Result<()> {
        broadcast_message(
            self.node_id,
            &self.peers,
            &self.gossip_state,
            self.gossip_validation.as_ref(),
            msg,
        )
        .await
    }

    /// Get gossip statistics for monitoring
    pub fn gossip_stats(&self) -> super::GossipStats {
        self.gossip_state.stats()
    }

    /// Return whether authenticated gossip propagation is enabled.
    ///
    /// Equivocation evidence is deliberately persisted by the consensus
    /// runner before it is rebroadcast.  The runner uses this bit to avoid
    /// turning development/direct-broadcast mode into an endless rebroadcast
    /// loop (direct broadcast already reaches every connected peer).
    pub fn gossip_enabled(&self) -> bool {
        self.gossip_state.is_enabled()
    }
}

#[async_trait]
impl UserTransactionPublisher for TcpNetwork {
    async fn publish_user_transaction(&self, envelope: SignedEnvelope) -> Result<()> {
        self.broadcast_internal(&Message::UserTransaction(envelope))
            .await
    }

    async fn rebroadcast_user_transaction(&self, envelope: SignedEnvelope) -> Result<()> {
        rebroadcast_user_transaction_direct(
            self.node_id,
            &self.peers,
            self.gossip_validation.as_ref(),
            envelope,
        )
        .await
    }
}

#[async_trait]
impl UserTransactionPublisher for TransactionBroadcaster {
    async fn publish_user_transaction(&self, envelope: SignedEnvelope) -> Result<()> {
        broadcast_message(
            self.node_id,
            &self.peers,
            &self.gossip_state,
            self.gossip_validation.as_ref(),
            &Message::UserTransaction(envelope),
        )
        .await
    }

    async fn rebroadcast_user_transaction(&self, envelope: SignedEnvelope) -> Result<()> {
        rebroadcast_user_transaction_direct(
            self.node_id,
            &self.peers,
            self.gossip_validation.as_ref(),
            envelope,
        )
        .await
    }
}

#[async_trait]
impl Network for TcpNetwork {
    async fn broadcast_propose(&self, propose: Propose) -> Result<()> {
        debug!(
            view = propose.block.view,
            height = propose.block.height,
            "Broadcasting propose"
        );
        self.broadcast_internal(&Message::Propose(propose)).await
    }

    async fn send_vote(&self, to: NodeId, vote: Vote) -> Result<()> {
        debug!(
            view = vote.view,
            to = %hash_short(&to),
            "Sending vote"
        );
        self.send_to_internal(to, &Message::Vote(vote)).await
    }

    async fn broadcast_prepare(&self, prepare: Prepare) -> Result<()> {
        debug!(view = prepare.view, "Broadcasting prepare");
        self.broadcast_internal(&Message::Prepare(prepare)).await
    }

    async fn broadcast_view_change(&self, vc: ViewChange) -> Result<()> {
        debug!(
            from_view = vc.from_view,
            to_view = vc.to_view,
            "Broadcasting view change"
        );
        self.broadcast_internal(&Message::ViewChange(vc)).await
    }

    async fn broadcast_new_view(&self, nv: NewView) -> Result<()> {
        debug!(view = nv.view, "Broadcasting new view");
        self.broadcast_internal(&Message::NewView(nv)).await
    }

    async fn broadcast(&self, msg: &Message) -> Result<()> {
        self.broadcast_internal(msg).await
    }

    async fn send_to(&self, to: NodeId, msg: &Message) -> Result<()> {
        self.send_to_internal(to, msg).await
    }

    async fn recv(&self) -> Result<(NodeId, Message)> {
        // This is a bit awkward because we need &mut self for recv
        // In a real impl, we'd use a different pattern
        Err(anyhow!("Use recv_mut instead"))
    }

    fn node_id(&self) -> NodeId {
        self.node_id
    }
}

impl TcpNetwork {
    /// Receive next message (requires mutable access)
    pub async fn recv_msg(&mut self) -> Result<(NodeId, Message)> {
        self.incoming_rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("Channel closed"))
    }

    /// Return whether this transport requires authenticated peer handshakes.
    pub fn requires_authenticated_peers(&self) -> bool {
        self.handshake_config.require_auth
    }

    /// Wait until every configured peer has completed its handshake.
    pub async fn wait_for_peers(&self, wait_timeout: Duration) -> Result<()> {
        let expected = self.config.peers.len();
        if expected == 0 {
            return Ok(());
        }

        let deadline = tokio::time::Instant::now() + wait_timeout;
        loop {
            let actual = {
                let peers = self.peers.read().await;
                self.config
                    .peers
                    .iter()
                    .filter(|(peer_id, _)| {
                        peers
                            .get(peer_id)
                            .is_some_and(|connection| !connection.sender.is_closed())
                    })
                    .count()
            };
            if actual >= expected {
                return Ok(());
            }

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!(
                    "timed out waiting for peers: expected {}, connected {}",
                    expected,
                    actual
                ));
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
        }
    }
}

async fn remove_peer_if_current(peers: &SharedPeers, peer_id: NodeId, token: &Arc<()>) {
    let mut peers = peers.write().await;
    let is_current = peers
        .get(&peer_id)
        .is_some_and(|connection| Arc::ptr_eq(&connection.token, token));
    if is_current {
        peers.remove(&peer_id);
    }
}

/// Validate and publish one message using only the cloneable transport state.
/// Every peer sender is copied out of the lock before any await.
async fn broadcast_message(
    node_id: NodeId,
    peers: &SharedPeers,
    gossip_state: &Arc<GossipState>,
    validation: Option<&GossipValidationConfig>,
    msg: &Message,
) -> Result<()> {
    if let Some(validation) = validation {
        validate_authenticated_message(msg, node_id, validation)
            .context("local message failed network admission")?;
    } else if let Message::UserTransaction(envelope) = msg {
        validate_user_envelope_structure(envelope)
            .context("local user transaction failed structural admission")?;
    }

    if gossip_state.is_enabled()
        && !matches!(
            msg,
            Message::SyncRequest(_)
                | Message::SyncResponse(_)
                | Message::SnapshotRequest(_)
                | Message::SnapshotResponse(_)
        )
    {
        broadcast_gossip_message(node_id, peers, gossip_state, msg).await
    } else {
        broadcast_direct_message(node_id, peers, gossip_state, msg).await
    }
}

/// Retry one authenticated user transaction directly to every connected
/// validator. The retry path deliberately does not wrap the message in the
/// probabilistic gossip envelope: it is a bounded all-peer recovery pass for
/// a transaction that is still present in the local canonical mempool.
///
/// Peer senders are copied out of the shared map before any send. `try_send`
/// makes a full writer queue a per-peer failure instead of blocking the
/// rebroadcast worker or holding a transport lock across I/O.
async fn rebroadcast_user_transaction_direct(
    node_id: NodeId,
    peers: &SharedPeers,
    validation: Option<&GossipValidationConfig>,
    envelope: SignedEnvelope,
) -> Result<()> {
    let message = Message::UserTransaction(envelope);
    if let Some(validation) = validation {
        validate_authenticated_message(&message, node_id, validation)
            .context("local user transaction failed network admission")?;
    } else if let Message::UserTransaction(envelope) = &message {
        validate_user_envelope_structure(envelope)
            .context("local user transaction failed structural admission")?;
    }
    let data = serialize_message(&message)?;
    let senders = {
        let peers = peers.read().await;
        peers
            .iter()
            .map(|(peer_id, connection)| (*peer_id, connection.sender.clone()))
            .collect::<Vec<_>>()
    };

    let total_peers = senders.len();
    let mut failed = 0usize;
    for (peer_id, sender) in senders {
        if let Err(error) = sender.try_send(data.clone()) {
            failed += 1;
            debug!(
                peer = %hash_short(&peer_id),
                error = %error,
                "Failed to enqueue user transaction rebroadcast"
            );
        }
    }
    if failed > 0 {
        debug!(
            failed,
            total_peers, "User transaction rebroadcast had unavailable peers"
        );
        Err(anyhow!(
            "user transaction rebroadcast could not enqueue {} peer send(s)",
            failed
        ))
    } else {
        Ok(())
    }
}

fn validate_user_envelope_structure(envelope: &SignedEnvelope) -> Result<()> {
    if matches!(&envelope.action, Transaction::SubmitEvidence { .. }) {
        return Err(anyhow!(
            "privileged system action cannot be carried as a user transaction"
        ));
    }
    envelope
        .validate_structure()
        .map_err(|error| anyhow!("invalid signed envelope: {error}"))
}

async fn broadcast_direct_message(
    _node_id: NodeId,
    peers: &SharedPeers,
    gossip_state: &Arc<GossipState>,
    msg: &Message,
) -> Result<()> {
    let data = serialize_message(msg)?;
    // Direct mode has no outer GossipMessage, so retain the same stable
    // payload identity for user transactions and suppress duplicate delivery.
    if matches!(msg, Message::UserTransaction(_)) {
        gossip_state.mark_seen(&compute_message_id(msg));
    }
    let senders = {
        let peers = peers.read().await;
        peers
            .iter()
            .map(|(peer_id, connection)| (*peer_id, connection.sender.clone()))
            .collect::<Vec<_>>()
    };

    for (peer_id, sender) in senders {
        if let Err(e) = sender.send(data.clone()).await {
            warn!(peer = %hash_short(&peer_id), error = %e, "Failed to broadcast to peer");
        }
    }

    Ok(())
}

async fn broadcast_gossip_message(
    node_id: NodeId,
    peers: &SharedPeers,
    gossip_state: &Arc<GossipState>,
    msg: &Message,
) -> Result<()> {
    // Wrap message in gossip envelope.
    let gossip_msg = gossip_state.wrap_message(msg.clone(), node_id);

    validate_gossip_envelope(&gossip_msg, gossip_state.initial_ttl())
        .map_err(|error| anyhow!("local gossip envelope is invalid: {error}"))?;

    // Mark as seen (we originated this message).
    gossip_state.mark_seen(&gossip_msg.msg_id);

    let peer_ids = {
        let peers = peers.read().await;
        peers.keys().copied().collect::<Vec<_>>()
    };
    let selected = select_gossip_peers(&peer_ids, &gossip_msg.msg_id, gossip_state.fanout(), None);

    debug!(
        fanout = selected.len(),
        total_peers = peer_ids.len(),
        msg_type = ?std::mem::discriminant(msg),
        "Gossip broadcasting"
    );

    let wrapped_msg = Message::Gossip(Box::new(gossip_msg));
    let data = serialize_message(&wrapped_msg)?;
    let senders = {
        let peers = peers.read().await;
        selected
            .iter()
            .filter_map(|peer_id| {
                peers
                    .get(peer_id)
                    .map(|connection| (*peer_id, connection.sender.clone()))
            })
            .collect::<Vec<_>>()
    };

    for (peer_id, sender) in senders {
        if let Err(e) = sender.send(data.clone()).await {
            warn!(peer = %hash_short(&peer_id), error = %e, "Failed to gossip broadcast");
        }
    }

    Ok(())
}

fn validate_inbound_connection(
    local_node_id: NodeId,
    peer_id: NodeId,
    peers: &HashMap<NodeId, PeerConnection>,
) -> Result<()> {
    if peer_id <= local_node_id {
        return Err(anyhow!(
            "inbound peer {} violates lower-ID dial rule",
            hash_short(&peer_id)
        ));
    }
    if let Some(connection) = peers.get(&peer_id) {
        if !connection.sender.is_closed() {
            return Err(anyhow!(
                "inbound peer {} already has an active connection",
                hash_short(&peer_id)
            ));
        }
    }
    Ok(())
}

fn connection_retry_delays(current: Duration, connected_for: Duration) -> (Duration, Duration) {
    let wait = if connected_for >= STABLE_CONNECTION_THRESHOLD {
        INITIAL_RETRY_DELAY
    } else {
        current
    };
    let next = (wait * 2).min(MAX_RETRY_DELAY);
    (wait, next)
}

/// Handle an incoming connection with BLS authentication (CRITICAL-6)
async fn handle_connection(
    stream: TcpStream,
    incoming_tx: mpsc::Sender<(NodeId, Message)>,
    peers: SharedPeers,
    handshake_config: HandshakeConfig,
    gossip_state: Arc<GossipState>,
    gossip_validation: Option<GossipValidationConfig>,
) {
    let (mut read_half, mut write_half) = stream.into_split();

    // Perform authenticated handshake (CRITICAL-6)
    let handshake_result =
        match handshake_inbound(&mut read_half, &mut write_half, &handshake_config).await {
            Ok(result) => result,
            Err(e) => {
                warn!(error = %e, "Inbound handshake failed - rejecting connection");
                return;
            }
        };

    let peer_id = handshake_result.peer_id;

    if handshake_config.require_auth && !handshake_result.authenticated {
        warn!(
            peer = %hash_short(&peer_id),
            "Rejecting unauthenticated peer (authentication required)"
        );
        return;
    }

    // Create channel for outgoing messages to this peer
    let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(100);
    let connection = PeerConnection::new(write_tx);
    let connection_token = connection.token.clone();

    // The lower-ID side owns outbound dialing. Reject duplicate inbound
    // sockets atomically, while allowing replacement of a closed sender.
    {
        let mut peers = peers.write().await;
        if let Err(error) = validate_inbound_connection(handshake_config.node_id, peer_id, &peers) {
            warn!(peer = %hash_short(&peer_id), error = %error, "Rejecting inbound connection");
            return;
        }
        peers.insert(peer_id, connection);
    }

    info!(
        peer = %hash_short(&peer_id),
        authenticated = handshake_result.authenticated,
        "Peer connected"
    );

    // Spawn writer task
    let peer_id_for_writer = peer_id;
    tokio::spawn(async move {
        while let Some(data) = write_rx.recv().await {
            // Length-prefix the message
            let len = data.len() as u32;
            if write_half.write_all(&len.to_be_bytes()).await.is_err() {
                break;
            }
            if write_half.write_all(&data).await.is_err() {
                break;
            }
        }
        debug!(peer = %hash_short(&peer_id_for_writer), "Writer task ended");
    });

    // Read loop
    loop {
        // Read length prefix with timeout
        let mut len_buf = [0u8; 4];
        match timeout(TCP_READ_TIMEOUT, read_half.read_exact(&mut len_buf)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break, // Read error
            Err(_) => {
                debug!(peer = %hash_short(&peer_id), "Read timeout, closing connection");
                break;
            }
        }
        let len = u32::from_be_bytes(len_buf) as usize;

        // Sanity check
        if !message_size_allowed(len) {
            warn!(len, "Message too large");
            break;
        }

        // Read message with timeout
        let mut msg_buf = vec![0u8; len];
        match timeout(TCP_READ_TIMEOUT, read_half.read_exact(&mut msg_buf)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_) => {
                debug!(peer = %hash_short(&peer_id), "Read timeout on message body");
                break;
            }
        }

        // Deserialize
        match deserialize_message(&msg_buf) {
            Ok(msg) => match admit_message(msg, peer_id, &gossip_state, gossip_validation.as_ref())
            {
                Ok(Admission::Duplicate) => {
                    debug!(peer = %hash_short(&peer_id), "Duplicate authenticated gossip dropped");
                }
                Ok(Admission::Deliver {
                    origin,
                    message,
                    relay,
                }) => {
                    if incoming_tx.send((origin, message)).await.is_err() {
                        break;
                    }
                    if let Some(gossip_msg) = relay {
                        relay_gossip_message(&gossip_msg, &peer_id, &peers, &gossip_state).await;
                    }
                }
                Err(error) => {
                    warn!(peer = %hash_short(&peer_id), error = %error, "Rejected inbound network message");
                }
            },
            Err(e) => {
                warn!(error = %e, "Failed to deserialize message");
            }
        }
    }

    // Clean up
    info!(peer = %hash_short(&peer_id), "Peer disconnected");
    remove_peer_if_current(&peers, peer_id, &connection_token).await;
}

/// Relay a gossip message to fanout peers
async fn relay_gossip_message(
    gossip_msg: &GossipMessage,
    from: &NodeId,
    peers: &SharedPeers,
    gossip_state: &Arc<GossipState>,
) {
    // Decrement TTL for relay
    let relayed = match gossip_msg.relay() {
        Some(r) => r,
        None => return, // TTL exhausted
    };

    // Get connected peer IDs
    let peers_guard = peers.read().await;
    let peer_ids: Vec<NodeId> = peers_guard.keys().copied().collect();
    drop(peers_guard);

    // Select fanout peers, excluding sender
    let selected = select_gossip_peers(
        &peer_ids,
        &relayed.msg_id,
        gossip_state.fanout(),
        Some(from),
    );

    if selected.is_empty() {
        return;
    }

    debug!(
        fanout = selected.len(),
        ttl = relayed.ttl,
        "Gossip relaying"
    );

    // Serialize relay message
    let wrapped_msg = Message::Gossip(Box::new(relayed));
    let data = match serialize_message(&wrapped_msg) {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "Failed to serialize gossip relay");
            return;
        }
    };

    // Send to selected peers after releasing the peer map lock.
    let senders = {
        let peers_guard = peers.read().await;
        selected
            .iter()
            .filter_map(|peer_id| {
                peers_guard
                    .get(peer_id)
                    .map(|connection| (*peer_id, connection.sender.clone()))
            })
            .collect::<Vec<_>>()
    };
    for (peer_id, sender) in senders {
        if let Err(e) = sender.send(data.clone()).await {
            debug!(peer = %hash_short(&peer_id), error = %e, "Failed to relay gossip");
        }
    }
}

/// Connect to a peer with retry and BLS authentication (CRITICAL-6)
async fn connect_to_peer(
    peer_id: NodeId,
    addr: String,
    peers: SharedPeers,
    incoming_tx: mpsc::Sender<(NodeId, Message)>,
    handshake_config: HandshakeConfig,
    gossip_state: Arc<GossipState>,
    gossip_validation: Option<GossipValidationConfig>,
) {
    let mut retry_delay = INITIAL_RETRY_DELAY;

    loop {
        match TcpStream::connect(&addr).await {
            Ok(stream) => {
                debug!(peer = %hash_short(&peer_id), %addr, "TCP connection established, starting handshake");

                // Perform authenticated handshake (CRITICAL-6)
                let (mut read_half, mut write_half) = stream.into_split();

                let handshake_result = match handshake_outbound(
                    &mut read_half,
                    &mut write_half,
                    &handshake_config,
                    &peer_id,
                )
                .await
                {
                    Ok(result) => result,
                    Err(e) => {
                        warn!(
                            peer = %hash_short(&peer_id),
                            %addr,
                            error = %e,
                            "Outbound handshake failed, retrying"
                        );
                        tokio::time::sleep(retry_delay).await;
                        retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
                        continue;
                    }
                };

                // Reject unauthenticated peers if authentication is required
                if handshake_config.require_auth && !handshake_result.authenticated {
                    warn!(
                        peer = %hash_short(&peer_id),
                        "Rejecting unauthenticated peer (authentication required)"
                    );
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
                    continue;
                }

                info!(
                    peer = %hash_short(&peer_id),
                    %addr,
                    authenticated = handshake_result.authenticated,
                    "Connected to peer"
                );

                // Create channel for outgoing messages
                let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(100);
                let connection = PeerConnection::new(write_tx);
                let connection_token = connection.token.clone();

                // Store in peers map
                {
                    let mut peers_guard = peers.write().await;
                    peers_guard.insert(peer_id, connection);
                }

                // Spawn writer task. The supervisor below observes this
                // handle so a write failure also tears down the reader.
                let peer_id_for_writer = peer_id;
                let mut writer_handle = tokio::spawn(async move {
                    while let Some(data) = write_rx.recv().await {
                        let len = data.len() as u32;
                        write_half.write_all(&len.to_be_bytes()).await?;
                        write_half.write_all(&data).await?;
                    }
                    debug!(peer = %hash_short(&peer_id_for_writer), "Outbound writer ended");
                    Ok::<(), anyhow::Error>(())
                });

                // Spawn reader task
                let incoming_tx = incoming_tx.clone();
                let peers_for_reader = peers.clone();
                let gossip_state = gossip_state.clone();
                let gossip_validation = gossip_validation.clone();
                let reader_token = connection_token.clone();
                let mut reader_handle = tokio::spawn(async move {
                    loop {
                        // Read length prefix with timeout
                        let mut len_buf = [0u8; 4];
                        match timeout(TCP_READ_TIMEOUT, read_half.read_exact(&mut len_buf)).await {
                            Ok(Ok(_)) => {}
                            Ok(Err(_)) => break,
                            Err(_) => {
                                debug!(peer = %hash_short(&peer_id), "Read timeout");
                                break;
                            }
                        }
                        let len = u32::from_be_bytes(len_buf) as usize;

                        if !message_size_allowed(len) {
                            break;
                        }

                        // Read message with timeout
                        let mut msg_buf = vec![0u8; len];
                        match timeout(TCP_READ_TIMEOUT, read_half.read_exact(&mut msg_buf)).await {
                            Ok(Ok(_)) => {}
                            Ok(Err(_)) => break,
                            Err(_) => break,
                        }

                        match deserialize_message(&msg_buf) {
                            Ok(msg) => match admit_message(
                                msg,
                                peer_id,
                                &gossip_state,
                                gossip_validation.as_ref(),
                            ) {
                                Ok(Admission::Duplicate) => {
                                    debug!(peer = %hash_short(&peer_id), "Duplicate authenticated gossip dropped");
                                }
                                Ok(Admission::Deliver {
                                    origin,
                                    message,
                                    relay,
                                }) => {
                                    if incoming_tx.send((origin, message)).await.is_err() {
                                        break;
                                    }
                                    if let Some(gossip_msg) = relay {
                                        relay_gossip_message(
                                            &gossip_msg,
                                            &peer_id,
                                            &peers_for_reader,
                                            &gossip_state,
                                        )
                                        .await;
                                    }
                                }
                                Err(error) => {
                                    warn!(peer = %hash_short(&peer_id), error = %error, "Rejected outbound-reader network message");
                                }
                            },
                            Err(e) => {
                                warn!(error = %e, "Deserialize failed");
                            }
                        }
                    }
                    remove_peer_if_current(&peers_for_reader, peer_id, &reader_token).await;
                });

                let connected_at = Instant::now();
                tokio::select! {
                    reader_result = &mut reader_handle => {
                        if let Err(error) = reader_result {
                            warn!(peer = %hash_short(&peer_id), error = %error, "Outbound reader task ended unexpectedly");
                        }
                        writer_handle.abort();
                        let _ = writer_handle.await;
                    }
                    writer_result = &mut writer_handle => {
                        match writer_result {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                warn!(peer = %hash_short(&peer_id), error = %error, "Outbound writer failed");
                            }
                            Err(error) => {
                                warn!(peer = %hash_short(&peer_id), error = %error, "Outbound writer task ended unexpectedly");
                            }
                        }
                        reader_handle.abort();
                        let _ = reader_handle.await;
                    }
                }
                remove_peer_if_current(&peers, peer_id, &connection_token).await;
                let (retry_wait, next_retry_delay) =
                    connection_retry_delays(retry_delay, connected_at.elapsed());
                tokio::time::sleep(retry_wait).await;
                retry_delay = next_retry_delay;
            }
            Err(e) => {
                debug!(peer = %hash_short(&peer_id), %addr, error = %e, "Connect failed, retrying");
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
            }
        }
    }
}

/// Result of the single transport admission gate shared by inbound and
/// outbound TCP readers.
enum Admission {
    Duplicate,
    Deliver {
        origin: NodeId,
        message: Message,
        relay: Option<GossipMessage>,
    },
}

fn parse_committee_key(committee: &Committee, node_id: &NodeId) -> Result<BlsPublicKey> {
    let bytes = committee
        .bls_pubkey(node_id)
        .ok_or_else(|| anyhow!("committee member has no configured BLS public key"))?;
    if bytes.len() != 48 {
        return Err(anyhow!("configured BLS public key has invalid length"));
    }
    let mut array = [0u8; 48];
    array.copy_from_slice(bytes);
    BlsPublicKey::from_bytes(&array).map_err(|_| anyhow!("configured BLS public key is invalid"))
}

fn verify_member_signature(
    committee: &Committee,
    signer: &NodeId,
    signing_data: &[u8],
    signature: &[u8],
) -> Result<()> {
    if signature.len() != 96 {
        return Err(anyhow!("message is missing a 96-byte BLS signature"));
    }
    let public_key = parse_committee_key(committee, signer)?;
    let signature =
        BlsSignature::from_slice(signature).map_err(|_| anyhow!("invalid BLS signature"))?;
    if !public_key.verify(signing_data, &signature) {
        return Err(anyhow!("BLS signature verification failed"));
    }
    Ok(())
}

fn verify_transport_qc(
    committee: &Committee,
    context: ConsensusContext,
    certificate: &Certificate,
) -> Result<()> {
    let app_hash = certificate
        .app_hash
        .as_ref()
        .ok_or_else(|| anyhow!("QC is missing its application hash"))?;
    verify_certificate(
        committee,
        certificate,
        context,
        certificate.view,
        &certificate.block_hash,
        Some(app_hash),
        true,
    )
    .map_err(|error| anyhow!("invalid committee-bound QC: {error}"))
}

fn validate_view_change(
    view_change: &ViewChange,
    origin: NodeId,
    validation: &GossipValidationConfig,
) -> Result<()> {
    let committee = &validation.committee;
    view_change
        .validate_context(validation.context)
        .map_err(|error| anyhow!("invalid ViewChange context: {error}"))?;
    if view_change.sender != origin {
        return Err(anyhow!("ViewChange sender does not match logical origin"));
    }
    if view_change.to_view != view_change.from_view.saturating_add(1) {
        return Err(anyhow!("ViewChange target must be from_view + 1"));
    }
    if committee.member(&view_change.sender).is_none() {
        return Err(anyhow!("ViewChange sender is not in the committee"));
    }
    verify_member_signature(
        committee,
        &view_change.sender,
        &view_change.signing_data(),
        &view_change.signature,
    )?;
    if let Some(high_qc) = &view_change.high_qc {
        high_qc
            .validate_context(validation.context)
            .map_err(|error| anyhow!("ViewChange high QC has invalid context: {error}"))?;
        verify_transport_qc(committee, validation.context, high_qc)?;
    }
    Ok(())
}

fn validate_view_change_certificate(
    certificate: &ViewChangeCertificate,
    target_view: View,
    validation: &GossipValidationConfig,
) -> Result<()> {
    let committee = &validation.committee;
    certificate
        .validate_context(validation.context)
        .map_err(|error| anyhow!("invalid ViewChange certificate context: {error}"))?;
    if certificate.view != target_view {
        return Err(anyhow!(
            "ViewChange certificate view does not match NewView"
        ));
    }
    if certificate.view_changes.is_empty() {
        return Err(anyhow!("ViewChange certificate has no members"));
    }

    let mut signers = HashSet::with_capacity(certificate.view_changes.len());
    for view_change in &certificate.view_changes {
        if view_change.to_view != certificate.view {
            return Err(anyhow!(
                "ViewChange certificate contains a different target view"
            ));
        }
        validate_view_change(view_change, view_change.sender, validation)?;
        if !signers.insert(view_change.sender) {
            return Err(anyhow!("ViewChange certificate contains duplicate senders"));
        }
    }
    if !committee
        .has_weighted_quorum(signers.iter().copied())
        .map_err(|error| anyhow!("invalid ViewChange quorum: {error}"))?
    {
        return Err(anyhow!("ViewChange certificate lacks weighted quorum"));
    }
    Ok(())
}

fn validate_authenticated_message(
    message: &Message,
    origin: NodeId,
    validation: &GossipValidationConfig,
) -> Result<()> {
    let context = validation.context;
    let committee = &validation.committee;
    committee
        .validate_context(context)
        .map_err(|error| anyhow!("invalid trusted committee context: {error}"))?;

    match message {
        Message::UserTransaction(envelope) => {
            // The transport origin is the validator that submitted/relayed
            // the envelope, not the user signer.  They are intentionally
            // authenticated as separate identities.
            if committee.member(&origin).is_none() {
                return Err(anyhow!("user transaction origin is not in the committee"));
            }
            envelope
                .validate_for_block(
                    context.genesis_hash,
                    current_timestamp_ms(),
                    validation.allow_dev_envelopes,
                )
                .map_err(|error| anyhow!("invalid user transaction envelope: {error}"))
        }
        Message::Vote(vote) => {
            if vote.voter != origin {
                return Err(anyhow!("Vote voter does not match logical origin"));
            }
            verify_vote(
                committee,
                vote,
                context,
                vote.view,
                &vote.block_hash,
                &vote.app_hash,
                true,
            )
            .map(|_| ())
            .map_err(|error| anyhow!("invalid Vote: {error}"))
        }
        Message::Timeout(timeout) => {
            if timeout.sender != origin {
                return Err(anyhow!("Timeout sender does not match logical origin"));
            }
            timeout
                .validate_context(context)
                .map_err(|error| anyhow!("invalid Timeout context: {error}"))?;
            if committee.member(&timeout.sender).is_none() {
                return Err(anyhow!("Timeout sender is not in the committee"));
            }
            verify_member_signature(
                committee,
                &timeout.sender,
                &timeout.signing_data(),
                &timeout.signature,
            )
        }
        Message::ViewChange(view_change) => validate_view_change(view_change, origin, validation),
        Message::Propose(propose) => {
            propose
                .validate_context(context)
                .map_err(|error| anyhow!("invalid Propose context: {error}"))?;
            propose
                .block
                .validate_context(context)
                .map_err(|error| anyhow!("invalid proposal block context: {error}"))?;
            propose
                .block
                .validate()
                .map_err(|error| anyhow!("invalid proposal block: {error}"))?;
            if propose.block.proposer != origin {
                return Err(anyhow!("proposal proposer does not match logical origin"));
            }
            if committee.leader(propose.block.view) != propose.block.proposer {
                return Err(anyhow!("proposal proposer is not the scheduled leader"));
            }
            propose
                .verify_signature(committee)
                .map_err(|error| anyhow!("invalid proposer signature: {error}"))?;
            if propose.block.height == 0 {
                return Err(anyhow!("network proposal may not use height zero"));
            }
            if propose.block.height == 1 {
                if propose.block.parent != Block::genesis(context).hash()
                    || propose.justify.is_some()
                    || propose.block.justify.is_some()
                {
                    return Err(anyhow!("height-one proposal has an invalid genesis anchor"));
                }
            } else {
                let justify = propose
                    .justify
                    .as_ref()
                    .ok_or_else(|| anyhow!("non-genesis proposal is missing a QC"))?;
                if propose.block.justify.as_ref() != Some(justify)
                    || justify.block_hash != propose.block.parent
                {
                    return Err(anyhow!("proposal QC does not certify its parent"));
                }
                verify_transport_qc(committee, context, justify)?;
            }
            Ok(())
        }
        Message::Prepare(prepare) => {
            prepare
                .validate_context(context)
                .map_err(|error| anyhow!("invalid Prepare context: {error}"))?;
            if committee.leader(prepare.view) != origin {
                return Err(anyhow!("Prepare origin is not the scheduled leader"));
            }
            if prepare.qc.view != prepare.view {
                return Err(anyhow!("Prepare view does not match its QC"));
            }
            verify_transport_qc(committee, context, &prepare.qc)
        }
        Message::NewView(new_view) => {
            new_view
                .validate_context(context)
                .map_err(|error| anyhow!("invalid NewView context: {error}"))?;
            if committee.leader(new_view.view) != origin {
                return Err(anyhow!("NewView origin is not the scheduled leader"));
            }
            validate_view_change_certificate(
                &new_view.view_change_cert,
                new_view.view,
                validation,
            )?;
            let highest_qc = new_view.view_change_cert.highest_qc();
            if new_view.high_qc.as_ref() != highest_qc {
                return Err(anyhow!("NewView high QC is inconsistent with its VCC"));
            }
            if let Some(high_qc) = &new_view.high_qc {
                verify_transport_qc(committee, context, high_qc)?;
            }
            Ok(())
        }
        Message::EquivocationEvidence(proof) => {
            verify_equivocation_proof(committee, proof, context, true)
                .map_err(|error| anyhow!("invalid equivocation evidence: {error}"))
        }
        Message::SyncRequest(_)
        | Message::SyncResponse(_)
        | Message::SnapshotRequest(_)
        | Message::SnapshotResponse(_) => Ok(()),
        Message::Gossip(_) => Err(anyhow!("nested gossip envelope is not admissible")),
    }
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

/// Validate and classify one decoded wire message.  No seen-cache mutation or
/// relay occurs until every applicable semantic and cryptographic check has
/// returned successfully.  Signed user transactions are deliberately a
/// two-stage admission: transport authenticates them here, while the runner
/// admits nonce/mempool policy and calls the outbound transport path only on
/// success.
fn admit_message(
    message: Message,
    peer_id: NodeId,
    gossip_state: &GossipState,
    validation: Option<&GossipValidationConfig>,
) -> Result<Admission> {
    match message {
        Message::Gossip(gossip_msg) => {
            validate_gossip_envelope(gossip_msg.as_ref(), gossip_state.initial_ttl())
                .map_err(|error| anyhow!("invalid gossip envelope: {error}"))?;
            // The envelope ID is derived from the payload, not trusted from
            // the wire.  Check it and consult the bounded seen cache before
            // doing the expensive semantic/BLS admission work.  An attacker
            // can still force one validation for each distinct payload, but
            // replaying the same valid evidence cannot force unbounded BLS
            // verification.
            if gossip_state.has_seen(&gossip_msg.msg_id) {
                return Ok(Admission::Duplicate);
            }

            let is_user_transaction = matches!(&gossip_msg.message, Message::UserTransaction(_));

            if let Message::EquivocationEvidence(proof) = &gossip_msg.message {
                // The stable key is checked after the envelope/msg_id check
                // but before BLS work.  It is safe to fast-drop here because
                // keys are inserted only by the authenticated callback below.
                if gossip_state.has_equivocation_key(proof) {
                    let _ = gossip_state.mark_seen(&gossip_msg.msg_id);
                    return Ok(Admission::Duplicate);
                }
                let validation = validation
                    .ok_or_else(|| anyhow!("gossip received without trusted validation context"))?;
                if !gossip_state.validate_and_mark_equivocation(proof, || {
                    validate_authenticated_message(
                        &gossip_msg.message,
                        gossip_msg.origin,
                        validation,
                    )
                })? {
                    return Ok(Admission::Duplicate);
                }
            } else {
                let validation = validation
                    .ok_or_else(|| anyhow!("gossip received without trusted validation context"))?;
                validate_authenticated_message(&gossip_msg.message, gossip_msg.origin, validation)?;
            }
            if !is_user_transaction && !gossip_state.mark_seen(&gossip_msg.msg_id) {
                return Ok(Admission::Duplicate);
            }
            // Evidence is journaled by the runner before it is rebroadcast.
            // Relaying it here would create a crash window in which peers can
            // receive evidence that this node never durably retained.
            let relay = if is_user_transaction
                || matches!(gossip_msg.message, Message::EquivocationEvidence(_))
            {
                None
            } else {
                gossip_msg.relay()
            };
            Ok(Admission::Deliver {
                origin: gossip_msg.origin,
                message: gossip_msg.message.clone(),
                relay,
            })
        }
        direct => {
            if let Message::EquivocationEvidence(proof) = &direct {
                if let Some(validation) = validation {
                    if gossip_state.has_equivocation_key(proof) {
                        return Ok(Admission::Duplicate);
                    }
                    if !gossip_state.validate_and_mark_equivocation(proof, || {
                        validate_authenticated_message(&direct, peer_id, validation)
                    })? {
                        return Ok(Admission::Duplicate);
                    }
                }
            } else if let Some(validation) = validation {
                validate_authenticated_message(&direct, peer_id, validation)?;
            } else if let Message::UserTransaction(envelope) = &direct {
                validate_user_envelope_structure(envelope)?;
            }
            if matches!(direct, Message::UserTransaction(_))
                && gossip_state.has_seen(&compute_message_id(&direct))
            {
                return Ok(Admission::Duplicate);
            }
            Ok(Admission::Deliver {
                origin: peer_id,
                message: direct,
                relay: None,
            })
        }
    }
}

/// Message format magic bytes
const FORMAT_BINCODE: u8 = 0x01;
const FORMAT_JSON: u8 = 0x02;

/// Serialize a message with bincode (default) or JSON
///
/// Format: [magic_byte][payload]
/// - 0x01 = bincode (default, ~3x faster, ~40% smaller)
/// - 0x02 = JSON (fallback for debugging)
fn serialize_message(msg: &Message) -> Result<Vec<u8>> {
    let payload_size = bincode::serialized_size(msg).context("Failed to size message")?;
    if payload_size
        .checked_add(1)
        .and_then(|size| usize::try_from(size).ok())
        .is_none_or(|size| !message_size_allowed(size))
    {
        return Err(anyhow!(
            "serialized network message exceeds {} byte limit",
            MAX_NETWORK_MESSAGE_BYTES
        ));
    }
    let payload = bincode::serialize(msg).context("Failed to serialize message")?;
    let mut result = Vec::with_capacity(1 + payload.len());
    result.push(FORMAT_BINCODE);
    result.extend(payload);
    Ok(result)
}

/// Deserialize a message, detecting format from magic byte
fn deserialize_message(data: &[u8]) -> Result<Message> {
    if data.is_empty() {
        return Err(anyhow!("Empty message"));
    }

    match data[0] {
        FORMAT_BINCODE => {
            bincode::deserialize(&data[1..]).context("Failed to deserialize bincode message")
        }
        FORMAT_JSON => {
            serde_json::from_slice(&data[1..]).context("Failed to deserialize JSON message")
        }
        _ => {
            // Legacy: try JSON without magic byte for backwards compatibility
            serde_json::from_slice(data).context("Failed to deserialize legacy JSON message")
        }
    }
}

/// Serialize a message as JSON (for debugging/logging)
#[allow(dead_code)]
fn serialize_message_json(msg: &Message) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(msg).context("Failed to serialize message")?;
    if payload
        .len()
        .checked_add(1)
        .is_none_or(|size| !message_size_allowed(size))
    {
        return Err(anyhow!(
            "serialized network message exceeds {} byte limit",
            MAX_NETWORK_MESSAGE_BYTES
        ));
    }
    let mut result = Vec::with_capacity(1 + payload.len());
    result.push(FORMAT_JSON);
    result.extend(payload);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{SignatureScheme, SignedEnvelope, Transaction};
    use crate::consensus::EquivocationProof;
    use crate::crypto::bls::{aggregate_signatures, BlsSecretKey};
    use crate::crypto::Signer;
    use crate::types::{
        Block, Certificate, CommitteeMember, ConsensusConfig, ConsensusContext, NodeId, Vote,
    };

    fn test_context() -> ConsensusContext {
        ConsensusContext::new(0, [7u8; 32])
    }

    fn user_transaction_fixture() -> (GossipValidationConfig, SignedEnvelope, NodeId) {
        let origin = [1u8; 32];
        let committee = Committee::from_members(vec![CommitteeMember {
            node_id: origin,
            bls_pubkey: None,
            voting_power: 1,
        }])
        .expect("test committee");
        let chain_domain = [7u8; 32];
        let context = ConsensusContext::with_genesis(0, committee.hash(), chain_domain);
        let signer = Signer::from_bytes(&[42u8; 32]).expect("test signer");
        let trader = format!("0x{}", hex::encode(signer.address().into_array()));
        let envelope = SignedEnvelope::sign(
            chain_domain,
            &signer,
            0,
            0,
            u64::MAX,
            Transaction::Deposit { trader, amount: 1 },
        )
        .expect("test envelope");
        (
            GossipValidationConfig {
                context,
                committee,
                allow_dev_envelopes: false,
            },
            envelope,
            origin,
        )
    }

    async fn test_local_addr() -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener must bind");
        listener
            .local_addr()
            .expect("test listener must have an address")
            .to_string()
    }

    fn create_test_propose() -> Message {
        let context = test_context();
        let block = Block::genesis(context);
        Message::Propose(Propose {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            block,
            justify: None,
            proposer_signature: vec![],
        })
    }

    fn create_test_vote() -> Message {
        let context = test_context();
        Message::Vote(Vote {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            block_hash: [1u8; 32],
            app_hash: [2u8; 32],
            voter: [3u8; 32],
            signature: vec![4u8; 64],
            bls_pubkey: None,
        })
    }

    fn equivocation_fixture() -> (GossipValidationConfig, EquivocationProof, NodeId) {
        let offender = [1u8; 32];
        let reporter = [9u8; 32];
        let other = [2u8; 32];
        let secrets = [
            BlsSecretKey::from_seed(&[31u8; 32]),
            BlsSecretKey::from_seed(&[32u8; 32]),
        ];
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [7u8; 32],
            node_id: reporter,
            validators: vec![offender, other],
            voting_powers: vec![1, 1],
            view_timeout_ms: 1_000,
            bls_pubkeys: secrets
                .iter()
                .map(|secret| secret.public_key().to_bytes().to_vec())
                .collect(),
            bls_secret_key: None,
        };
        let committee = config.committee().expect("test committee");
        let context = config.context().expect("test context");
        let vote_a = Vote::new_bls(context, 4, [1u8; 32], [11u8; 32], offender, &secrets[0]);
        let vote_b = Vote::new_bls(context, 4, [2u8; 32], [12u8; 32], offender, &secrets[0]);
        let proof = EquivocationProof {
            context,
            offender,
            view: 4,
            hash_a: vote_a.block_hash,
            app_hash_a: vote_a.app_hash,
            hash_b: vote_b.block_hash,
            app_hash_b: vote_b.app_hash,
            signature_a: vote_a.signature,
            signature_b: vote_b.signature,
        };
        (
            GossipValidationConfig {
                context,
                committee,
                allow_dev_envelopes: false,
            },
            proof,
            reporter,
        )
    }

    fn alternate_equivocation_proof(proof: &EquivocationProof) -> EquivocationProof {
        let secret = BlsSecretKey::from_seed(&[31u8; 32]);
        let vote_a = Vote::new_bls(
            proof.context,
            proof.view + 1,
            [3u8; 32],
            [13u8; 32],
            proof.offender,
            &secret,
        );
        let vote_b = Vote::new_bls(
            proof.context,
            proof.view + 1,
            [4u8; 32],
            [14u8; 32],
            proof.offender,
            &secret,
        );
        EquivocationProof {
            context: proof.context,
            offender: proof.offender,
            view: proof.view + 1,
            hash_a: vote_a.block_hash,
            app_hash_a: vote_a.app_hash,
            hash_b: vote_b.block_hash,
            app_hash_b: vote_b.app_hash,
            signature_a: vote_a.signature,
            signature_b: vote_b.signature,
        }
        .canonicalized()
        .expect("alternate proof must be canonical")
    }

    #[test]
    fn forged_first_does_not_poison_gossip_evidence_key() {
        let (validation, proof, reporter) = equivocation_fixture();
        let state = GossipState::default();
        let mut forged = proof.clone();
        forged.signature_a[0] ^= 1;

        let forged_envelope = GossipMessage::new(
            Message::EquivocationEvidence(forged),
            state.initial_ttl(),
            reporter,
        );
        assert!(admit_message(
            Message::Gossip(Box::new(forged_envelope)),
            reporter,
            &state,
            Some(&validation),
        )
        .is_err());
        assert!(!state.has_equivocation_key(&proof));

        let valid_envelope = GossipMessage::new(
            Message::EquivocationEvidence(proof.clone()),
            state.initial_ttl(),
            reporter,
        );
        assert!(matches!(
            admit_message(
                Message::Gossip(Box::new(valid_envelope)),
                reporter,
                &state,
                Some(&validation),
            )
            .expect("valid evidence must remain admissible"),
            Admission::Deliver { .. }
        ));
        assert!(state.has_equivocation_key(&proof));
    }

    #[test]
    fn alternate_valid_gossip_evidence_is_stable_key_duplicate() {
        let (validation, proof, reporter) = equivocation_fixture();
        let alternate = alternate_equivocation_proof(&proof);
        let state = GossipState::default();
        let first = GossipMessage::new(
            Message::EquivocationEvidence(proof),
            state.initial_ttl(),
            reporter,
        );
        admit_message(
            Message::Gossip(Box::new(first)),
            reporter,
            &state,
            Some(&validation),
        )
        .expect("first evidence must pass");

        let alternate = GossipMessage::new(
            Message::EquivocationEvidence(alternate),
            state.initial_ttl(),
            reporter,
        );
        assert!(matches!(
            admit_message(Message::Gossip(Box::new(alternate)), reporter, &state, None,)
                .expect("stable evidence key should fast-drop without BLS context"),
            Admission::Duplicate
        ));
    }

    #[test]
    fn direct_evidence_uses_stable_key_and_forged_first_is_retryable() {
        let (validation, proof, reporter) = equivocation_fixture();
        let state = GossipState::default();
        let mut forged = proof.clone();
        forged.signature_b[0] ^= 1;
        assert!(admit_message(
            Message::EquivocationEvidence(forged),
            reporter,
            &state,
            Some(&validation),
        )
        .is_err());

        assert!(matches!(
            admit_message(
                Message::EquivocationEvidence(proof.clone()),
                reporter,
                &state,
                Some(&validation),
            )
            .expect("valid direct evidence must pass after forged input"),
            Admission::Deliver { .. }
        ));

        let alternate = alternate_equivocation_proof(&proof);
        assert!(matches!(
            admit_message(
                Message::EquivocationEvidence(alternate),
                reporter,
                &state,
                Some(&validation),
            )
            .expect("alternate direct evidence should be classified as duplicate"),
            Admission::Duplicate
        ));
    }

    #[test]
    fn authenticated_equivocation_admission_validates_before_seen_cache() {
        let (validation, proof, reporter) = equivocation_fixture();
        let state = GossipState::default();
        let envelope = GossipMessage::new(
            Message::EquivocationEvidence(proof.clone()),
            state.initial_ttl(),
            reporter,
        );
        let msg_id = envelope.msg_id;
        let admitted = admit_message(
            Message::Gossip(Box::new(envelope.clone())),
            reporter,
            &state,
            Some(&validation),
        )
        .expect("reporter may relay evidence for another offender");
        assert!(matches!(admitted, Admission::Deliver { relay: None, .. }));
        assert!(state.has_seen(&msg_id));

        assert!(matches!(
            admit_message(
                Message::Gossip(Box::new(envelope)),
                reporter,
                &state,
                Some(&validation),
            )
            .expect("duplicate gossip should be classified after validation"),
            Admission::Duplicate
        ));

        let mut forged = proof;
        forged.signature_a[0] ^= 1;
        let forged_envelope = GossipMessage::new(
            Message::EquivocationEvidence(forged),
            state.initial_ttl(),
            reporter,
        );
        let forged_id = forged_envelope.msg_id;
        assert!(matches!(
            admit_message(
                Message::Gossip(Box::new(forged_envelope)),
                reporter,
                &state,
                Some(&validation),
            )
            .expect("an already authenticated evidence key is a duplicate"),
            Admission::Duplicate
        ));
        assert!(state.has_seen(&forged_id));
    }

    #[test]
    fn duplicate_gossip_uses_seen_cache_before_semantic_validation() {
        let (validation, proof, reporter) = equivocation_fixture();
        let state = GossipState::default();
        let envelope = GossipMessage::new(
            Message::EquivocationEvidence(proof),
            state.initial_ttl(),
            reporter,
        );

        admit_message(
            Message::Gossip(Box::new(envelope.clone())),
            reporter,
            &state,
            Some(&validation),
        )
        .expect("first evidence must pass semantic validation");

        // A duplicate with no validation context is still safe to classify
        // as a duplicate: the payload-derived ID was already admitted under
        // the trusted context.  This also proves that a replay cannot force a
        // second BLS verification through this admission gate.
        assert!(matches!(
            admit_message(Message::Gossip(Box::new(envelope)), reporter, &state, None,)
                .expect("duplicate should be fast-dropped before semantic validation"),
            Admission::Duplicate
        ));
    }

    #[test]
    fn forged_gossip_id_is_rejected_without_poisoning_seen_cache() {
        let (validation, proof, reporter) = equivocation_fixture();
        let state = GossipState::default();
        let mut envelope = GossipMessage::new(
            Message::EquivocationEvidence(proof),
            state.initial_ttl(),
            reporter,
        );
        envelope.msg_id = [0xabu8; 32];

        assert!(admit_message(
            Message::Gossip(Box::new(envelope.clone())),
            reporter,
            &state,
            Some(&validation),
        )
        .is_err());
        assert!(!state.has_seen(&envelope.msg_id));
    }

    #[test]
    fn user_transaction_waits_for_application_admission_before_seen_or_relay() {
        let (validation, envelope, origin) = user_transaction_fixture();
        let state = GossipState::default();
        let message = Message::UserTransaction(envelope.clone());
        let gossip = GossipMessage::new(message.clone(), state.initial_ttl(), origin);

        let admitted = admit_message(
            Message::Gossip(Box::new(gossip)),
            origin,
            &state,
            Some(&validation),
        )
        .expect("valid envelope should reach the runner");
        assert!(matches!(
            admitted,
            Admission::Deliver {
                relay: None,
                message: Message::UserTransaction(_),
                ..
            }
        ));
        assert!(!state.has_seen(&compute_message_id(&message)));

        // This is the runner-success side of the boundary: transport marks
        // the stable ID only when the accepted envelope is broadcast.
        assert!(state.mark_seen(&compute_message_id(&message)));
        assert!(matches!(
            admit_message(
                Message::Gossip(Box::new(GossipMessage::new(
                    message,
                    state.initial_ttl(),
                    origin,
                ))),
                origin,
                &state,
                Some(&validation),
            )
            .expect("seen user transaction should be deduplicated"),
            Admission::Duplicate
        ));
    }

    #[test]
    fn invalid_user_transaction_does_not_poison_seen_cache() {
        let (validation, envelope, origin) = user_transaction_fixture();
        let state = GossipState::default();

        let mut invalid_signature = envelope.clone();
        invalid_signature.signature[0] ^= 1;
        let invalid_message = Message::UserTransaction(invalid_signature.clone());
        let invalid_id = compute_message_id(&invalid_message);
        assert!(admit_message(
            Message::Gossip(Box::new(GossipMessage::new(
                invalid_message,
                state.initial_ttl(),
                origin,
            ))),
            origin,
            &state,
            Some(&validation),
        )
        .is_err());
        assert!(!state.has_seen(&invalid_id));

        let mut wrong_domain = envelope;
        wrong_domain.chain_domain = [8u8; 32];
        let wrong_domain_message = Message::UserTransaction(wrong_domain);
        let wrong_domain_id = compute_message_id(&wrong_domain_message);
        assert!(admit_message(
            Message::Gossip(Box::new(GossipMessage::new(
                wrong_domain_message,
                state.initial_ttl(),
                origin,
            ))),
            origin,
            &state,
            Some(&validation),
        )
        .is_err());
        assert!(!state.has_seen(&wrong_domain_id));
    }

    #[test]
    fn direct_user_transaction_deduplicates_after_successful_rebroadcast() {
        let (validation, envelope, origin) = user_transaction_fixture();
        let state = GossipState::default();
        let message = Message::UserTransaction(envelope);

        assert!(matches!(
            admit_message(message.clone(), origin, &state, Some(&validation))
                .expect("valid direct envelope should reach the runner"),
            Admission::Deliver {
                relay: None,
                message: Message::UserTransaction(_),
                ..
            }
        ));
        assert!(!state.has_seen(&compute_message_id(&message)));

        // `broadcast_direct_message` performs this mark after runner-side
        // application admission. A later inbound copy must then be dropped.
        assert!(state.mark_seen(&compute_message_id(&message)));
        assert!(matches!(
            admit_message(message, origin, &state, Some(&validation))
                .expect("rebroadcast copy should be deduplicated"),
            Admission::Duplicate
        ));
    }

    #[tokio::test]
    async fn direct_user_transaction_rebroadcast_reaches_every_connected_peer() {
        let (_validation, envelope, _origin) = user_transaction_fixture();
        let peers: SharedPeers = Arc::new(RwLock::new(HashMap::new()));
        let mut receivers = Vec::new();
        for index in 2..=4u8 {
            let peer_id = [index; 32];
            let (sender, receiver) = mpsc::channel(2);
            peers
                .write()
                .await
                .insert(peer_id, PeerConnection::new(sender));
            receivers.push(receiver);
        }

        rebroadcast_user_transaction_direct([1u8; 32], &peers, None, envelope.clone())
            .await
            .expect("direct rebroadcast should not fail for connected peers");

        for mut receiver in receivers {
            let bytes = receiver
                .recv()
                .await
                .expect("every connected peer must receive the retry");
            let received = deserialize_message(&bytes).expect("peer message must decode");
            let Message::UserTransaction(received_envelope) = received else {
                panic!("retry must carry a user transaction");
            };
            assert_eq!(
                bincode::serialize(&received_envelope).unwrap(),
                bincode::serialize(&envelope).unwrap()
            );
        }
    }

    #[tokio::test]
    async fn direct_user_transaction_rebroadcast_delivers_after_peer_reconnects() {
        let (_validation, envelope, _origin) = user_transaction_fixture();
        let peers: SharedPeers = Arc::new(RwLock::new(HashMap::new()));
        let peer_id = [2u8; 32];

        let (closed_sender, closed_receiver) = mpsc::channel(1);
        drop(closed_receiver);
        peers
            .write()
            .await
            .insert(peer_id, PeerConnection::new(closed_sender));

        // The first pass observes a disconnected writer and leaves the
        // envelope in the caller's canonical mempool for a later retry.
        assert!(
            rebroadcast_user_transaction_direct([1u8; 32], &peers, None, envelope.clone())
                .await
                .is_err()
        );

        let (sender, mut receiver) = mpsc::channel(1);
        peers
            .write()
            .await
            .insert(peer_id, PeerConnection::new(sender));
        rebroadcast_user_transaction_direct([1u8; 32], &peers, None, envelope.clone())
            .await
            .expect("retry after reconnect should enqueue the envelope");

        let bytes = receiver
            .recv()
            .await
            .expect("reconnected peer must receive the retry");
        let received = deserialize_message(&bytes).expect("retry must decode");
        let Message::UserTransaction(received_envelope) = received else {
            panic!("retry must carry a user transaction");
        };
        assert_eq!(
            bincode::serialize(&received_envelope).unwrap(),
            bincode::serialize(&envelope).unwrap()
        );
    }

    #[tokio::test]
    async fn direct_user_transaction_rebroadcast_does_not_wait_on_full_writer() {
        let (_validation, envelope, _origin) = user_transaction_fixture();
        let expected = envelope.clone();
        let peers: SharedPeers = Arc::new(RwLock::new(HashMap::new()));
        let peer_id = [2u8; 32];
        let (sender, _receiver) = mpsc::channel(1);
        sender
            .try_send(vec![0u8])
            .expect("test writer must be full");
        peers
            .write()
            .await
            .insert(peer_id, PeerConnection::new(sender));
        let healthy_peer_id = [3u8; 32];
        let (healthy_sender, mut healthy_receiver) = mpsc::channel(1);
        peers
            .write()
            .await
            .insert(healthy_peer_id, PeerConnection::new(healthy_sender));

        let result = tokio::time::timeout(
            Duration::from_millis(100),
            rebroadcast_user_transaction_direct([1u8; 32], &peers, None, envelope),
        )
        .await
        .expect("a full peer writer must not block the retry worker");
        assert!(result.is_err(), "a full writer should remain retryable");
        let received = deserialize_message(
            &healthy_receiver
                .recv()
                .await
                .expect("healthy peers must still receive the retry"),
        )
        .expect("healthy peer retry must decode");
        let Message::UserTransaction(received_envelope) = received else {
            panic!("healthy peer retry must carry a user transaction");
        };
        assert_eq!(
            bincode::serialize(&received_envelope).unwrap(),
            bincode::serialize(&expected).unwrap()
        );
    }

    #[test]
    fn development_envelopes_follow_explicit_runtime_policy() {
        let (mut validation, envelope, origin) = user_transaction_fixture();
        let mut dev_envelope = envelope;
        dev_envelope.signature_scheme = SignatureScheme::Dev;
        dev_envelope.signature = b"dev".to_vec();
        let message = Message::UserTransaction(dev_envelope);
        let message_id = compute_message_id(&message);

        let production_state = GossipState::default();
        assert!(admit_message(
            message.clone(),
            origin,
            &production_state,
            Some(&validation),
        )
        .is_err());
        assert!(!production_state.has_seen(&message_id));

        validation.allow_dev_envelopes = true;
        let development_state = GossipState::default();
        assert!(admit_message(message, origin, &development_state, Some(&validation)).is_ok());
        assert!(!development_state.has_seen(&message_id));
    }

    #[test]
    fn test_bincode_roundtrip() {
        let msg = create_test_propose();
        let data = serialize_message(&msg).unwrap();

        // Check format byte
        assert_eq!(data[0], FORMAT_BINCODE);

        // Roundtrip
        let decoded = deserialize_message(&data).unwrap();
        match decoded {
            Message::Propose(p) => {
                assert_eq!(p.block.height, 0);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn wire_message_limit_accepts_boundary_and_rejects_one_byte_over() {
        assert!(message_size_allowed(MAX_NETWORK_MESSAGE_BYTES));
        assert!(!message_size_allowed(MAX_NETWORK_MESSAGE_BYTES + 1));
    }

    #[test]
    fn test_bls_prepare_bincode_roundtrip_preserves_voter_metadata() {
        let context = test_context();
        let secret = BlsSecretKey::from_seed(&[21u8; 32]);
        let vote = Vote::new_bls(context, 4, [1u8; 32], [2u8; 32], [3u8; 32], &secret);
        let signature = crate::crypto::bls::BlsSignature::from_slice(&vote.signature)
            .expect("BLS vote signature must parse");
        let aggregate = aggregate_signatures(&[signature])
            .expect("single BLS signature must aggregate")
            .to_bytes()
            .to_vec();
        let certificate = Certificate::new_bls(context, 4, [1u8; 32], vec![vote], aggregate)
            .expect("BLS certificate must be constructible");
        assert!(certificate.votes.is_empty());
        assert_eq!(certificate.voters.len(), 1);
        assert_eq!(certificate.bls_pubkeys.len(), 1);

        let message = Message::Prepare(Prepare {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 4,
            qc: certificate,
        });
        let encoded = serialize_message(&message).expect("prepare must serialize");
        let decoded = deserialize_message(&encoded).expect("prepare must deserialize");

        match decoded {
            Message::Prepare(prepare) => {
                assert!(prepare.qc.votes.is_empty());
                assert_eq!(prepare.qc.voters.len(), 1);
                assert_eq!(prepare.qc.bls_pubkeys.len(), 1);
                assert_eq!(prepare.qc.context(), context);
            }
            other => panic!("expected prepare, got {other:?}"),
        }
    }

    #[test]
    fn test_json_roundtrip() {
        let msg = create_test_vote();
        let data = serialize_message_json(&msg).unwrap();

        // Check format byte
        assert_eq!(data[0], FORMAT_JSON);

        // Roundtrip
        let decoded = deserialize_message(&data).unwrap();
        match decoded {
            Message::Vote(v) => {
                assert_eq!(v.view, 1);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_bincode_smaller_than_json() {
        let msg = create_test_propose();

        let bincode_data = serialize_message(&msg).unwrap();
        let json_data = serialize_message_json(&msg).unwrap();

        // Bincode should be significantly smaller
        assert!(
            bincode_data.len() < json_data.len(),
            "Bincode ({} bytes) should be smaller than JSON ({} bytes)",
            bincode_data.len(),
            json_data.len()
        );

        // Typically 30-50% smaller
        let ratio = (json_data.len() as f64) / (bincode_data.len() as f64);
        assert!(
            ratio > 1.2,
            "JSON should be at least 20% larger than bincode, got ratio {}",
            ratio
        );
    }

    #[test]
    fn test_legacy_json_compatibility() {
        // Test that we can still read old JSON messages without magic byte
        let msg = create_test_vote();
        let legacy_data = serde_json::to_vec(&msg).unwrap();

        // Should deserialize successfully
        let decoded = deserialize_message(&legacy_data).unwrap();
        match decoded {
            Message::Vote(v) => {
                assert_eq!(v.view, 1);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[tokio::test]
    async fn test_new_rejects_authenticated_network_without_local_key() {
        let mut config = NetworkConfig::local_three_nodes(0);
        config.require_authenticated_peers = true;

        let error = TcpNetwork::new(config)
            .await
            .err()
            .expect("authenticated network without a local key must fail closed");
        assert!(format!("{error:#}").contains("local BLS secret key"));
    }

    #[tokio::test]
    async fn test_new_rejects_authenticated_network_without_committee_keys() {
        let mut config = NetworkConfig::local_three_nodes(0);
        config.require_authenticated_peers = true;
        config.bls_secret_key = Some(crate::crypto::bls::BlsSecretKey::from_seed(&[7u8; 32]));

        let error = TcpNetwork::new(config)
            .await
            .err()
            .expect("authenticated network without committee keys must fail closed");
        assert!(format!("{error:#}").contains("committee is empty"));
    }

    #[tokio::test]
    async fn test_new_rejects_duplicate_peer_ids() {
        let mut config = NetworkConfig::local_three_nodes(0);
        config.peers[1].0 = config.peers[0].0;

        let error = TcpNetwork::new(config)
            .await
            .err()
            .expect("duplicate peer IDs must be rejected");
        assert!(format!("{error:#}").contains("duplicate node"));
    }

    #[tokio::test]
    async fn test_new_rejects_self_peer() {
        let mut config = NetworkConfig::local_three_nodes(0);
        config.peers[0].0 = config.node_id;

        let error = TcpNetwork::new(config)
            .await
            .err()
            .expect("a self peer must be rejected");
        assert!(format!("{error:#}").contains("local node"));
    }

    #[tokio::test]
    async fn test_new_allows_unauthenticated_dev_network() {
        let config = NetworkConfig::local_three_nodes(0);

        TcpNetwork::new(config)
            .await
            .expect("dev network should allow unauthenticated peers");
    }

    #[tokio::test]
    async fn test_authentication_requirement_getter() {
        let unauthenticated = TcpNetwork::new(NetworkConfig::local_three_nodes(0))
            .await
            .expect("dev network should be constructible");
        assert!(!unauthenticated.requires_authenticated_peers());

        let mut validator_pubkeys = HashMap::new();
        for (index, node_id) in [[1u8; 32], [2u8; 32], [3u8; 32]].into_iter().enumerate() {
            validator_pubkeys.insert(
                node_id,
                crate::crypto::bls::BlsSecretKey::from_seed(&[(index + 1) as u8; 32]).public_key(),
            );
        }
        let consensus = ConsensusConfig {
            epoch: 0,
            genesis_hash: [7u8; 32],
            node_id: [1u8; 32],
            validators: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
            voting_powers: vec![1, 1, 1],
            view_timeout_ms: 1_000,
            bls_pubkeys: vec![
                validator_pubkeys[&[1u8; 32]].to_bytes().to_vec(),
                validator_pubkeys[&[2u8; 32]].to_bytes().to_vec(),
                validator_pubkeys[&[3u8; 32]].to_bytes().to_vec(),
            ],
            bls_secret_key: None,
        };
        let authenticated_config = NetworkConfig::local_three_nodes(0)
            .with_authentication(
                crate::crypto::bls::BlsSecretKey::from_seed(&[1u8; 32]),
                validator_pubkeys,
            )
            .with_gossip_validation(
                consensus.context().expect("test consensus context"),
                consensus.committee().expect("test consensus committee"),
            );
        let authenticated = TcpNetwork::new(authenticated_config)
            .await
            .expect("complete authenticated network should be constructible");
        assert!(authenticated.requires_authenticated_peers());
    }

    #[tokio::test]
    async fn test_send_releases_peer_lock_before_waiting_on_sender() {
        let mut config = NetworkConfig::local_three_nodes(0);
        config.peers.clear();
        let network = TcpNetwork::new(config)
            .await
            .expect("test network should be constructible");
        let peer_id = [2u8; 32];
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .send(vec![0u8])
            .await
            .expect("test channel should accept its first item");
        network
            .peers
            .write()
            .await
            .insert(peer_id, PeerConnection::new(sender));

        let message = create_test_vote();
        let mut send = Box::pin(network.send_to_internal(peer_id, &message));
        assert!(matches!(
            futures::poll!(&mut send),
            std::task::Poll::Pending
        ));
        let _ = timeout(Duration::from_millis(100), network.peers.write())
            .await
            .expect("send must not hold the peer map lock while awaiting");
        drop(send);
        receiver.close();
    }

    #[test]
    fn test_retry_backoff_resets_only_after_stable_connection() {
        let (short_wait, short_next) =
            connection_retry_delays(MAX_RETRY_DELAY, Duration::from_millis(10));
        assert_eq!(short_wait, MAX_RETRY_DELAY);
        assert_eq!(short_next, MAX_RETRY_DELAY);

        let (stable_wait, stable_next) =
            connection_retry_delays(MAX_RETRY_DELAY, STABLE_CONNECTION_THRESHOLD);
        assert_eq!(stable_wait, INITIAL_RETRY_DELAY);
        assert_eq!(stable_next, INITIAL_RETRY_DELAY * 2);
    }

    #[test]
    fn test_inbound_connection_policy_rejects_lower_and_active_duplicates() {
        let local_id = [2u8; 32];
        let lower_peer_id = [1u8; 32];
        let higher_peer_id = [3u8; 32];
        let peers = HashMap::new();
        let error = validate_inbound_connection(local_id, lower_peer_id, &peers)
            .expect_err("lower-ID inbound connections must be rejected");
        assert!(error.to_string().contains("lower-ID dial rule"));

        let (sender, _receiver) = mpsc::channel(1);
        let mut peers = HashMap::new();
        peers.insert(higher_peer_id, PeerConnection::new(sender));
        let error = validate_inbound_connection(local_id, higher_peer_id, &peers)
            .expect_err("active duplicate inbound connections must be rejected");
        assert!(error.to_string().contains("active connection"));
    }

    #[tokio::test]
    async fn test_wait_for_peers_returns_immediately_without_configured_peers() {
        let mut config = NetworkConfig::local_three_nodes(0);
        config.peers.clear();
        let network = TcpNetwork::new(config)
            .await
            .expect("single-node dev network should be constructible");

        tokio::time::timeout(
            Duration::from_secs(1),
            network.wait_for_peers(Duration::from_secs(10)),
        )
        .await
        .expect("zero-peer wait should return immediately")
        .expect("zero configured peers are already ready");
    }

    #[tokio::test]
    async fn test_wait_for_peers_rejects_closed_sender() {
        let mut config = NetworkConfig::local_three_nodes(0);
        config.peers.truncate(1);
        let peer_id = config.peers[0].0;
        let network = TcpNetwork::new(config)
            .await
            .expect("test network should be constructible");

        let (sender, mut receiver) = mpsc::channel(1);
        receiver.close();
        network
            .peers
            .write()
            .await
            .insert(peer_id, PeerConnection::new(sender));

        let error = network
            .wait_for_peers(Duration::ZERO)
            .await
            .expect_err("a closed peer sender must not satisfy readiness");
        assert!(error.to_string().contains("expected 1, connected 0"));
    }

    #[tokio::test]
    async fn test_stale_cleanup_does_not_remove_replacement() {
        let peer_id = [8u8; 32];
        let peers: SharedPeers = Arc::new(RwLock::new(HashMap::new()));

        let (old_sender, _old_receiver) = mpsc::channel(1);
        let old_connection = PeerConnection::new(old_sender);
        let old_token = old_connection.token.clone();
        peers.write().await.insert(peer_id, old_connection);

        let (replacement_sender, _replacement_receiver) = mpsc::channel(1);
        let replacement = PeerConnection::new(replacement_sender);
        let replacement_token = replacement.token.clone();
        peers.write().await.insert(peer_id, replacement);

        remove_peer_if_current(&peers, peer_id, &old_token).await;

        let peers_guard = peers.read().await;
        let current = peers_guard
            .get(&peer_id)
            .expect("replacement connection must remain installed");
        assert!(Arc::ptr_eq(&current.token, &replacement_token));
    }

    #[tokio::test]
    async fn test_outbound_reconnects_after_remote_connection_closes() {
        let lower_id = [1u8; 32];
        let upper_id = [2u8; 32];
        let lower_addr = test_local_addr().await;
        let upper_addr = test_local_addr().await;

        let lower = TcpNetwork::new(NetworkConfig {
            node_id: lower_id,
            listen_addr: lower_addr.clone(),
            peers: vec![(upper_id, upper_addr.clone())],
            require_authenticated_peers: false,
            bls_secret_key: None,
            validator_pubkeys: HashMap::new(),
            gossip_validation: None,
        })
        .await
        .expect("lower network should be constructible");
        let upper = TcpNetwork::new(NetworkConfig {
            node_id: upper_id,
            listen_addr: upper_addr,
            peers: vec![(lower_id, lower_addr)],
            require_authenticated_peers: false,
            bls_secret_key: None,
            validator_pubkeys: HashMap::new(),
            gossip_validation: None,
        })
        .await
        .expect("upper network should be constructible");

        lower.start().await.expect("lower network must start");
        upper.start().await.expect("upper network must start");
        upper
            .wait_for_peers(Duration::from_secs(2))
            .await
            .expect("upper node must establish its lower-ID outbound connection");

        // Drop the lower node's active sender. Its writer closes the TCP
        // stream, which must make the upper outbound supervisor reconnect.
        lower
            .peers
            .write()
            .await
            .remove(&upper_id)
            .expect("lower node must have the inbound connection");

        timeout(Duration::from_secs(2), async {
            loop {
                if !upper.peers.read().await.contains_key(&lower_id) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("upper node must observe the closed connection");

        upper
            .wait_for_peers(Duration::from_secs(2))
            .await
            .expect("upper outbound supervisor must reconnect");
    }
}
