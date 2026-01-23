//! Test Fixtures
//!
//! Provides TestContext and common constants for E2E tests.

use hyperlicked::app::AppState;
use hyperlicked::consensus::AppHook;
use hyperlicked::types::Block;

// =============================================================================
// Constants
// =============================================================================

/// Standard trader names
pub const TRADER_ALICE: &str = "alice";
pub const TRADER_BOB: &str = "bob";
pub const TRADER_CHARLIE: &str = "charlie";

/// Default deposit amount: $1,000,000 (100_000_000 cents)
pub const DEFAULT_DEPOSIT: i64 = 100_000_000;

/// Small deposit for margin testing: $5,000 (500_000 cents)
pub const SMALL_DEPOSIT: i64 = 500_000;

/// Default trading symbol
pub const BTC_SYMBOL: &str = "BTC-USDT";

/// Default BTC price: $50,000 (5_000_000 cents)
pub const DEFAULT_BTC_PRICE: i64 = 5_000_000;

/// 1 BTC in satoshis
pub const ONE_BTC: i64 = 100_000_000;

/// Half BTC in satoshis
pub const HALF_BTC: i64 = 50_000_000;

/// Minimum tick increment for prices (1 cent)
pub const TICK_SIZE: i64 = 1;

// =============================================================================
// TestContext
// =============================================================================

/// Test context providing a fresh AppState and helper methods
pub struct TestContext {
    pub state: AppState,
    block_height: u64,
    current_timestamp: u64,
}

impl TestContext {
    /// Create a new test context with fresh state
    pub fn new() -> Self {
        Self {
            state: AppState::new(),
            block_height: 0,
            current_timestamp: 1000,
        }
    }

    /// Get current timestamp
    pub fn timestamp(&self) -> u64 {
        self.current_timestamp
    }

    /// Create a context with pre-funded traders
    ///
    /// # Example
    /// ```ignore
    /// let ctx = TestContext::with_traders(&[
    ///     ("alice", 100_000_000),
    ///     ("bob", 50_000_000),
    /// ]);
    /// ```
    pub fn with_traders(traders: &[(&str, i64)]) -> Self {
        use hyperlicked::app::Transaction;

        let mut ctx = Self::new();

        for (trader, amount) in traders {
            ctx.state
                .submit_tx(Transaction::Deposit {
                    trader: (*trader).into(),
                    amount: *amount,
                })
                .expect("deposit should succeed");
        }

        // Execute the deposits
        ctx.execute_block();

        ctx
    }

    /// Create a context with Alice and Bob funded with default amounts
    pub fn with_default_traders() -> Self {
        Self::with_traders(&[
            (TRADER_ALICE, DEFAULT_DEPOSIT),
            (TRADER_BOB, DEFAULT_DEPOSIT),
        ])
    }

    /// Execute a block, processing all pending transactions
    pub fn execute_block(&mut self) -> Block {
        self.execute_block_at(self.current_timestamp + 1000)
    }

    /// Execute a block at a specific timestamp
    pub fn execute_block_at(&mut self, timestamp: u64) -> Block {
        self.block_height += 1;
        self.current_timestamp = timestamp;

        let block = Block {
            view: self.block_height,
            height: self.block_height,
            parent: [0u8; 32],
            payload: vec![],
            proposer: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp,
            justify: None,
        };

        self.state.execute(&block);
        block
    }

    /// Get current block height
    pub fn block_height(&self) -> u64 {
        self.block_height
    }

    /// Set the mark price for a symbol
    pub fn set_mark_price(&mut self, symbol: &str, price: i64) {
        self.state.set_mark_price(symbol, price);
    }

    /// Get account balance
    pub fn balance(&self, trader: &str) -> i64 {
        self.state
            .account(trader)
            .map(|a| a.balance)
            .unwrap_or(0)
    }

    /// Get position size for a trader/symbol
    pub fn position_size(&self, trader: &str, symbol: &str) -> i64 {
        self.state
            .account(trader)
            .map(|a| a.position(symbol).size)
            .unwrap_or(0)
    }

    /// Get position entry price for a trader/symbol
    pub fn entry_price(&self, trader: &str, symbol: &str) -> i64 {
        self.state
            .account(trader)
            .map(|a| a.position(symbol).entry_price)
            .unwrap_or(0)
    }

    /// Get best bid price for a symbol
    pub fn best_bid(&self, symbol: &str) -> Option<i64> {
        self.state.orderbook(symbol)?.best_bid()
    }

    /// Get best ask price for a symbol
    pub fn best_ask(&self, symbol: &str) -> Option<i64> {
        self.state.orderbook(symbol)?.best_ask()
    }

    /// Get orderbook depth at a price level
    pub fn bid_depth(&self, symbol: &str, limit: usize) -> Vec<(i64, i64)> {
        self.state
            .orderbook(symbol)
            .map(|b| b.bid_levels(limit).into_iter().map(|l| (l.price, l.size)).collect())
            .unwrap_or_default()
    }

    /// Get orderbook ask depth
    pub fn ask_depth(&self, symbol: &str, limit: usize) -> Vec<(i64, i64)> {
        self.state
            .orderbook(symbol)
            .map(|b| b.ask_levels(limit).into_iter().map(|l| (l.price, l.size)).collect())
            .unwrap_or_default()
    }
}

impl Default for TestContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_creation() {
        let ctx = TestContext::new();
        assert_eq!(ctx.block_height(), 0);
    }

    #[test]
    fn test_context_with_traders() {
        let ctx = TestContext::with_traders(&[("alice", 100_000)]);
        assert_eq!(ctx.balance("alice"), 100_000);
    }

    #[test]
    fn test_default_traders() {
        let ctx = TestContext::with_default_traders();
        assert_eq!(ctx.balance(TRADER_ALICE), DEFAULT_DEPOSIT);
        assert_eq!(ctx.balance(TRADER_BOB), DEFAULT_DEPOSIT);
    }
}
