//! Hyperliquid Server
//!
//! Runs the API server with consensus in the background.
//!
//! Usage:
//!   cargo run --bin hl-server
//!   cargo run --bin hl-server -- --port 8080
//!
//! Environment variables:
//!   RUST_LOG=info        - Log level (error, warn, info, debug, trace)
//!   BLOCK_TIME_MS=100    - Block interval in milliseconds
//!   LOG_BLOCKS=true      - Log every block (even empty ones)

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{info, debug, warn};

use hyperlicked::api::{create_router, SharedState, WebSocketHandler};
use hyperlicked::api::state::PriceLevel;
use hyperlicked::app::AppState;
use hyperlicked::consensus::AppHook;
use hyperlicked::types::Block;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file (if exists)
    dotenvy::dotenv().ok();

    // Initialize logging from RUST_LOG env var (default: info)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap())
        )
        .with_target(true)
        .init();

    println!("╔════════════════════════════════════════╗");
    println!("║     Hyperlicked Server v0.1.0          ║");
    println!("║     REST + WebSocket + Consensus       ║");
    println!("╚════════════════════════════════════════╝");
    println!();

    // Port: CLI arg > env var > default 8080
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::var("PORT").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(8080);

    // Create shared state
    let app_state = AppState::new();
    let shared_state = SharedState::new(app_state);

    println!("Configuration:");
    println!("  Port: {}", port);
    println!("  Markets: BTC-USDT");
    println!();

    // Create router with CORS
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = create_router(shared_state.clone())
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    // Spawn consensus simulation in background
    let consensus_state = shared_state.clone();
    tokio::spawn(async move {
        run_consensus_loop(consensus_state).await;
    });

    // Start server
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("Starting server on http://{}", addr);
    println!("────────────────────────────────────────");
    println!();
    println!("API v1 Endpoints:");
    println!("  GET  /api/v1/markets              - List markets");
    println!("  GET  /api/v1/markets/:sym         - Get market info");
    println!("  GET  /api/v1/markets/:sym/orderbook - Get orderbook");
    println!("  GET  /api/v1/accounts/:addr       - Get account");
    println!("  GET  /api/v1/accounts/:addr/positions - Get positions");
    println!("  POST /api/v1/orders               - Submit signed order");
    println!("  POST /api/v1/orders/cancel        - Cancel order");
    println!("  POST /api/v1/delegations          - Register agent key");
    println!("  GET  /api/v1/chain/status         - Chain status");
    println!("  WS   /ws                          - WebSocket stream");
    println!();

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Run consensus loop in background
/// This simulates block production and executes pending transactions
async fn run_consensus_loop(state: SharedState) {
    // Configuration from environment
    let block_time_ms: u64 = std::env::var("BLOCK_TIME_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);

    let log_all_blocks = std::env::var("LOG_BLOCKS")
        .map(|s| s == "true" || s == "1")
        .unwrap_or(false);

    let mut height = 0u64;
    let mut view = 0u64;
    let start_time = std::time::Instant::now();

    info!(
        block_time_ms,
        log_all_blocks,
        "Consensus loop started"
    );

    loop {
        // Wait for block interval (0 = no wait, max speed)
        if block_time_ms > 0 {
            tokio::time::sleep(Duration::from_millis(block_time_ms)).await;
        } else {
            // Yield to allow other tasks (API requests) to run
            tokio::task::yield_now().await;
        }

        view += 1;

        // Check if there are pending transactions
        let (bucket0, bucket1, bucket2) = {
            let app = state.app.read().await;
            app.mempool_stats()
        };

        let total_pending = bucket0 + bucket1 + bucket2;

        // Produce block if there are transactions (or every 10th view for heartbeat)
        if total_pending > 0 || view % 10 == 0 {
            height += 1;

            // Create block
            let block = Block {
                view,
                height,
                parent: [0u8; 32],
                payload: vec![],
                proposer: [1u8; 32],
                app_hash: [0u8; 32],
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            };

            // Execute block (processes mempool transactions)
            let exec_start = std::time::Instant::now();
            let app_hash = {
                let mut app = state.app.write().await;
                app.execute(&block)
            };
            let exec_time = exec_start.elapsed();

            // Broadcast block committed event
            WebSocketHandler::broadcast_block(
                &state,
                height,
                &hex::encode(&app_hash[..4]),
                total_pending,
            );

            // Broadcast orderbook update
            let (bids, asks, best_bid, best_ask) = {
                let app = state.app.read().await;
                if let Some(book) = app.orderbook("BTC-USDT") {
                    let bids: Vec<PriceLevel> = book.bid_levels(20).iter().map(|l| PriceLevel {
                        price: l.price,
                        size: l.size,
                    }).collect();
                    let asks: Vec<PriceLevel> = book.ask_levels(20).iter().map(|l| PriceLevel {
                        price: l.price,
                        size: l.size,
                    }).collect();
                    let best_bid = bids.first().map(|l| l.price);
                    let best_ask = asks.first().map(|l| l.price);
                    (bids, asks, best_bid, best_ask)
                } else {
                    (vec![], vec![], None, None)
                }
            };

            WebSocketHandler::broadcast_orderbook_update(&state, "BTC-USDT", bids, asks);

            // Log block production
            if total_pending > 0 {
                // Always log blocks with transactions
                info!(
                    height,
                    view,
                    txs = total_pending,
                    hash = %hex::encode(&app_hash[..4]),
                    exec_ms = exec_time.as_millis(),
                    best_bid = ?best_bid,
                    best_ask = ?best_ask,
                    "📦 Block committed"
                );
            } else if log_all_blocks {
                // Log empty blocks only if LOG_BLOCKS=true
                debug!(
                    height,
                    view,
                    hash = %hex::encode(&app_hash[..4]),
                    "📦 Heartbeat block"
                );
            }

            // Periodic status log every 100 blocks
            if height % 100 == 0 {
                let uptime = start_time.elapsed();
                let blocks_per_sec = height as f64 / uptime.as_secs_f64();
                info!(
                    height,
                    uptime_secs = uptime.as_secs(),
                    blocks_per_sec = format!("{:.2}", blocks_per_sec),
                    "📊 Status"
                );
            }
        }
    }
}
