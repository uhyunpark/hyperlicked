//! Application Layer
//!
//! Handles the business logic of the exchange:
//! - Orderbook matching
//! - Account/position management
//! - Transaction ordering (mempool)
//!
//! ## Architecture
//!
//! ```text
//! Transactions → Mempool → Block → Execution → State Update
//!                  │                    │
//!                  │                    ├── Orderbook (matching)
//!                  │                    ├── Accounts (positions)
//!                  │                    └── Liquidation
//!                  │
//!                  └── 3-bucket ordering:
//!                      1. Non-order txs (deposits, withdrawals)
//!                      2. Cancels
//!                      3. Orders (GTC, IOC, ALO)
//! ```

pub mod accounts;
pub mod candles;
pub mod funding;
pub mod liquidation;
pub mod mempool;
pub mod orderbook;
pub mod state;

pub use accounts::{Account, AccountManager, Position};
pub use candles::{Candle, CandleManager, Interval};
pub use funding::FundingResult;
pub use liquidation::LiquidationResult;
pub use mempool::Mempool;
pub use orderbook::{Fill, Order, OrderBook, OrderId, OrderType, Side};
pub use state::{AppState, DepositInfo, OrderUpdateInfo, MAINTENANCE_MARGIN_BPS};

use crate::types::{Price, Size};

/// Address type (simplified - would be [u8; 20] for Ethereum compatibility)
pub type Address = String;

/// Symbol for a trading pair (e.g., "BTC-USDT")
pub type Symbol = String;

/// Transaction types in the system
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Transaction {
    /// Place an order
    PlaceOrder {
        trader: Address,
        symbol: Symbol,
        side: Side,
        price: Price,
        size: Size,
        order_type: OrderType,
        reduce_only: bool,
    },
    /// Cancel an order
    CancelOrder {
        trader: Address,
        order_id: OrderId,
    },
    /// Deposit collateral
    Deposit {
        trader: Address,
        amount: i64,
    },
    /// Withdraw collateral
    Withdraw {
        trader: Address,
        amount: i64,
    },
}

impl Transaction {
    /// Get the bucket for mempool ordering
    /// Lower bucket = higher priority
    pub fn bucket(&self) -> u8 {
        match self {
            Transaction::Deposit { .. } | Transaction::Withdraw { .. } => 0,
            Transaction::CancelOrder { .. } => 1,
            Transaction::PlaceOrder { .. } => 2,
        }
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
    }
}

/// Market configuration
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MarketConfig {
    pub symbol: Symbol,
    pub tick_size: Price,      // Minimum price increment
    pub lot_size: Size,        // Minimum size increment
    pub min_notional: i64,     // Minimum order value
    pub maker_fee: i64,        // Fee in basis points (e.g., 2 = 0.02%)
    pub taker_fee: i64,        // Fee in basis points
    // Funding parameters
    pub funding_interval_ms: u64,  // Funding interval in ms (3600000 = 1 hour)
    pub interest_rate_bps: i64,    // Interest rate component (1 = 0.01%)
    pub max_funding_rate_bps: i64, // Max funding rate cap (400 = 4%)
}

impl Default for MarketConfig {
    fn default() -> Self {
        Self {
            symbol: "BTC-USDT".to_string(),
            tick_size: 1,                // 1 cent
            lot_size: 1,                 // 1 satoshi
            min_notional: 10_00,         // $10 minimum
            maker_fee: 2,                // 0.02%
            taker_fee: 5,                // 0.05%
            funding_interval_ms: 3600000, // 1 hour
            interest_rate_bps: 1,        // 0.01% interest
            max_funding_rate_bps: 400,   // 4% max hourly rate
        }
    }
}
