//! Application State
//!
//! Integrates orderbook, accounts, and mempool into a single
//! AppHook implementation for consensus.

use std::collections::{HashMap, VecDeque};

use sha2::{Digest, Sha256};

use super::{
    accounts::{AccountManager, AccountError},
    mempool::Mempool,
    orderbook::{Fill, Order, OrderBook, OrderBookError, Side},
    MarketConfig, Symbol, Transaction,
};
use crate::consensus::AppHook;
use crate::types::{Block, Hash, Price};

/// Maximum trades stored per symbol
const MAX_TRADES_PER_SYMBOL: usize = 1000;

/// Order update info for WebSocket event emission
#[derive(Debug, Clone)]
pub struct OrderUpdateInfo {
    pub trader: String,
    pub order_id: String,
    pub symbol: String,
    pub status: String,   // "open", "partial", "filled"
    pub filled: i64,
    pub remaining: i64,
}

/// Complete application state
pub struct AppState {
    /// Orderbooks by symbol
    orderbooks: HashMap<Symbol, OrderBook>,
    /// Account manager
    accounts: AccountManager,
    /// Transaction mempool
    mempool: Mempool,
    /// Market configurations
    configs: HashMap<Symbol, MarketConfig>,
    /// Oracle prices (mark prices for liquidation)
    mark_prices: HashMap<Symbol, Price>,
    /// Current timestamp
    timestamp: u64,
    /// Fills from the last block execution (for event emission)
    pending_fills: Vec<Fill>,
    /// Order updates from the last block execution (for event emission)
    pending_order_updates: Vec<OrderUpdateInfo>,
    /// Trade history by symbol (recent trades, capped at MAX_TRADES_PER_SYMBOL)
    trade_history: HashMap<Symbol, VecDeque<Fill>>,
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
        };

        // Add default BTC-USDT market
        state.add_market(MarketConfig::default());

        state
    }

    /// Add a new market
    pub fn add_market(&mut self, config: MarketConfig) {
        let symbol = config.symbol.clone();
        self.orderbooks.insert(symbol.clone(), OrderBook::new(&symbol));
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
        self.mempool.add(tx, self.timestamp)
            .map_err(|e| AppError::Mempool(e.to_string()))
    }

    /// Execute a single transaction
    fn execute_tx(&mut self, tx: Transaction) -> Result<Vec<Fill>, AppError> {
        match tx {
            Transaction::Deposit { trader, amount } => {
                self.accounts.deposit(&trader, amount)?;
                Ok(vec![])
            }

            Transaction::Withdraw { trader, amount } => {
                self.accounts.withdraw(&trader, amount)?;
                Ok(vec![])
            }

            Transaction::CancelOrder { trader: _, order_id } => {
                // Find the orderbook with this order
                for book in self.orderbooks.values_mut() {
                    if book.cancel(&order_id) {
                        // TODO: Verify trader owns order
                        return Ok(vec![]);
                    }
                }
                Err(AppError::OrderNotFound)
            }

            Transaction::PlaceOrder {
                trader,
                symbol,
                side,
                price,
                mut size,
                order_type,
                reduce_only,
            } => {
                let config = self.configs.get(&symbol)
                    .ok_or(AppError::MarketNotFound)?;

                let book = self.orderbooks.get_mut(&symbol)
                    .ok_or(AppError::MarketNotFound)?;

                // Handle reduce_only validation
                if reduce_only {
                    let account = self.accounts.get_or_create(&trader);
                    let pos = account.position(&symbol);

                    // Long position (size > 0) can only reduce with sells (Ask)
                    // Short position (size < 0) can only reduce with buys (Bid)
                    let is_reducing = (pos.size > 0 && side == Side::Ask) ||
                                     (pos.size < 0 && side == Side::Bid);

                    if !is_reducing || pos.size == 0 {
                        return Err(AppError::ReduceOnlyViolation);
                    }

                    // Clamp size to position size
                    size = size.min(pos.size.abs());
                }

                // Check margin (simplified: require full notional)
                let notional = (size * price) / 100_000_000;
                let account = self.accounts.get_or_create(&trader);
                if account.balance < notional / 10 {
                    return Err(AppError::InsufficientMargin);
                }

                // Create order
                let order_id = book.next_order_id();
                let order = Order {
                    id: order_id.clone(),
                    trader: trader.clone(),
                    symbol: symbol.clone(),
                    side,
                    price,
                    size,
                    original_size: size,
                    order_type,
                    reduce_only,
                    timestamp: self.timestamp,
                };

                // Place order
                let fills = book.place(order, config)?;

                // Calculate filled amount for the taker's order
                let filled: i64 = fills.iter().map(|f| f.size).sum();
                let remaining = size - filled;

                // Determine order status
                let status = if remaining == 0 {
                    "filled"
                } else if filled > 0 {
                    "partial"
                } else {
                    "open"
                };

                // Emit order update for the taker (order placer)
                self.pending_order_updates.push(OrderUpdateInfo {
                    trader: trader.clone(),
                    order_id: order_id.clone(),
                    symbol: symbol.clone(),
                    status: status.to_string(),
                    filled,
                    remaining,
                });

                // Process fills
                for fill in &fills {
                    let is_buy = fill.side == Side::Bid;
                    self.accounts.apply_fill(
                        &fill.maker,
                        &fill.taker,
                        &fill.symbol,
                        is_buy,
                        fill.size,
                        fill.price,
                        config.maker_fee,
                        config.taker_fee,
                    );

                    // Update mark price to last trade
                    self.mark_prices.insert(symbol.clone(), fill.price);

                    // Store fill in trade history
                    let history = self.trade_history.entry(fill.symbol.clone()).or_default();
                    history.push_back(fill.clone());
                    while history.len() > MAX_TRADES_PER_SYMBOL {
                        history.pop_front();
                    }
                }

                Ok(fills)
            }
        }
    }

    /// Set mark price for a symbol
    pub fn set_mark_price(&mut self, symbol: &str, price: Price) {
        self.mark_prices.insert(symbol.to_string(), price);
    }

    /// Get mark price for a symbol
    pub fn mark_price(&self, symbol: &str) -> Option<Price> {
        self.mark_prices.get(symbol).copied()
    }

    /// Compute state hash for Byzantine detection
    fn compute_state_hash(&self) -> Hash {
        let mut hasher = Sha256::new();

        // Hash all orderbooks (sorted by symbol for determinism)
        let mut symbols: Vec<_> = self.orderbooks.keys().collect();
        symbols.sort();

        for symbol in symbols {
            if let Some(book) = self.orderbooks.get(symbol) {
                hasher.update(symbol.as_bytes());
                hasher.update(book.best_bid().unwrap_or(0).to_le_bytes());
                hasher.update(book.best_ask().unwrap_or(0).to_le_bytes());
                hasher.update(book.last_price().to_le_bytes());
            }
        }

        // Hash mark prices
        for (symbol, price) in &self.mark_prices {
            hasher.update(symbol.as_bytes());
            hasher.update(price.to_le_bytes());
        }

        hasher.finalize().into()
    }

    /// Get account
    pub fn account(&self, address: &str) -> Option<&super::accounts::Account> {
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

    /// Get account for position updates
    pub fn accounts(&self) -> &AccountManager {
        &self.accounts
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
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppHook for AppState {
    fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
        // Get pending transactions (without removing them yet)
        // This is called before propose, actual removal happens on commit
        // For now, just return empty - real impl would serialize pending txs
        vec![]
    }

    fn execute(&mut self, block: &Block) -> Hash {
        self.timestamp = block.timestamp;

        // Clear pending events from previous block
        self.pending_fills.clear();
        self.pending_order_updates.clear();

        // Get transactions for this block from mempool
        let txs = self.mempool.prepare_block(1000);

        // Execute each transaction
        for tx in txs {
            match self.execute_tx(tx) {
                Ok(fills) => {
                    // Collect fills for event emission
                    self.pending_fills.extend(fills);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Transaction failed");
                }
            }
        }

        // Return state hash for Byzantine detection
        self.compute_state_hash()
    }
}

/// Application errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum AppError {
    #[error("mempool error: {0}")]
    Mempool(String),
    #[error("account error: {0}")]
    Account(#[from] AccountError),
    #[error("orderbook error: {0}")]
    OrderBook(#[from] OrderBookError),
    #[error("market not found")]
    MarketNotFound,
    #[error("order not found")]
    OrderNotFound,
    #[error("insufficient margin")]
    InsufficientMargin,
    #[error("reduce-only order would increase position")]
    ReduceOnlyViolation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::OrderType;

    #[test]
    fn test_deposit_and_order() {
        let mut state = AppState::new();

        // Deposit
        state.execute_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000, // $1M in cents
        }).unwrap();

        assert_eq!(state.account("alice").unwrap().balance, 100_000_000);

        // Place order
        let fills = state.execute_tx(Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000, // $50,000
            size: 100_000_000, // 1 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        }).unwrap();

        assert!(fills.is_empty()); // No counterparty
        assert!(state.orderbook("BTC-USDT").unwrap().best_bid().is_some());
    }

    #[test]
    fn test_matching() {
        let mut state = AppState::new();

        // Alice deposits and bids
        state.execute_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        }).unwrap();

        state.execute_tx(Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }).unwrap();

        // Bob deposits and asks (should match)
        state.execute_tx(Transaction::Deposit {
            trader: "bob".into(),
            amount: 100_000_000,
        }).unwrap();

        let fills = state.execute_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 4_900_000, // Below bid
            size: 50_000_000, // 0.5 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        }).unwrap();

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].size, 50_000_000);
        assert_eq!(fills[0].price, 5_000_000); // At bid price

        // Check positions
        let alice_pos = state.account("alice").unwrap().position("BTC-USDT");
        assert_eq!(alice_pos.size, 50_000_000); // Long 0.5 BTC

        let bob_pos = state.account("bob").unwrap().position("BTC-USDT");
        assert_eq!(bob_pos.size, -50_000_000); // Short 0.5 BTC
    }

    #[test]
    fn test_state_hash_deterministic() {
        let mut state = AppState::new();

        state.execute_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        }).unwrap();

        let hash1 = state.compute_state_hash();
        let hash2 = state.compute_state_hash();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_trade_history() {
        let mut state = AppState::new();

        // Setup: Alice bids, Bob asks -> should match and create trade
        state.execute_tx(Transaction::Deposit { trader: "alice".into(), amount: 100_000_000 }).unwrap();
        state.execute_tx(Transaction::Deposit { trader: "bob".into(), amount: 100_000_000 }).unwrap();

        // Alice places bid
        state.execute_tx(Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }).unwrap();

        // No trades yet
        assert!(state.get_trades("BTC-USDT", 10).is_empty());

        // Bob places ask (matches Alice's bid)
        state.execute_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 4_900_000,
            size: 50_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }).unwrap();

        // Now we should have 1 trade
        let trades = state.get_trades("BTC-USDT", 10);
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price, 5_000_000);
        assert_eq!(trades[0].size, 50_000_000);

        // Unknown symbol returns empty
        assert!(state.get_trades("ETH-USDT", 10).is_empty());
    }
}
