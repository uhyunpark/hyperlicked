//! REST API Routes
//!
//! All REST endpoints for the exchange, organized by domain.
//!
//! ## Rate Limiting (CRITICAL-5)
//!
//! Rate limiting is applied by endpoint type:
//! - Trading: 100 req/min (orders, cancels, deposits, withdrawals)
//! - Read: 1000 req/min (orderbook, account, market data)
//! - Heavy: 20 req/min (sync, snapshots)

mod account;
mod admin;
mod adl;
mod chain;
mod market;
mod oracle;
mod order;
mod staking;
pub mod sync;
mod trigger;

use std::sync::Arc;

use axum::{
    middleware,
    routing::{get, post},
    Json, Router,
};

use super::handlers::{deposit, register_delegation, submit_order_legacy, withdraw};
use super::rate_limit::{rate_limit_heavy, rate_limit_read, rate_limit_trading, RateLimiter, SharedRateLimiter};
use super::state::SharedState;
use super::types::ApiState;
use crate::storage::PersistentStore;

pub fn create_router(state: SharedState) -> Router {
    create_router_with_store(state, None)
}

pub fn create_router_with_store(
    state: SharedState,
    store: Option<Arc<dyn PersistentStore + Send + Sync>>,
) -> Router {
    let api_state = match store {
        Some(s) => ApiState::with_store(state, s),
        None => ApiState::new(state),
    };

    // Create shared rate limiter (CRITICAL-5)
    let rate_limiter: SharedRateLimiter = Arc::new(RateLimiter::new());

    // Read endpoints (1000 req/min) - market data, account info
    let read_routes = Router::new()
        // Market endpoints
        .route("/markets", get(market::get_markets))
        .route("/markets/:symbol", get(market::get_market))
        .route("/markets/:symbol/orderbook", get(market::get_orderbook))
        .route("/markets/:symbol/trades", get(market::get_trades))
        .route("/markets/:symbol/candles", get(market::get_candles))
        .route("/markets/:symbol/funding", get(market::get_funding))
        .route("/markets/:symbol/ctx", get(market::get_asset_ctx))
        // Account endpoints
        .route("/accounts/:address", get(account::get_account))
        .route("/accounts/:address/positions", get(account::get_positions))
        .route("/accounts/:address/orders", get(account::get_orders))
        .route("/accounts/:address/nonce", get(account::get_nonce))
        .route("/accounts/:address/funding", get(account::get_account_funding))
        .route("/accounts/:address/fills", get(account::get_user_fills))
        .route(
            "/accounts/:address/trigger-orders",
            get(trigger::get_trigger_orders),
        )
        // Chain endpoints
        .route("/chain/status", get(chain::get_chain_status))
        .route("/chain/health", get(chain::get_node_health))
        .route("/chain/insurance-fund", get(chain::get_insurance_fund))
        // Staking endpoints
        .route("/staking/validators", get(staking::get_validators))
        .route("/staking/validators/:operator", get(staking::get_validator))
        .route("/staking/delegations/:address", get(staking::get_delegations))
        .route("/staking/unstakes/:address", get(staking::get_pending_unstakes))
        .route("/staking/summary/:address", get(staking::get_staking_summary))
        .route("/staking/epoch", get(staking::get_epoch))
        // Oracle read endpoints
        .route("/oracle/status", get(oracle::get_oracle_status))
        .route("/oracle/:symbol", get(oracle::get_oracle_price))
        .route("/oracle/:symbol/sources", get(oracle::get_oracle_sources))
        // ADL history
        .route("/adl/history", get(adl::get_adl_history))
        .with_state(api_state.clone())
        .layer(middleware::from_fn_with_state(
            rate_limiter.clone(),
            rate_limit_read,
        ));

    // Trading endpoints (100 req/min) - orders, cancels, deposits
    let trading_routes = Router::new()
        // Order submission
        .route("/orders", post(order::submit_order))
        .route("/orders/cancel", post(order::cancel_order))
        // Trigger orders (Stop Loss / Take Profit)
        .route("/trigger-orders", post(trigger::place_trigger_order))
        .route("/trigger-orders/cancel", post(trigger::cancel_trigger_order))
        // Agent delegation
        .route("/delegations", post(register_delegation))
        // Deposit/Withdraw
        .route("/deposit", post(deposit))
        .route("/withdraw", post(withdraw))
        // Oracle submit (admin action, but rate limited with trading)
        .route("/oracle/submit", post(oracle::submit_oracle_update))
        .route("/oracle/enable", post(oracle::set_oracle_enabled))
        // Admin
        .route("/admin/add-market", post(admin::add_market))
        .with_state(api_state.clone())
        .layer(middleware::from_fn_with_state(
            rate_limiter.clone(),
            rate_limit_trading,
        ));

    // Heavy endpoints (20 req/min) - sync, snapshots
    let heavy_routes = Router::new()
        .route("/sync/status", get(sync::get_sync_status))
        .route("/sync/blocks", get(sync::get_blocks))
        .route("/sync/block/:height", get(sync::get_block_by_height))
        .route("/sync/snapshot/latest", get(sync::get_latest_snapshot))
        .route("/sync/snapshot/:height", get(sync::get_snapshot))
        .with_state(api_state.clone())
        .layer(middleware::from_fn_with_state(
            rate_limiter.clone(),
            rate_limit_heavy,
        ));

    // Merge all API v1 routes
    let api_v1 = Router::new()
        .merge(read_routes)
        .merge(trading_routes)
        .merge(heavy_routes);

    // Legacy routes (also rate limited)
    let legacy_trading = Router::new()
        .route("/api/order", post(submit_order_legacy))
        .route("/api/deposit", post(deposit))
        .route("/api/withdraw", post(withdraw))
        .with_state(api_state.clone())
        .layer(middleware::from_fn_with_state(
            rate_limiter.clone(),
            rate_limit_trading,
        ));

    let legacy_read = Router::new()
        .route("/api/orderbook/:symbol", get(market::get_orderbook))
        .route("/api/account/:address", get(account::get_account))
        .with_state(api_state.clone())
        .layer(middleware::from_fn_with_state(
            rate_limiter,
            rate_limit_read,
        ));

    Router::new()
        // Health check (no rate limit)
        .route("/health", get(health))
        // API v1 routes
        .nest("/api/v1", api_v1)
        // WebSocket (no rate limit - has its own auth)
        .route("/ws", get(ws_handler_wrapped))
        // Legacy routes
        .merge(legacy_trading)
        .merge(legacy_read)
        .with_state(api_state)
}

// =============================================================================
// WebSocket wrapper
// =============================================================================

async fn ws_handler_wrapped(
    ws: axum::extract::ws::WebSocketUpgrade,
    axum::extract::State(state): axum::extract::State<ApiState>,
) -> axum::response::Response {
    // Pass full ApiState so websocket can check agent delegations for auth
    ws.on_upgrade(|socket| super::websocket::handle_socket(socket, state))
}

// =============================================================================
// Health Check
// =============================================================================

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}
