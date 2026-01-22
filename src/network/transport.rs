//! TCP Transport Implementation
//!
//! Simple TCP-based networking for consensus messages.
//! Uses length-prefixed bincode for efficient message framing.
//!
//! ## Serialization
//!
//! Consensus messages use bincode for performance (2-5x faster, 30-50% smaller
//! than JSON). A magic byte prefix identifies the format for forward compatibility.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use super::{Network, NetworkConfig};
use crate::types::{hash_short, Message, NewView, NodeId, Prepare, Propose, ViewChange, Vote};

/// TCP-based network implementation
pub struct TcpNetwork {
    /// Our node ID
    node_id: NodeId,

    /// Connected peers: NodeId -> write half of stream
    peers: Arc<RwLock<HashMap<NodeId, mpsc::Sender<Vec<u8>>>>>,

    /// Channel for received messages
    incoming_rx: mpsc::Receiver<(NodeId, Message)>,

    /// Sender for incoming messages (used by listener tasks)
    incoming_tx: mpsc::Sender<(NodeId, Message)>,

    /// Network config
    config: NetworkConfig,
}

impl TcpNetwork {
    /// Create and start a new TCP network
    pub async fn new(config: NetworkConfig) -> Result<Self> {
        let (incoming_tx, incoming_rx) = mpsc::channel(1000);
        let peers = Arc::new(RwLock::new(HashMap::new()));

        let network = Self {
            node_id: config.node_id,
            peers,
            incoming_rx,
            incoming_tx,
            config,
        };

        Ok(network)
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
            "Network listening"
        );

        // Spawn listener task
        let incoming_tx = self.incoming_tx.clone();
        let peers = self.peers.clone();
        let our_node_id = self.node_id;

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        debug!(%addr, "Accepted connection");
                        let tx = incoming_tx.clone();
                        let peers = peers.clone();
                        tokio::spawn(handle_connection(stream, tx, peers, our_node_id));
                    }
                    Err(e) => {
                        error!(error = %e, "Accept failed");
                    }
                }
            }
        });

        // Connect to peers with LOWER IDs only (to avoid duplicate connections)
        // Nodes with higher IDs will connect to us
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
            let our_node_id = self.node_id;

            tokio::spawn(async move {
                connect_to_peer(peer_id, addr, peers, incoming_tx, our_node_id).await;
            });
        }

        Ok(())
    }

    /// Send a message to a specific peer
    async fn send_to(&self, to: NodeId, msg: &Message) -> Result<()> {
        let data = serialize_message(msg)?;

        let peers = self.peers.read().await;
        if let Some(tx) = peers.get(&to) {
            tx.send(data)
                .await
                .map_err(|_| anyhow!("Failed to send to peer"))?;
            Ok(())
        } else {
            Err(anyhow!(
                "Peer {} not connected",
                hash_short(&to)
            ))
        }
    }

    /// Broadcast a message to all connected peers
    async fn broadcast(&self, msg: &Message) -> Result<()> {
        let data = serialize_message(msg)?;
        let peers = self.peers.read().await;

        for (peer_id, tx) in peers.iter() {
            if let Err(e) = tx.send(data.clone()).await {
                warn!(peer = %hash_short(peer_id), error = %e, "Failed to broadcast to peer");
            }
        }

        Ok(())
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
        self.broadcast(&Message::Propose(propose)).await
    }

    async fn send_vote(&self, to: NodeId, vote: Vote) -> Result<()> {
        debug!(
            view = vote.view,
            to = %hash_short(&to),
            "Sending vote"
        );
        self.send_to(to, &Message::Vote(vote)).await
    }

    async fn broadcast_prepare(&self, prepare: Prepare) -> Result<()> {
        debug!(view = prepare.view, "Broadcasting prepare");
        self.broadcast(&Message::Prepare(prepare)).await
    }

    async fn broadcast_view_change(&self, vc: ViewChange) -> Result<()> {
        debug!(
            from_view = vc.from_view,
            to_view = vc.to_view,
            "Broadcasting view change"
        );
        self.broadcast(&Message::ViewChange(vc)).await
    }

    async fn broadcast_new_view(&self, nv: NewView) -> Result<()> {
        debug!(view = nv.view, "Broadcasting new view");
        self.broadcast(&Message::NewView(nv)).await
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
}

