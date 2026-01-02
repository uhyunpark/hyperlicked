//! Multi-Node Consensus Runner
//!
//! Runs a validator node in a multi-node network.
//!
//! Usage:
//!   cargo run --bin multinode -- --node 0
//!   cargo run --bin multinode -- --node 1
//!   cargo run --bin multinode -- --node 2

use std::time::Duration;

use anyhow::Result;
use hyperlicked::app::AppState;
use hyperlicked::consensus::ConsensusRunner;
use hyperlicked::network::{NetworkConfig, TcpNetwork};
use hyperlicked::types::{hash_short, ConsensusConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    // Parse args
    let args: Vec<String> = std::env::args().collect();
    let node_index: usize = args
        .iter()
        .position(|a| a == "--node")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .expect("Usage: multinode --node <0|1|2>");

    if node_index > 2 {
        panic!("Node index must be 0, 1, or 2");
    }

    println!("╔════════════════════════════════════════╗");
    println!("║   Hyperliquid-RS Multi-Node v0.1.0     ║");
    println!("║   HotStuff-2 Consensus Engine          ║");
    println!("╚════════════════════════════════════════╝");
    println!();

    // Create network config
    let net_config = NetworkConfig::local_three_nodes(node_index);

    println!("Node Configuration:");
    println!("  Index: {}", node_index);
    println!("  Node ID: {}", hash_short(&net_config.node_id));
    println!("  Listen: {}", net_config.listen_addr);
    println!("  Peers: {:?}", net_config.peers.iter().map(|(_, addr)| addr).collect::<Vec<_>>());
    println!();

    // Create consensus config
    let node_ids = [
        [1u8; 32],
        [2u8; 32],
        [3u8; 32],
    ];

    let consensus_config = ConsensusConfig {
        node_id: net_config.node_id,
        validators: node_ids.to_vec(),
        view_timeout_ms: 3000,
    };

    println!("Consensus Configuration:");
    println!("  Validators: {}", consensus_config.n());
    println!("  Quorum: {}", consensus_config.quorum());
    println!("  Byzantine fault tolerance: {}", consensus_config.f());
    println!();

    // Create and start network
    let network = TcpNetwork::new(net_config).await?;
    network.start().await?;

    // Wait for connections to establish
    println!("Waiting for peer connections...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create and run consensus with AppState
    let app_state = AppState::new();
    let mut runner = ConsensusRunner::new(consensus_config, network).await?
        .with_app(app_state);

    println!("Starting consensus with orderbook...");
    println!("────────────────────────────────────────");

    runner.run().await?;

    Ok(())
}
