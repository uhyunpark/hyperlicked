//! API Layer
//!
//! Exposes the exchange via REST and WebSocket.
//!
//! ## Endpoints
//!
//! ### REST
//! - `POST /api/order` - Place order
//! - `DELETE /api/order/:id` - Cancel order
//! - `GET /api/orderbook/:symbol` - Get L2 orderbook
//! - `GET /api/account/:address` - Get account info
//! - `POST /api/deposit` - Deposit funds
//! - `POST /api/withdraw` - Withdraw funds
//!
//! ### WebSocket
//! - `WS /ws` - Real-time updates (orderbook, fills, positions)
//!
//! ## Rate Limiting (CRITICAL-5)
//!
//! IP-based rate limiting is applied per endpoint type:
//! - Trading endpoints: 100 req/min
//! - Read endpoints: 1000 req/min
//! - Heavy endpoints (sync): 20 req/min

mod canonical;
mod handlers;
pub mod rate_limit;
mod routes;
pub mod state;
pub(crate) mod transaction;
pub mod types;
mod verify;
mod websocket;

pub use canonical::CanonicalAppHook;
pub use rate_limit::{RateLimiter, SharedRateLimiter};
pub use routes::{
    create_router, create_router_with_mode, create_router_with_store,
    create_router_with_store_and_mode,
};
pub use state::{
    run_user_transaction_rebroadcast, SharedState, MAX_USER_TRANSACTION_REBROADCAST_BATCH,
    MAX_USER_TRANSACTION_REBROADCAST_BYTES, USER_TRANSACTION_REBROADCAST_INTERVAL,
};
pub use types::{ApiSecurityPolicy, ApiState};
pub use websocket::{AssetCtxData, WebSocketHandler};
