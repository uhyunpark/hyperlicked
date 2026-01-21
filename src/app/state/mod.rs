//! Application State
//!
//! Integrates orderbook, accounts, and mempool into a single
//! AppHook implementation for consensus.

mod consensus;
mod execution;
mod trigger;

use std::collections::{HashMap, VecDeque};

use crate::app::{
    accounts::AccountManager,
    candles::{Candle, CandleManager, Interval},
    mempool::{Mempool, MempoolError},
    orderbook::{Fill, Order, OrderBook},
    staking::StakingState,
    trigger::{Cloid, TriggerEvent, TriggerOrder, TriggerOrderId},
    Address, MarketConfig, Symbol, Transaction,
};
use crate::types::{Hash, Price, View};

/// Maximum trades stored per symbol
pub const MAX_TRADES_PER_SYMBOL: usize = 1000;

/// Maintenance margin rate in basis points (500 = 5%)
pub const MAINTENANCE_MARGIN_BPS: i64 = 500;

/// Order update info for WebSocket event emission
#[derive(Debug, Clone)]
pub struct OrderUpdateInfo {
    pub trader: String,
    pub order_id: String,
    pub symbol: String,
    pub status: String, // "open", "partial", "filled"
    pub filled: i64,
    pub remaining: i64,
}

/// Deposit info for WebSocket event emission
#[derive(Debug, Clone)]
pub struct DepositInfo {
    pub trader: String,
    pub amount: i64,
}

/// Complete application state
pub struct AppState {
    /// Orderbooks by symbol
    pub(crate) orderbooks: HashMap<Symbol, OrderBook>,
    /// Account manager
    pub(crate) accounts: AccountManager,
    /// Transaction mempool
    pub(crate) mempool: Mempool,
    /// Market configurations
    pub(crate) configs: HashMap<Symbol, MarketConfig>,
    /// Oracle prices (mark prices for liquidation)
    pub(crate) mark_prices: HashMap<Symbol, Price>,
    /// Current timestamp
    pub(crate) timestamp: u64,
    /// Fills from the last block execution (for event emission)
    pub(crate) pending_fills: Vec<Fill>,
    /// Order updates from the last block execution (for event emission)
    pub(crate) pending_order_updates: Vec<OrderUpdateInfo>,
    /// Trade history by symbol (recent trades, capped at MAX_TRADES_PER_SYMBOL)
    pub(crate) trade_history: HashMap<Symbol, VecDeque<Fill>>,
    /// Insurance fund balance (in cents)
    pub(crate) insurance_fund: i64,
    /// Liquidations from the last block execution (for event emission)
    pub(crate) pending_liquidations: Vec<crate::app::liquidation::LiquidationResult>,
    // Funding rate state
    /// Rolling premium samples per symbol (in bps)
    pub(crate) premium_samples: HashMap<Symbol, VecDeque<i64>>,
    /// Current funding rate per symbol (in bps)
    pub(crate) current_funding_rates: HashMap<Symbol, i64>,
    /// Last funding payment time per symbol (ms timestamp)
    pub(crate) last_funding_times: HashMap<Symbol, u64>,
    /// Funding events from last block (for event emission)
    pub(crate) pending_funding: Vec<crate::app::funding::FundingResult>,
    /// Deposits from last block (for WebSocket balance updates)
    pub(crate) pending_deposits: Vec<DepositInfo>,
    /// Candle (OHLCV) aggregation manager
    pub(crate) candle_manager: CandleManager,
    /// Staking state (validators, delegations, epochs)
    pub(crate) staking: StakingState,
    /// Pending staking events from last block
    pub(crate) pending_staking_events: Vec<crate::app::staking::StakingTxResult>,
    /// Current view (for epoch tracking)
    pub(crate) current_view: View,
    // === Trigger Orders ===
    /// All trigger orders by ID
    pub(crate) trigger_orders: HashMap<TriggerOrderId, TriggerOrder>,
    /// Trigger order IDs by trader address
    pub(crate) trigger_orders_by_trader: HashMap<Address, Vec<TriggerOrderId>>,
    /// Trigger order IDs by symbol (for efficient mark price checking)
    pub(crate) trigger_orders_by_symbol: HashMap<Symbol, Vec<TriggerOrderId>>,
    /// Trigger order ID by (trader, symbol, cloid) for cloid lookup
    pub(crate) trigger_orders_by_cloid: HashMap<(Address, Symbol, Cloid), TriggerOrderId>,
    /// Sequence number for generating trigger order IDs
    pub(crate) trigger_seq: u64,
    /// Pending trigger events from last block (for WebSocket emission)
    pub(crate) pending_trigger_events: Vec<TriggerEvent>,
}