/// Handle an incoming connection
async fn handle_connection(
    stream: TcpStream,
    incoming_tx: mpsc::Sender<(NodeId, Message)>,
    peers: Arc<RwLock<HashMap<NodeId, mpsc::Sender<Vec<u8>>>>>,
    our_node_id: NodeId,
) {
    let (mut read_half, mut write_half) = stream.into_split();

    // First, perform handshake: receive peer's node ID
    let mut id_buf = [0u8; 32];
    if let Err(e) = read_half.read_exact(&mut id_buf).await {
        warn!(error = %e, "Failed to read peer ID");
        return;
    }
    let peer_id: NodeId = id_buf;

    // Send our node ID
    if let Err(e) = write_half.write_all(&our_node_id).await {
        warn!(error = %e, "Failed to send our ID");
        return;
    }

    info!(peer = %hash_short(&peer_id), "Peer connected");

    // Create channel for outgoing messages to this peer
    let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(100);

    // Store in peers map
    {
        let mut peers = peers.write().await;
        peers.insert(peer_id, write_tx);
    }

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
        // Read length prefix
        let mut len_buf = [0u8; 4];
        if read_half.read_exact(&mut len_buf).await.is_err() {
            break;
        }
        let len = u32::from_be_bytes(len_buf) as usize;

        // Sanity check
        if len > 10_000_000 {
            warn!(len, "Message too large");
            break;
        }

        // Read message
        let mut msg_buf = vec![0u8; len];
        if read_half.read_exact(&mut msg_buf).await.is_err() {
            break;
        }

        // Deserialize
        match deserialize_message(&msg_buf) {
            Ok(msg) => {
                if incoming_tx.send((peer_id, msg)).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to deserialize message");
            }
        }
    }

    // Clean up
    info!(peer = %hash_short(&peer_id), "Peer disconnected");
    peers.write().await.remove(&peer_id);
}

/// Connect to a peer with retry
async fn connect_to_peer(
    peer_id: NodeId,
    addr: String,
    peers: Arc<RwLock<HashMap<NodeId, mpsc::Sender<Vec<u8>>>>>,
    incoming_tx: mpsc::Sender<(NodeId, Message)>,
    our_node_id: NodeId,
) {
    let mut retry_delay = std::time::Duration::from_millis(100);
    let max_delay = std::time::Duration::from_secs(5);

    loop {
        match TcpStream::connect(&addr).await {
            Ok(stream) => {
                info!(peer = %hash_short(&peer_id), %addr, "Connected to peer");

                // Handshake: send our ID, receive their ID
                let (mut read_half, mut write_half) = stream.into_split();

                // Send our ID
                if write_half.write_all(&our_node_id).await.is_err() {
                    tokio::time::sleep(retry_delay).await;
                    continue;
                }

                // Read their ID
                let mut id_buf = [0u8; 32];
                if read_half.read_exact(&mut id_buf).await.is_err() {
                    tokio::time::sleep(retry_delay).await;
                    continue;
                }

                // Create channel for outgoing messages
                let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(100);

                // Store in peers map
                {
                    let mut peers_guard = peers.write().await;
                    peers_guard.insert(peer_id, write_tx);
                }

                // Spawn writer task
                let peer_id_for_writer = peer_id;
                tokio::spawn(async move {
                    while let Some(data) = write_rx.recv().await {
                        let len = data.len() as u32;
                        if write_half.write_all(&len.to_be_bytes()).await.is_err() {
                            break;
                        }
                        if write_half.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    debug!(peer = %hash_short(&peer_id_for_writer), "Outbound writer ended");
                });

                // Spawn reader task
                let incoming_tx = incoming_tx.clone();
                let peers = peers.clone();
                tokio::spawn(async move {
                    loop {
                        // Read length prefix
                        let mut len_buf = [0u8; 4];
                        if read_half.read_exact(&mut len_buf).await.is_err() {
                            break;
                        }
                        let len = u32::from_be_bytes(len_buf) as usize;

                        if len > 10_000_000 {
                            break;
                        }

                        let mut msg_buf = vec![0u8; len];
                        if read_half.read_exact(&mut msg_buf).await.is_err() {
                            break;
                        }

                        match deserialize_message(&msg_buf) {
                            Ok(msg) => {
                                if incoming_tx.send((peer_id, msg)).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "Deserialize failed");
                            }
                        }
                    }
                    peers.write().await.remove(&peer_id);
                });

                return; // Successfully connected
            }
            Err(e) => {
                debug!(peer = %hash_short(&peer_id), %addr, error = %e, "Connect failed, retrying");
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(max_delay);
            }
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
    let mut result = Vec::with_capacity(1 + payload.len());
    result.push(FORMAT_JSON);
    result.extend(payload);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Block;

    fn create_test_propose() -> Message {
        let block = Block::genesis();
        Message::Propose(Propose {
            block,
            justify: None,
        })
    }

    fn create_test_vote() -> Message {
        Message::Vote(Vote {
            view: 1,
            block_hash: [1u8; 32],
            app_hash: [2u8; 32],
            voter: [3u8; 32],
            signature: vec![4u8; 64],
            bls_pubkey: None,
        })
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
}
