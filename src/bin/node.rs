//! Hyperliquid Node Binary
//!
//! Runs a single-node consensus engine for testing.
//!
//! Usage:
//!   cargo run --bin hl-node
//!   cargo run --bin hl-node -- --blocks 100

use std::time::{Duration, Instant};

use hyperlicked::app::AppState;
use hyperlicked::consensus::{Engine, MemoryBlockStore};
use hyperlicked::types::{hash_short, ConsensusConfig};

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    println!("╔════════════════════════════════════════╗");
    println!("║      Hyperlicked    Node v0.1.0        ║");
    println!("║      HotStuff-2 Consensus Engine       ║");
    println!("╚════════════════════════════════════════╝");
    println!();

    // Parse args (simple)
    let args: Vec<String> = std::env::args().collect();
    let num_blocks: u64 = args
        .iter()
        .position(|a| a == "--blocks")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    // Create single-node config
    let config = ConsensusConfig::single_node();
    println!("Config:");
    println!("  Node ID: {}", hash_short(&config.node_id));
    println!("  Validators: {}", config.n());
    println!("  Quorum: {}", config.quorum());
    println!("  Blocks to produce: {}", num_blocks);
    println!();

    // Create engine with AppState
    let app = AppState::new();
    let store = MemoryBlockStore::new();
    let mut engine = Engine::new(config, app, store);

    println!("Starting consensus loop with orderbook...");
    println!("────────────────────────────────────────");

    let start = Instant::now();
    let mut blocks_produced = 0u64;

    // Run consensus loop
    while engine.committed_height() < num_blocks {
        if let Some(block) = engine.tick() {
            blocks_produced += 1;
            println!(
                "Block #{:>4} | View {:>4} | Hash {} | Time {:>6.2}ms",
                block.height,
                block.view,
                hash_short(&block.hash()),
                start.elapsed().as_secs_f64() * 1000.0
            );
        }

        // Small delay for readability (remove in production)
        std::thread::sleep(Duration::from_millis(10));
    }

    let elapsed = start.elapsed();
    println!("────────────────────────────────────────");
    println!();
    println!("Summary:");
    println!("  Blocks produced: {}", blocks_produced);
    println!("  Total time: {:.2?}", elapsed);
    println!(
        "  Blocks/sec: {:.2}",
        blocks_produced as f64 / elapsed.as_secs_f64()
    );
    println!(
        "  Avg block time: {:.2}ms",
        elapsed.as_millis() as f64 / blocks_produced as f64
    );
    println!();
    println!("Phase 1-3 complete! Consensus + App layer working.");
    println!();
    println!("Next steps:");
    println!("  - Phase 4: Add API (REST, WebSocket)");
    println!("  - Phase 5: Liquidation engine");
    println!("  - Phase 6: Production optimizations");
}
