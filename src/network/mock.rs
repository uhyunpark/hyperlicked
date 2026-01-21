//! Mock Network for In-Process Testing
//!
//! Provides a channel-based network mock for deterministic multi-node testing
//! without real TCP connections. All messages are delivered via MPSC channels.
//!
//! ## Usage
//!
//! ```ignore
//! let (net0, net1, net2) = MockNetwork::create_connected_trio();
//! // Each network can send/receive messages to/from others
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex};

use super::Network;
use crate::types::{Message, NewView, NodeId, Prepare, Propose, ViewChange, Vote};

/// Channel capacity for message queues
const CHANNEL_CAPACITY: usize = 1000;

/// Mock network implementation using MPSC channels
pub struct MockNetwork {
    /// This node's ID
    node_id: NodeId,
    /// Inbox for receiving messages
    inbox: Arc<Mutex<mpsc::Receiver<(NodeId, Message)>>>,
    /// Outbox senders to all peers (including self for broadcasts)
    peers: HashMap<NodeId, mpsc::Sender<(NodeId, Message)>>,
    /// All node IDs in the network
    all_nodes: Vec<NodeId>,
}

impl MockNetwork {
    /// Create a trio of connected mock networks for 3-node testing
    pub fn create_connected_trio() -> (Self, Self, Self) {
        let node_ids: [NodeId; 3] = [[1u8; 32], [2u8; 32], [3u8; 32]];

        // Create channels for each node
        let (tx0, rx0) = mpsc::channel(CHANNEL_CAPACITY);
        let (tx1, rx1) = mpsc::channel(CHANNEL_CAPACITY);
        let (tx2, rx2) = mpsc::channel(CHANNEL_CAPACITY);

        let senders = [tx0, tx1, tx2];
        let receivers = [rx0, rx1, rx2];

        // Build peer maps for each node
        let mut networks = Vec::with_capacity(3);

        for (i, rx) in receivers.into_iter().enumerate() {
            let mut peers = HashMap::new();
            for (j, tx) in senders.iter().enumerate() {
                // Include all nodes including self (for broadcasts)
                peers.insert(node_ids[j], tx.clone());
            }

            networks.push(MockNetwork {
                node_id: node_ids[i],
                inbox: Arc::new(Mutex::new(rx)),
                peers,
                all_nodes: node_ids.to_vec(),
            });
        }

        // Extract in reverse order to avoid move issues
        let net2 = networks.pop().unwrap();
        let net1 = networks.pop().unwrap();
        let net0 = networks.pop().unwrap();

        (net0, net1, net2)
    }

    /// Create N connected mock networks
    pub fn create_connected(n: usize) -> Vec<Self> {
        assert!(n > 0, "Need at least one node");

        // Generate node IDs
        let node_ids: Vec<NodeId> = (0..n).map(|i| {
            let mut id = [0u8; 32];
            id[0] = (i + 1) as u8; // Start from 1
            id
        }).collect();

        // Create channels
        let channels: Vec<_> = (0..n)
            .map(|_| mpsc::channel(CHANNEL_CAPACITY))
            .collect();

        let senders: Vec<_> = channels.iter().map(|(tx, _)| tx.clone()).collect();
        let receivers: Vec<_> = channels.into_iter().map(|(_, rx)| rx).collect();

        // Build networks
        receivers
            .into_iter()
            .enumerate()
            .map(|(i, rx)| {
                let mut peers = HashMap::new();
                for (j, tx) in senders.iter().enumerate() {
                    peers.insert(node_ids[j], tx.clone());
                }

                MockNetwork {
                    node_id: node_ids[i],
                    inbox: Arc::new(Mutex::new(rx)),
                    peers,
                    all_nodes: node_ids.clone(),
                }
            })
            .collect()
    }

