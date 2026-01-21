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
//!   DATA_DIR=/path       - RocksDB persistence directory (optional)
//!   SNAPSHOT_INTERVAL=1000 - Snapshot every N blocks
//!   ORACLE_ENABLED=true  - Enable oracle system at startup (dev mode)

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

use hyperlicked::api::{create_router, AssetCtxData, SharedState, WebSocketHandler};
use hyperlicked::api::state::PriceLevel;
use hyperlicked::app::oracle::{FetcherConfig, OracleConfig, OracleFetcher};
use hyperlicked::app::AppState;
use hyperlicked::config::Config;
use hyperlicked::consensus::AppHook;
use hyperlicked::storage::{ConsensusState, PersistentStore, RocksDbStore};
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

    // Initialize config from environment
    let config = Config::global();

    println!("╔════════════════════════════════════════╗");
    println!("║     Hyperlicked Server v0.1.0          ║");
    println!("║     REST + WebSocket + Consensus       ║");
    println!("╚════════════════════════════════════════╝");
    println!();

    // Port: CLI arg > config
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(config.port);

    // Initialize storage (RocksDB or in-memory)
    let (store, initial_height, initial_view): (Option<Arc<RocksDbStore>>, u64, u64) =
        if let Some(ref data_dir) = config.data_dir {
            info!(path = %data_dir, "Opening persistent storage");
            let store = Arc::new(RocksDbStore::open(data_dir)?);

            // Recover state from storage
            let recovery = hyperlicked::storage::recover_from_storage(&*store)?;
            let height = recovery.consensus_state.committed_height;
            let view = recovery.consensus_state.current_view;

            info!(
                committed_height = height,
                current_view = view,
                snapshot_height = recovery.snapshot_height,
                "Recovered from storage"
            );

            (Some(store), height, view)
        } else {
            (None, 0, 0)
        };

    // Create app state (from recovery or fresh)
    let app_state = if let Some(ref store) = store {
        let recovery = hyperlicked::storage::recover_from_storage(&**store)?;

        // Start from snapshot
        let mut app = AppState::from_snapshot(recovery.snapshot);

        // Replay blocks since snapshot
        let blocks = hyperlicked::storage::recovery::get_blocks_to_replay(
            &**store,
            recovery.snapshot_height,
            recovery.consensus_state.committed_height,
        )?;

        for block in &blocks {
            app.execute(block);
        }

        if !blocks.is_empty() {
            info!(count = blocks.len(), "Replayed blocks from storage");
        }

        app
    } else {
        AppState::new()
    };

    // Enable oracle if configured (dev mode)
    let mut app_state = app_state;
    if config.oracle_enabled {
        app_state.oracle_mut().set_enabled(true);
        // Configure oracle for BTC-USDT with relaxed settings for dev
        // High deviation allowed because mark price starts at $50k but real BTC is ~$100k+
        app_state.oracle_mut().set_config("BTC-USDT", OracleConfig {
            max_staleness_ms: 10_000,   // 10 seconds (more lenient)
            min_sources: 1,              // Accept even 1 source in dev
            max_deviation_bps: 10000,    // 100% deviation allowed (dev mode only!)
            fallback_to_mark: true,
        });
        info!("Oracle system enabled via ORACLE_ENABLED env var");
    }

    let shared_state = SharedState::new(app_state);

    println!("Configuration:");
    println!("  Mode: {} {}", config.mode, if config.mode.is_dev() { "(faucet enabled)" } else { "" });
    println!("  Port: {}", port);
    println!("  Block time: {}ms", config.block_time_ms);
    println!("  Log blocks: {}", config.log_all_blocks);
    if config.mode.is_dev() {
        println!("  Faucet: ${:.2} per account", config.faucet_amount as f64 / 100.0);
    }
    if config.data_dir.is_some() {
        println!("  Storage: {} (snapshot every {} blocks)",
            config.data_dir.as_ref().unwrap(),
            config.snapshot_interval);
        println!("  Recovered: height={}, view={}", initial_height, initial_view);
    } else {
        println!("  Storage: in-memory (no persistence)");
    }
    println!("  Markets: BTC-USDT");
    println!("  Oracle: {}", if config.oracle_enabled { "enabled" } else { "disabled" });
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
    let consensus_store = store.clone();
    tokio::spawn(async move {
        run_consensus_loop(consensus_state, consensus_store, initial_height, initial_view).await;
    });

    // Spawn oracle price fetcher if enabled
    if config.oracle_enabled {
        let oracle_state = shared_state.clone();
        tokio::spawn(async move {
            run_oracle_fetcher(oracle_state).await;
        });
    }

    // Start server
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("Starting server on http://{}", addr);
    println!("────────────────────────────────────────");
    println!();
    println!("API v1 Endpoints:");
    println!("  GET  /api/v1/markets              - List markets");
    println!("  GET  /api/v1/markets/:sym         - Get market info");
    println!("  GET  /api/v1/markets/:sym/orderbook - Get orderbook");
    println!("  GET  /api/v1/markets/:sym/ctx     - Get market stats");
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
async fn run_consensus_loop(
    state: SharedState,
    store: Option<Arc<RocksDbStore>>,
    initial_height: u64,
    initial_view: u64,
) {
    let config = Config::global();
    let block_time_ms = config.block_time_ms;
    let log_all_blocks = config.log_all_blocks;
    let snapshot_interval = config.snapshot_interval;

    let mut height = initial_height;
    let mut view = initial_view;
    let mut last_snapshot_height = initial_height;
    let start_time = std::time::Instant::now();

    info!(
        block_time_ms,
        log_all_blocks,
        mode = %config.mode,
        initial_height,
        initial_view,
        persistent = store.is_some(),
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
            let (app_hash, fills, order_updates, deposits, adl_events, funding_events, liquidation_events) = {
                let mut app = state.app.write().await;
                // Reset daily stats at day boundary (before processing)
                app.maybe_reset_daily_stats(block.timestamp);
                let hash = app.execute(&block);
                let fills = app.take_pending_fills();
                let order_updates = app.take_pending_order_updates();
                let deposits = app.take_pending_deposits();
                let adl_events = app.take_pending_adl_events();
                let funding_events = app.take_pending_funding();
                let liquidation_events = app.take_pending_liquidations();
                (hash, fills, order_updates, deposits, adl_events, funding_events, liquidation_events)
            };
            let exec_time = exec_start.elapsed();

            // Record fill volumes for daily stats
            if !fills.is_empty() {
                let mut app = state.app.write().await;
                for fill in &fills {
                    app.record_fill_volume(&fill.symbol, fill.size, fill.price);
                }
            }

            // Persist block and consensus state (if storage enabled)
            if let Some(ref store) = store {
                let consensus_state = ConsensusState {
                    high_qc: None,      // Simplified: not tracking QCs in server mode
                    locked_qc: None,
                    voted_views: vec![], // Simplified: not tracking votes
                    current_view: view,
                    committed_height: height,
                    committed_hash: app_hash,
                };

                if let Err(e) = store.commit_block(&block, &consensus_state) {
                    tracing::error!(error = %e, "Failed to persist block");
                }

                // Periodic snapshot
                if snapshot_interval > 0 && height - last_snapshot_height >= snapshot_interval {
                    let snapshot = {
                        let app = state.app.read().await;
                        app.create_snapshot(height)
                    };
                    if let Err(e) = store.save_snapshot(height, &snapshot) {
                        tracing::error!(error = %e, "Failed to save snapshot");
                    } else {
                        last_snapshot_height = height;
                    }
                }
            }

            // Emit user events for fills
            for fill in &fills {
                let side = match fill.side {
                    hyperlicked::app::Side::Bid => "buy",
                    hyperlicked::app::Side::Ask => "sell",
                };

                // Get fees from config (simplified)
                let maker_fee = (fill.price * fill.size / 100_000_000) * 2 / 10000; // 0.02%
                let taker_fee = (fill.price * fill.size / 100_000_000) * 5 / 10000; // 0.05%

                state.users.notify_fill(
                    &fill.maker,
                    &fill.taker,
                    &fill.symbol,
                    &fill.maker_order_id,
                    side,
                    fill.price,
                    fill.size,
                    maker_fee,
                    taker_fee,
                    block.timestamp,
                ).await;

                // Also broadcast public trade event
                WebSocketHandler::broadcast_trade(
                    &state,
                    &fill.symbol,
                    fill.price,
                    fill.size,
                    side,
                    block.timestamp,
                );

                // Send position and balance updates to both maker and taker
                let app = state.app.read().await;
                let mark_price = app.mark_price(&fill.symbol).unwrap_or(0);

                // Maker position/balance update
                if let Some(account) = app.account(&fill.maker) {
                    let pos = account.position(&fill.symbol);
                    let unrealized_pnl = pos.unrealized_pnl(mark_price);
                    state.users.notify_position_update(
                        &fill.maker,
                        &fill.symbol,
                        pos.size,
                        pos.entry_price,
                        mark_price,
                        unrealized_pnl,
                        block.timestamp,
                    ).await;

                    state.users.notify_balance_update(
                        &fill.maker,
                        account.balance,
                        account.balance,
                        account.locked,
                        block.timestamp,
                    ).await;
                }

                // Taker position/balance update
                if let Some(account) = app.account(&fill.taker) {
                    let pos = account.position(&fill.symbol);
                    let unrealized_pnl = pos.unrealized_pnl(mark_price);
                    state.users.notify_position_update(
                        &fill.taker,
                        &fill.symbol,
                        pos.size,
                        pos.entry_price,
                        mark_price,
                        unrealized_pnl,
                        block.timestamp,
                    ).await;

                    state.users.notify_balance_update(
                        &fill.taker,
                        account.balance,
                        account.balance,
                        account.locked,
                        block.timestamp,
                    ).await;
                }
            }

            // Emit user events for order updates
            for order_update in &order_updates {
                state.users.notify_order_update(
                    &order_update.trader,
                    &order_update.order_id,
                    &order_update.symbol,
                    &order_update.status,
                    order_update.filled,
                    order_update.remaining,
                    block.timestamp,
                ).await;
            }

            // Emit user events for deposits (balance updates)
            for deposit in &deposits {
                let app = state.app.read().await;
                if let Some(account) = app.account(&deposit.trader) {
                    state.users.notify_balance_update(
                        &deposit.trader,
                        account.balance,
                        account.balance, // available = balance (simplified)
                        account.locked,
                        block.timestamp,
                    ).await;
                }
            }

            // Emit user events for ADL (auto-deleveraging)
            for adl_event in &adl_events {
                state.users.notify_adl(
                    &adl_event.address,
                    &adl_event.symbol,
                    adl_event.size_reduced,
                    adl_event.close_price,
                    adl_event.realized_pnl,
                    &adl_event.triggering_liquidation,
                    adl_event.timestamp,
                ).await;

                // Also send position update after ADL
                let app = state.app.read().await;
                if let Some(account) = app.account(&adl_event.address) {
                    let pos = account.position(&adl_event.symbol);
                    let mark_price = app.mark_price(&adl_event.symbol).unwrap_or(0);
                    let unrealized_pnl = pos.unrealized_pnl(mark_price);
                    state.users.notify_position_update(
                        &adl_event.address,
                        &adl_event.symbol,
                        pos.size,
                        pos.entry_price,
                        mark_price,
                        unrealized_pnl,
                        block.timestamp,
                    ).await;
                }
            }

            // Emit user events for funding payments
            for funding_result in &funding_events {
                for user_payment in &funding_result.user_payments {
                    state.users.notify_funding_payment(
                        &user_payment.address,
                        &user_payment.symbol,
                        user_payment.payment,
                        user_payment.position_size,
                        user_payment.funding_rate_bps,
                        user_payment.timestamp,
                    ).await;

                    // Also send balance update after funding
                    let app = state.app.read().await;
                    if let Some(account) = app.account(&user_payment.address) {
                        state.users.notify_balance_update(
                            &user_payment.address,
                            account.balance,
                            account.balance,
                            account.locked,
                            block.timestamp,
                        ).await;
                    }
                }
            }

            // Emit user events for liquidations
            for liq in &liquidation_events {
                state.users.notify_liquidated(
                    &liq.address,
                    &liq.symbol,
                    liq.size,
                    liq.price,
                    liq.pnl,
                    liq.was_long,
                    block.timestamp,
                ).await;

                // Send position update (position closed) and balance update
                let app = state.app.read().await;
                if let Some(account) = app.account(&liq.address) {
                    let pos = account.position(&liq.symbol);
                    let mark_price = app.mark_price(&liq.symbol).unwrap_or(0);
                    let unrealized_pnl = pos.unrealized_pnl(mark_price);
                    state.users.notify_position_update(
                        &liq.address,
                        &liq.symbol,
                        pos.size,
                        pos.entry_price,
                        mark_price,
                        unrealized_pnl,
                        block.timestamp,
                    ).await;

                    state.users.notify_balance_update(
                        &liq.address,
                        account.balance,
                        account.balance,
                        account.locked,
                        block.timestamp,
                    ).await;
                }
            }

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

            // Broadcast mark price update when there are trades
            if !fills.is_empty() {
                let (mark_price, index_price) = {
                    let app = state.app.read().await;
                    let mark = app.mark_price("BTC-USDT").unwrap_or(0);
                    let index = app.oracle_price("BTC-USDT");
                    (mark, index)
                };

                WebSocketHandler::broadcast_mark_price(
                    &state,
                    "BTC-USDT",
                    mark_price,
                    index_price,
                    block.timestamp,
                );
            }

            // Broadcast asset context (market stats) every block
            {
                let app = state.app.read().await;
                let symbol = "BTC-USDT";

                // Convert funding rate from bps to 1/1M units (multiply by 100)
                let funding_rate_bps = app.funding_rate(symbol);
                let funding_rate_1m = funding_rate_bps * 100;

                let ctx = AssetCtxData {
                    symbol: symbol.to_string(),
                    mark_price: app.mark_price(symbol).unwrap_or(0),
                    oracle_price: app.oracle_price(symbol),
                    mid_price: app.mid_price(symbol).unwrap_or(app.mark_price(symbol).unwrap_or(0)),
                    funding_rate: funding_rate_1m,
                    premium: app.premium(symbol).unwrap_or(0),
                    open_interest: app.get_open_interest(symbol),
                    prev_day_price: app.prev_day_price(symbol).unwrap_or(0),
                    day_volume: app.day_volume(symbol),
                    day_notional_volume: app.day_notional_volume(symbol),
                    next_funding_time: app.next_funding_time(symbol),
                    timestamp: block.timestamp,
                };

                WebSocketHandler::broadcast_asset_ctx(&state, ctx);
            }

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
                info!(
                    height,
                    view,
                    hash = %hex::encode(&app_hash[..4]),
                    "💓 Heartbeat block"
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

/// Run oracle price fetcher loop
/// Polls external CEXs and updates oracle prices
async fn run_oracle_fetcher(state: SharedState) {
    let config = FetcherConfig::default();
    let fetcher = OracleFetcher::new(config.clone());

    info!(
        poll_interval_ms = config.poll_interval_ms,
        symbols = ?config.symbols,
        "Oracle fetcher started"
    );

    loop {
        // Fetch prices for each symbol
        for symbol in &config.symbols {
            tracing::debug!(symbol = %symbol, "Fetching oracle prices...");
            let sources = fetcher.fetch_prices(symbol).await;

            if sources.is_empty() {
                tracing::warn!(symbol = %symbol, "No oracle prices fetched");
                continue;
            }

            tracing::info!(
                symbol = %symbol,
                sources = sources.len(),
                "Fetched {} price sources",
                sources.len()
            );

            // Aggregate and update oracle state
            if let Some(oracle_price) = fetcher.aggregate(symbol, &sources) {
                let mut app = state.app.write().await;
                let mark_price = app.mark_price(symbol);

                // Update oracle with fetched prices
                let result = app.oracle_mut().process_update(
                    symbol,
                    sources.clone(),
                    oracle_price.timestamp,
                    mark_price,
                );

                match result {
                    Ok(()) => {
                        tracing::info!(
                            symbol = %symbol,
                            price = oracle_price.price,
                            price_usd = format!("${:.2}", oracle_price.price as f64 / 100.0),
                            sources = oracle_price.source_count,
                            "🔮 Oracle price updated"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            symbol = %symbol,
                            error = %e,
                            "Failed to update oracle price"
                        );
                    }
                }
            }
        }

        // Wait for next poll interval
        tokio::time::sleep(Duration::from_millis(config.poll_interval_ms)).await;
    }
}
