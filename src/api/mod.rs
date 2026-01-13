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

mod handlers;
mod routes;
pub mod state;
pub mod types;
mod websocket;

pub use routes::create_router;
pub use state::SharedState;
pub use types::ApiState;
pub use websocket::WebSocketHandler;