    /// Send a message to a specific node
    async fn send_to(&self, to: NodeId, msg: Message) -> anyhow::Result<()> {
        if let Some(tx) = self.peers.get(&to) {
            tx.send((self.node_id, msg))
                .await
                .map_err(|e| anyhow::anyhow!("send failed: {}", e))?;
        }
        Ok(())
    }

    /// Broadcast a message to all nodes except self
    async fn broadcast(&self, msg: Message) -> anyhow::Result<()> {
        for (&peer_id, tx) in &self.peers {
            if peer_id != self.node_id {
                // Ignore send errors (peer might be dropped)
                let _ = tx.send((self.node_id, msg.clone())).await;
            }
        }
        Ok(())
    }

    /// Get all node IDs in the network
    pub fn all_node_ids(&self) -> &[NodeId] {
        &self.all_nodes
    }
}

#[async_trait]
impl Network for MockNetwork {
    async fn broadcast_propose(&self, propose: Propose) -> anyhow::Result<()> {
        self.broadcast(Message::Propose(propose)).await
    }

    async fn send_vote(&self, to: NodeId, vote: Vote) -> anyhow::Result<()> {
        self.send_to(to, Message::Vote(vote)).await
    }

    async fn broadcast_prepare(&self, prepare: Prepare) -> anyhow::Result<()> {
        self.broadcast(Message::Prepare(prepare)).await
    }

    async fn broadcast_view_change(&self, vc: ViewChange) -> anyhow::Result<()> {
        self.broadcast(Message::ViewChange(vc)).await
    }

    async fn broadcast_new_view(&self, nv: NewView) -> anyhow::Result<()> {
        self.broadcast(Message::NewView(nv)).await
    }

    async fn recv(&self) -> anyhow::Result<(NodeId, Message)> {
        let mut inbox = self.inbox.lock().await;
        inbox
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("channel closed"))
    }

    fn node_id(&self) -> NodeId {
        self.node_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Block;

    #[tokio::test]
    async fn test_create_connected_trio() {
        let (net0, net1, net2) = MockNetwork::create_connected_trio();

        assert_eq!(net0.node_id(), [1u8; 32]);
        assert_eq!(net1.node_id(), [2u8; 32]);
        assert_eq!(net2.node_id(), [3u8; 32]);

        assert_eq!(net0.peers.len(), 3);
        assert_eq!(net1.peers.len(), 3);
        assert_eq!(net2.peers.len(), 3);
    }

    #[tokio::test]
    async fn test_send_vote() {
        let (net0, net1, _net2) = MockNetwork::create_connected_trio();

        // Node 0 sends vote to Node 1
        let vote = Vote::new(1, [0u8; 32], [0u8; 32], net0.node_id());
        net0.send_vote(net1.node_id(), vote.clone()).await.unwrap();

        // Node 1 receives it
        let (from, msg) = net1.recv().await.unwrap();
        assert_eq!(from, net0.node_id());
        assert!(matches!(msg, Message::Vote(_)));
    }

    #[tokio::test]
    async fn test_broadcast_propose() {
        let (net0, net1, net2) = MockNetwork::create_connected_trio();

        // Node 0 broadcasts proposal
        let propose = Propose {
            block: Block::genesis(),
            justify: None,
        };
        net0.broadcast_propose(propose).await.unwrap();

        // Both Node 1 and Node 2 should receive it
        let (from1, msg1) = net1.recv().await.unwrap();
        let (from2, msg2) = net2.recv().await.unwrap();

        assert_eq!(from1, net0.node_id());
        assert_eq!(from2, net0.node_id());
        assert!(matches!(msg1, Message::Propose(_)));
        assert!(matches!(msg2, Message::Propose(_)));
    }

    #[tokio::test]
    async fn test_create_connected_n() {
        let networks = MockNetwork::create_connected(5);
        assert_eq!(networks.len(), 5);

        for (i, net) in networks.iter().enumerate() {
            let mut expected_id = [0u8; 32];
            expected_id[0] = (i + 1) as u8;
            assert_eq!(net.node_id(), expected_id);
        }
    }
}
