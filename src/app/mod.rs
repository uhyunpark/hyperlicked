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
pub mod adl;
pub mod candles;
pub mod funding;
pub mod liquidation;
pub mod liquidation_queue;
pub mod market_maker;
pub mod mempool;
pub mod oracle;
pub mod orderbook;
pub mod positions;
pub mod staking;
pub mod state;
pub mod trigger;

pub use accounts::{Account, AccountManager};
pub use positions::Position;
pub use candles::{Candle, CandleManager, Interval};
pub use funding::{FundingError, FundingResult};
pub use liquidation::{LiquidationError, LiquidationResult};
pub use liquidation_queue::LiquidationQueue;
pub use adl::{ADLError, ADLResult, ADLSummary};
pub use mempool::Mempool;
pub use orderbook::{Fill, Order, OrderBook, OrderId, OrderType, Side};
pub use staking::{
    StakingError, StakingState, StakingTransaction, StakingTxResult,
    ValidatorInfo, ValidatorStatus, Delegation, EpochSnapshot,
};
pub use oracle::{OracleConfig, OracleError, OraclePrice, OracleState, PriceSource};
pub use market_maker::{MarketMakerConfig, MarketMakerState, Intensity as MMIntensity};
pub use state::{AppError, AppState, DepositInfo, OrderUpdateInfo, MAINTENANCE_MARGIN_BPS};
pub use trigger::{
    Cloid, TriggerCondition, TriggerError, TriggerEvent, TriggerEventType,
    TriggerOrder, TriggerOrderId, TriggerOrderStatus, TriggerType,
};

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
    /// Register a new validator
    RegisterValidator {
        operator: Address,
        node_id: crate::types::NodeId,
        bls_pubkey: Vec<u8>,
        self_stake: i64,
        commission_bps: i64,
    },
    /// Delegate stake to a validator
    Delegate {
        delegator: Address,
        validator: Address,
        amount: i64,
    },
    /// Undelegate stake from a validator
    Undelegate {
        delegator: Address,
        validator: Address,
        amount: i64,
    },
    /// Claim completed unstakes
    ClaimUnstaked {
        delegator: Address,
    },
    /// Claim staking rewards
    ClaimRewards {
        claimant: Address,
        validator: Option<Address>,
    },
    /// Unjail a jailed validator
    Unjail {
        operator: Address,
    },
    /// Submit evidence of misbehavior
    SubmitEvidence {
        submitter: Address,
        evidence: staking::Evidence,
    },
    /// Place a trigger order (Stop Loss or Take Profit)
    PlaceTriggerOrder {
        trader: Address,
        symbol: Symbol,
        trigger_type: TriggerType,
        trigger_price: Price,
        size: Size,
        limit_price: Option<Price>,
        cloid: Option<Cloid>,
    },
    /// Cancel a trigger order by ID
    CancelTriggerOrder {
        trader: Address,
        trigger_order_id: TriggerOrderId,
    },
    /// Cancel a trigger order by client order ID
    CancelTriggerOrderByCloid {
        trader: Address,
        symbol: Symbol,
        cloid: Cloid,
    },
    /// Update oracle prices (authorized validators only)
    OraclePriceUpdate {
        operator: Address,
        symbol: Symbol,
        sources: Vec<oracle::PriceSource>,
        signature: Vec<u8>,
    },
}

impl Transaction {
    /// Get the bucket for mempool ordering
    /// Lower bucket = higher priority
    pub fn bucket(&self) -> u8 {
        match self {
            // Highest priority: deposits, withdrawals, staking, and oracle operations
            Transaction::Deposit { .. }
            | Transaction::Withdraw { .. }
            | Transaction::RegisterValidator { .. }
            | Transaction::Delegate { .. }
            | Transaction::Undelegate { .. }
            | Transaction::ClaimUnstaked { .. }
            | Transaction::ClaimRewards { .. }
            | Transaction::Unjail { .. }
            | Transaction::SubmitEvidence { .. }
            | Transaction::OraclePriceUpdate { .. } => 0,
            // Medium priority: order cancellations (including trigger cancels)
            Transaction::CancelOrder { .. }
            | Transaction::CancelTriggerOrder { .. }
            | Transaction::CancelTriggerOrderByCloid { .. } => 1,
            // Lower priority: order placements (including trigger orders)
            Transaction::PlaceOrder { .. }
            | Transaction::PlaceTriggerOrder { .. } => 2,
        }
    }

    /// Get the symbol for symbol-scoped transactions (for parallel execution)
    /// Returns None for global transactions (deposits, withdrawals, staking, etc.)
    pub fn symbol(&self) -> Option<&Symbol> {
        match self {
            Transaction::PlaceOrder { symbol, .. } => Some(symbol),
            Transaction::PlaceTriggerOrder { symbol, .. } => Some(symbol),
            Transaction::CancelTriggerOrderByCloid { symbol, .. } => Some(symbol),
            Transaction::OraclePriceUpdate { symbol, .. } => Some(symbol),
            // Global transactions - affect shared state
            Transaction::Deposit { .. }
            | Transaction::Withdraw { .. }
            | Transaction::CancelOrder { .. } // Order ID not symbol-scoped
            | Transaction::CancelTriggerOrder { .. } // Trigger ID not symbol-scoped
            | Transaction::RegisterValidator { .. }
            | Transaction::Delegate { .. }
            | Transaction::Undelegate { .. }
            | Transaction::ClaimUnstaked { .. }
            | Transaction::ClaimRewards { .. }
            | Transaction::Unjail { .. }
            | Transaction::SubmitEvidence { .. } => None,
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