impl AppState {
    pub fn new() -> Self {
        let mut state = Self {
            orderbooks: HashMap::new(),
            accounts: AccountManager::new(),
            mempool: Mempool::default(),
            configs: HashMap::new(),
            mark_prices: HashMap::new(),
            timestamp: 0,
            pending_fills: Vec::new(),
            pending_order_updates: Vec::new(),
            trade_history: HashMap::new(),
            insurance_fund: 0,
            pending_liquidations: Vec::new(),
            premium_samples: HashMap::new(),
            current_funding_rates: HashMap::new(),
            last_funding_times: HashMap::new(),
            pending_funding: Vec::new(),
            pending_deposits: Vec::new(),
            candle_manager: CandleManager::new(),
            staking: StakingState::new(),
            pending_staking_events: Vec::new(),
            current_view: 0,
            trigger_orders: HashMap::new(),
            trigger_orders_by_trader: HashMap::new(),
            trigger_orders_by_symbol: HashMap::new(),
            trigger_orders_by_cloid: HashMap::new(),
            trigger_seq: 0,
            pending_trigger_events: Vec::new(),
        };

        // Add default BTC-USDT market
        state.add_market(MarketConfig::default());

        state
    }

    /// Add a new market
    pub fn add_market(&mut self, config: MarketConfig) {
        let symbol = config.symbol.clone();
        self.orderbooks
            .insert(symbol.clone(), OrderBook::new(&symbol));
        self.configs.insert(symbol.clone(), config);
        self.mark_prices.insert(symbol, 5_000_000); // Default: $50,000
    }

    /// Get orderbook for a symbol
    pub fn orderbook(&self, symbol: &str) -> Option<&OrderBook> {
        self.orderbooks.get(symbol)
    }

    /// Get mutable orderbook
    pub fn orderbook_mut(&mut self, symbol: &str) -> Option<&mut OrderBook> {
        self.orderbooks.get_mut(symbol)
    }

    /// Submit a transaction to the mempool
    pub fn submit_tx(&mut self, tx: Transaction) -> Result<Hash, AppError> {
        self.mempool
            .add(tx, self.timestamp)
            .map_err(AppError::Mempool)
    }

    /// Set mark price for a symbol
    pub fn set_mark_price(&mut self, symbol: &str, price: Price) {
        self.mark_prices.insert(symbol.to_string(), price);
    }

    /// Get mark price for a symbol
    pub fn mark_price(&self, symbol: &str) -> Option<Price> {
        self.mark_prices.get(symbol).copied()
    }

    /// Get account
    pub fn account(&self, address: &str) -> Option<&crate::app::accounts::Account> {
        self.accounts.get(address)
    }

    /// Get mempool stats
    pub fn mempool_stats(&self) -> (usize, usize, usize) {
        self.mempool.bucket_counts()
    }

    /// Get all open orders for a specific address across all orderbooks
    pub fn orders_by_address(&self, address: &str) -> Vec<Order> {
        let mut orders = Vec::new();
        for book in self.orderbooks.values() {
            for order in book.orders_by_trader(address) {
                orders.push(order.clone());
            }
        }
        orders
    }

    /// Take pending fills (clears the list)
    pub fn take_pending_fills(&mut self) -> Vec<Fill> {
        std::mem::take(&mut self.pending_fills)
    }

    /// Take pending order updates (clears the list)
    pub fn take_pending_order_updates(&mut self) -> Vec<OrderUpdateInfo> {
        std::mem::take(&mut self.pending_order_updates)
    }

    /// Take pending liquidations (clears the list)
    pub fn take_pending_liquidations(&mut self) -> Vec<crate::app::liquidation::LiquidationResult> {
        std::mem::take(&mut self.pending_liquidations)
    }

    /// Take pending deposits (clears the list)
    pub fn take_pending_deposits(&mut self) -> Vec<DepositInfo> {
        std::mem::take(&mut self.pending_deposits)
    }

    /// Get account for position updates
    pub fn accounts(&self) -> &AccountManager {
        &self.accounts
    }

    /// Get mutable account manager (for nonce validation)
    pub fn accounts_mut(&mut self) -> &mut AccountManager {
        &mut self.accounts
    }

    /// Get market config
    pub fn market_config(&self, symbol: &str) -> Option<&MarketConfig> {
        self.configs.get(symbol)
    }

    /// Get recent trades for a symbol (most recent first)
    pub fn get_trades(&self, symbol: &str, limit: usize) -> Vec<&Fill> {
        self.trade_history
            .get(symbol)
            .map(|h| h.iter().rev().take(limit).collect())
            .unwrap_or_default()
    }

