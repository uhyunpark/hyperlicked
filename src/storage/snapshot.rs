//! App State Snapshots
//!
//! Serializable snapshots of application state for persistence.
//!
//! ## What's Snapshotted
//! - Account balances and positions
//! - Market configurations
//! - Mark prices
//!
//! ## What's NOT Snapshotted (rebuilt from block replay)
//! - Orderbook open orders
//! - Mempool pending transactions
//! - Trade history (recent trades)

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::app::{Account, MarketConfig, Symbol};
use crate::types::Price;

/// Serializable app state snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSnapshot {
    /// Block height at snapshot
    pub height: u64,
    /// Block timestamp at snapshot
    pub timestamp: u64,
    /// All accounts with balances and positions
    pub accounts: Vec<Account>,
    /// Market configurations
    pub market_configs: Vec<MarketConfig>,
    /// Mark prices per symbol
    pub mark_prices: Vec<(Symbol, Price)>,
    /// Insurance fund balance (in cents)
    #[serde(default)]
    pub insurance_fund: i64,
    /// Current funding rates per symbol (in bps)
    #[serde(default)]
    pub funding_rates: Vec<(Symbol, i64)>,
    /// Last funding payment times per symbol (ms timestamp)
    #[serde(default)]
    pub last_funding_times: Vec<(Symbol, u64)>,
}

impl AppSnapshot {
    /// Create empty genesis snapshot
    pub fn genesis() -> Self {
        Self {
            height: 0,
            timestamp: 0,
            accounts: Vec::new(),
            market_configs: vec![MarketConfig::default()],
            mark_prices: vec![("BTC-USDT".to_string(), 5_000_000)],
            insurance_fund: 0,
            funding_rates: Vec::new(),
            last_funding_times: Vec::new(),
        }
    }

    /// Convert mark_prices vec to HashMap for use
    pub fn mark_prices_map(&self) -> HashMap<Symbol, Price> {
        self.mark_prices.iter().cloned().collect()
    }

    /// Convert funding_rates vec to HashMap for use
    pub fn funding_rates_map(&self) -> HashMap<Symbol, i64> {
        self.funding_rates.iter().cloned().collect()
    }

    /// Convert last_funding_times vec to HashMap for use
    pub fn last_funding_times_map(&self) -> HashMap<Symbol, u64> {
        self.last_funding_times.iter().cloned().collect()
    }
}
