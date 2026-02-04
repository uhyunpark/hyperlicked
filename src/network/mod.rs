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

pub use active_sync::{ActiveSyncClient, ActiveSyncConfig, SyncResult};
pub use gossip::{GossipConfig, GossipMessage, GossipState, GossipStats, select_gossip_peers};
pub use handshake::{HandshakeConfig, HandshakeResult};
pub use mock::MockNetwork;
pub use sync::{SyncClient, SyncHandler};
pub use transport::TcpNetwork;

use crate::crypto::bls::{BlsPublicKey, BlsSecretKey};

use async_trait::async_trait;

use crate::types::{Message, NewView, NodeId, Prepare, Propose, ViewChange, Vote};

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
}

impl std::fmt::Debug for NetworkConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkConfig")
            .field("node_id", &crate::types::hash_short(&self.node_id))
            .field("listen_addr", &self.listen_addr)
            .field("peers", &self.peers.len())
            .field("require_authenticated_peers", &self.require_authenticated_peers)
            .field("has_bls_key", &self.bls_secret_key.is_some())
            .field("validator_pubkeys", &self.validator_pubkeys.len())
            .finish()
    }
}

impl NetworkConfig {
    /// Create config for local 3-node testing (unauthenticated)
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

    /// Create handshake config from network config
    pub fn handshake_config(&self) -> HandshakeConfig {
        if self.require_authenticated_peers {
            if let Some(ref bls_sk) = self.bls_secret_key {
                HandshakeConfig::authenticated(
                    self.node_id,
                    bls_sk.clone(),
                    self.validator_pubkeys.clone(),
                )
            } else {
                // Authentication required but no key - this is a configuration error
                // Log warning and fall back to unauthenticated
                tracing::warn!(
                    "Authentication required but no BLS key configured - falling back to unauthenticated"
                );
                HandshakeConfig::unauthenticated(self.node_id)
            }
        } else {
            HandshakeConfig::unauthenticated(self.node_id)
        }
    }
}
