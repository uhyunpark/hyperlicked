//! Network Layer for Multi-Node Consensus
//!
//! Provides TCP-based networking for HotStuff-2 consensus:
//! - Broadcast proposals to all validators
//! - Send votes to leader
//! - Broadcast prepare certificates
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

mod transport;

pub use transport::TcpNetwork;

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

    /// Receive the next incoming message (blocking)
    async fn recv(&self) -> anyhow::Result<(NodeId, Message)>;

    /// Get our own node ID
    fn node_id(&self) -> NodeId;
}

/// Network configuration
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Our node ID
    pub node_id: NodeId,
    /// Our listen address (e.g., "0.0.0.0:9000")
    pub listen_addr: String,
    /// Peer addresses: (NodeId, "host:port")
    pub peers: Vec<(NodeId, String)>,
}

impl NetworkConfig {
    /// Create config for local 3-node testing
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
        }
    }
}
