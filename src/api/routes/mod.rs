//! REST API Routes
//!
//! All REST endpoints for the exchange, organized by domain.

mod account;
mod adl;
mod chain;
mod market;
mod oracle;
mod order;
mod staking;
mod trigger;

use axum::{
    routing::{get, post},
    Json, Router,
};

use super::handlers::{deposit, register_delegation, submit_order_legacy, withdraw};
use super::state::SharedState;
use super::types::ApiState;

pub fn create_router(state: SharedState) -> Router {
    let api_state = ApiState::new(state);

    let api_v1 = Router::new()
        // Market endpoints
        .route("/markets", get(market::get_markets))
        .route("/markets/:symbol", get(market::get_market))
        .route("/markets/:symbol/orderbook", get(market::get_orderbook))
        .route("/markets/:symbol/trades", get(market::get_trades))
        .route("/markets/:symbol/candles", get(market::get_candles))
        .route("/markets/:symbol/funding", get(market::get_funding))
        // Account endpoints
        .route("/accounts/:address", get(account::get_account))
        .route("/accounts/:address/positions", get(account::get_positions))
        .route("/accounts/:address/orders", get(account::get_orders))
        .route("/accounts/:address/nonce", get(account::get_nonce))
        .route("/accounts/:address/funding", get(account::get_account_funding))
        .route(
            "/accounts/:address/trigger-orders",
            get(trigger::get_trigger_orders),
        )
        // Chain endpoints
        .route("/chain/status", get(chain::get_chain_status))
        .route("/chain/insurance-fund", get(chain::get_insurance_fund))
        // Staking endpoints
        .route("/staking/validators", get(staking::get_validators))
        .route("/staking/validators/:operator", get(staking::get_validator))
        .route("/staking/delegations/:address", get(staking::get_delegations))
        .route("/staking/epoch", get(staking::get_epoch))
        // Oracle endpoints
        .route("/oracle/status", get(oracle::get_oracle_status))
        .route("/oracle/:symbol", get(oracle::get_oracle_price))
        .route("/oracle/:symbol/sources", get(oracle::get_oracle_sources))
        .route("/oracle/submit", post(oracle::submit_oracle_update))
        // Order submission
        .route("/orders", post(order::submit_order))
        .route("/orders/cancel", post(order::cancel_order))
        // Trigger orders (Stop Loss / Take Profit)
        .route("/trigger-orders", post(trigger::place_trigger_order))
        .route(
            "/trigger-orders/:id",
            axum::routing::delete(trigger::cancel_trigger_order_by_id),
        )
        .route("/trigger-orders/cancel", post(trigger::cancel_trigger_order))
        // Agent delegation
        .route("/delegations", post(register_delegation))
        // Deposit/Withdraw
        .route("/deposit", post(deposit))
        .route("/withdraw", post(withdraw))
        // ADL history
        .route("/adl/history", get(adl::get_adl_history));

    Router::new()
        // Health check
        .route("/health", get(health))
        // API v1 routes
        .nest("/api/v1", api_v1)
        // WebSocket (legacy route too)
        .route("/ws", get(ws_handler_wrapped))
        // Legacy routes for compatibility
        .route("/api/order", post(submit_order_legacy))
        .route("/api/orderbook/:symbol", get(market::get_orderbook))
        .route("/api/account/:address", get(account::get_account))
        .route("/api/deposit", post(deposit))
        .route("/api/withdraw", post(withdraw))
        // State
        .with_state(api_state)
}

// =============================================================================
// WebSocket wrapper
// =============================================================================

async fn ws_handler_wrapped(
    ws: axum::extract::ws::WebSocketUpgrade,
    axum::extract::State(state): axum::extract::State<ApiState>,
) -> axum::response::Response {
    ws.on_upgrade(|socket| super::websocket::handle_socket(socket, state.shared))
}

// =============================================================================
// Health Check
// =============================================================================

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}
