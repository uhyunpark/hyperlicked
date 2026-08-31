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
mod adl;
mod admin;
mod chain;
mod market;
mod oracle;
mod order;
mod staking;
pub mod sync;
mod transactions;
mod trigger;

use std::sync::Arc;

use axum::{
    middleware,
    routing::{get, post},
    Json, Router,
};

use super::handlers::{deposit, register_delegation, submit_order_legacy, withdraw};
use super::rate_limit::{
    rate_limit_heavy, rate_limit_read, rate_limit_trading, RateLimiter, SharedRateLimiter,
};
use super::state::SharedState;
use super::types::{ApiSecurityPolicy, ApiState};
use crate::config::Mode;
use crate::storage::PersistentStore;

pub fn create_router(state: SharedState) -> Router {
    create_router_with_mode(state, Mode::from_env())
}

/// Build the API router for an explicit runtime mode.
///
/// Mode is an input to the router so callers and tests can select the
/// production surface without relying on process-global configuration.
pub fn create_router_with_mode(state: SharedState, mode: Mode) -> Router {
    create_router_with_store_and_mode(state, None, mode)
}

pub fn create_router_with_store(
    state: SharedState,
    store: Option<Arc<dyn PersistentStore + Send + Sync>>,
) -> Router {
    create_router_with_store_and_mode(state, store, Mode::from_env())
}

/// Build the API router with an explicit runtime mode and optional store.
pub fn create_router_with_store_and_mode(
    state: SharedState,
    store: Option<Arc<dyn PersistentStore + Send + Sync>>,
    mode: Mode,
) -> Router {
    let security_policy = ApiSecurityPolicy::for_mode(mode);
    let api_state = match store {
        Some(s) => ApiState::with_store_and_policy(state, s, security_policy),
        None => ApiState::with_policy(state, security_policy),
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
        .route(
            "/accounts/:address/funding",
            get(account::get_account_funding),
        )
        .route("/accounts/:address/fills", get(account::get_user_fills))
        .route(
            "/accounts/:address/trigger-orders",
            get(trigger::get_trigger_orders),
        )
        // Finalized transaction receipts
        .route("/transactions/:tx_hash", get(transactions::get_transaction))
        // Chain endpoints
        .route("/chain/status", get(chain::get_chain_status))
        .route("/chain/health", get(chain::get_node_health))
        .route("/chain/insurance-fund", get(chain::get_insurance_fund))
        // Staking endpoints
        .route("/staking/validators", get(staking::get_validators))
        .route("/staking/validators/:operator", get(staking::get_validator))
        .route(
            "/staking/delegations/:address",
            get(staking::get_delegations),
        )
        .route(
            "/staking/unstakes/:address",
            get(staking::get_pending_unstakes),
        )
        .route(
            "/staking/summary/:address",
            get(staking::get_staking_summary),
        )
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
        .route(
            "/trigger-orders/cancel",
            post(trigger::cancel_trigger_order),
        )
        // Agent delegation
        .route("/delegations", post(register_delegation))
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

    // Simulated balance mutations are intentionally available only to local
    // development nodes. Testnet and mainnet must use future authenticated
    // deposit/withdraw transaction paths instead of these handlers.
    // Heavy endpoints (20 req/min) - sync, snapshots
    let heavy_routes = Router::new()
        .route("/sync/status", get(sync::get_sync_status))
        .route("/sync/blocks", get(sync::get_blocks))
        .route("/sync/block/:height", get(sync::get_block_by_height))
        .route("/sync/finality/:height", get(sync::get_finality_proof))
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

    let api_v1 = if mode.is_dev() {
        api_v1.merge(
            Router::new()
                .route("/deposit", post(deposit))
                .route("/withdraw", post(withdraw))
                .with_state(api_state.clone())
                .layer(middleware::from_fn_with_state(
                    rate_limiter.clone(),
                    rate_limit_trading,
                )),
        )
    } else {
        api_v1
    };

    // Legacy mutation routes are a dev-only compatibility surface.
    let legacy_read = Router::new()
        .route("/api/orderbook/:symbol", get(market::get_orderbook))
        .route("/api/account/:address", get(account::get_account))
        .with_state(api_state.clone())
        .layer(middleware::from_fn_with_state(
            rate_limiter.clone(),
            rate_limit_read,
        ));

    let mut router = Router::new()
        // Health check (no rate limit)
        .route("/health", get(health))
        // API v1 routes
        .nest("/api/v1", api_v1)
        // WebSocket (no rate limit - has its own auth)
        .route("/ws", get(ws_handler_wrapped))
        .merge(legacy_read);

    if mode.is_dev() {
        let legacy_trading = Router::new()
            .route("/api/order", post(submit_order_legacy))
            .route("/api/deposit", post(deposit))
            .route("/api/withdraw", post(withdraw))
            .with_state(api_state.clone())
            .layer(middleware::from_fn_with_state(
                rate_limiter,
                rate_limit_trading,
            ));
        router = router.merge(legacy_trading);
    }

    router.with_state(api_state)
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{Request, StatusCode},
    };
    use std::future::poll_fn;
    use std::net::SocketAddr;
    use tower::Service;

    fn test_router(mode: Mode) -> Router {
        let state = SharedState::new(crate::app::AppState::new());
        create_router_with_mode(state, mode)
    }

    async fn post_status(router: Router, path: &str) -> StatusCode {
        let mut router = router;
        poll_fn(|cx| <Router as Service<Request<Body>>>::poll_ready(&mut router, cx))
            .await
            .expect("router should become ready");
        let mut request = Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));
        let response = router
            .call(request)
            .await
            .expect("router request should complete");
        response.status()
    }

    #[tokio::test]
    async fn dev_router_mounts_legacy_and_simulated_mutations() {
        let paths = [
            "/api/v1/deposit",
            "/api/v1/withdraw",
            "/api/order",
            "/api/deposit",
            "/api/withdraw",
        ];

        for path in paths {
            assert_ne!(
                post_status(test_router(Mode::Dev), path).await,
                StatusCode::NOT_FOUND,
                "dev route should be mounted: {path}"
            );
        }
    }

    #[tokio::test]
    async fn production_routers_hide_legacy_and_simulated_mutations() {
        let paths = [
            "/api/v1/deposit",
            "/api/v1/withdraw",
            "/api/order",
            "/api/deposit",
            "/api/withdraw",
        ];

        for mode in [Mode::Testnet, Mode::Mainnet] {
            for path in paths {
                assert_eq!(
                    post_status(test_router(mode), path).await,
                    StatusCode::NOT_FOUND,
                    "non-dev route should be hidden in {mode}: {path}"
                );
            }
        }
    }
}