    /// Get candles for a symbol and interval
    pub fn get_candles(&self, symbol: &str, interval: Interval, limit: usize) -> Vec<Candle> {
        self.candle_manager.get_candles(symbol, interval, limit)
    }

    /// Get all market configs (for snapshot)
    pub fn market_configs(&self) -> &HashMap<Symbol, MarketConfig> {
        &self.configs
    }

    /// Get all mark prices (for snapshot)
    pub fn mark_prices(&self) -> &HashMap<Symbol, Price> {
        &self.mark_prices
    }

    /// Get insurance fund balance
    pub fn insurance_fund_balance(&self) -> i64 {
        self.insurance_fund
    }

    /// Add to insurance fund (can be negative for losses)
    pub fn add_to_insurance_fund(&mut self, amount: i64) {
        self.insurance_fund += amount;
    }

    /// Get current funding rate for a symbol (in bps)
    pub fn funding_rate(&self, symbol: &str) -> i64 {
        self.current_funding_rates.get(symbol).copied().unwrap_or(0)
    }

    /// Get last funding time for a symbol (ms timestamp)
    pub fn last_funding_time(&self, symbol: &str) -> u64 {
        self.last_funding_times.get(symbol).copied().unwrap_or(0)
    }

    /// Get next funding time for a symbol (ms timestamp)
    pub fn next_funding_time(&self, symbol: &str) -> u64 {
        let last = self.last_funding_time(symbol);
        let interval = self
            .configs
            .get(symbol)
            .map(|c| c.funding_interval_ms)
            .unwrap_or(3600000); // 1 hour default
        if last == 0 {
            self.timestamp + interval
        } else {
            last + interval
        }
    }

    /// Take pending funding events (clears the list)
    pub fn take_pending_funding(&mut self) -> Vec<crate::app::funding::FundingResult> {
        std::mem::take(&mut self.pending_funding)
    }

    /// Get all funding rates (for API)
    pub fn all_funding_rates(&self) -> &HashMap<Symbol, i64> {
        &self.current_funding_rates
    }

    // === Staking Accessors ===

    /// Get staking state (read-only)
    pub fn staking(&self) -> &StakingState {
        &self.staking
    }

    /// Get mutable staking state
    pub fn staking_mut(&mut self) -> &mut StakingState {
        &mut self.staking
    }

    /// Take pending staking events (clears the list)
    pub fn take_pending_staking_events(&mut self) -> Vec<crate::app::staking::StakingTxResult> {
        std::mem::take(&mut self.pending_staking_events)
    }

    /// Get active validators for consensus
    pub fn active_validators(&self) -> Vec<crate::types::NodeId> {
        self.staking.active_validators()
    }

    /// Get current epoch
    pub fn current_epoch(&self) -> u64 {
        self.staking.current_epoch
    }

    /// Get current view
    pub fn current_view(&self) -> View {
        self.current_view
    }

    // === Trigger Order Accessors ===

    /// Get a trigger order by ID
    pub fn trigger_order(&self, id: &str) -> Option<&TriggerOrder> {
        self.trigger_orders.get(id)
    }

    /// Get all trigger orders for a trader
    pub fn trigger_orders_by_trader(&self, trader: &str) -> Vec<&TriggerOrder> {
        self.trigger_orders_by_trader
            .get(trader)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.trigger_orders.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all trigger orders for a symbol
    pub fn trigger_orders_by_symbol(&self, symbol: &str) -> Vec<&TriggerOrder> {
        self.trigger_orders_by_symbol
            .get(symbol)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.trigger_orders.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Take pending trigger events (clears the list)
    pub fn take_pending_trigger_events(&mut self) -> Vec<TriggerEvent> {
        std::mem::take(&mut self.pending_trigger_events)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Application errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum AppError {
    #[error("mempool error: {0}")]
    Mempool(#[from] MempoolError),
    #[error("account error: {0}")]
    Account(#[from] crate::app::accounts::AccountError),
    #[error("orderbook error: {0}")]
    OrderBook(#[from] crate::app::orderbook::OrderBookError),
    #[error("staking error: {0}")]
    Staking(#[from] crate::app::staking::StakingError),
    #[error("trigger order error: {0}")]
    Trigger(#[from] crate::app::trigger::TriggerError),
    #[error("market not found")]
    MarketNotFound,
    #[error("order not found")]
    OrderNotFound,
    #[error("insufficient margin")]
    InsufficientMargin,
    #[error("reduce-only order would increase position")]
    ReduceOnlyViolation,
}
