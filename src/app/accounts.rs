//! Account and Position Management
//!
//! Tracks trader balances, positions, and margin.
//! Uses integer math (satoshis/cents) for determinism.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{Address, Symbol};
use crate::types::{Price, Size};

/// A position in a single market
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Position {
    /// Signed size: positive = long, negative = short
    pub size: Size,
    /// Average entry price (in cents)
    pub entry_price: Price,
    /// Realized PnL from closed portions (in cents)
    pub realized_pnl: i64,
    /// Cumulative funding paid (negative) or received (positive)
    #[serde(default)]
    pub cumulative_funding: i64,
    /// Timestamp when funding was last applied to this position
    #[serde(default)]
    pub last_funding_timestamp: u64,
}

impl Position {
    /// Calculate unrealized PnL at a given mark price
    pub fn unrealized_pnl(&self, mark_price: Price) -> i64 {
        if self.size == 0 {
            return 0;
        }
        // PnL = size * (mark - entry) / scale_factor
        // Since price is in cents and size in satoshis,
        // we need to adjust for the scaling
        let price_diff = mark_price - self.entry_price;
        // For simplicity: PnL in cents = size * price_diff / 100_000_000
        // This gives PnL per satoshi of position
        (self.size * price_diff) / 100_000_000
    }

    /// Calculate notional value at mark price
    pub fn notional(&self, mark_price: Price) -> i64 {
        (self.size.abs() * mark_price) / 100_000_000
    }

    /// Apply funding payment to this position
    /// funding_rate_bps: positive = longs pay shorts
    /// index_price: price used to calculate payment (usually mark/index price)
    /// Returns the payment amount (positive = received, negative = paid)
    pub fn apply_funding(&mut self, funding_rate_bps: i64, index_price: Price, timestamp: u64) -> i64 {
        if self.size == 0 {
            return 0;
        }

        // Payment = |size| * index_price * funding_rate / 1e8 / 10000
        // Longs pay when rate > 0, shorts receive
        // Shorts pay when rate < 0, longs receive
        let notional = (self.size.abs() * index_price) / 100_000_000;
        let payment = (notional * funding_rate_bps) / 10000;

        // Positive rate: longs pay, shorts receive
        // Negative rate: shorts pay, longs receive
        let signed_payment = if self.size > 0 {
            -payment // Long pays (or receives if rate negative)
        } else {
            payment // Short receives (or pays if rate negative)
        };

        self.cumulative_funding += signed_payment;
        self.last_funding_timestamp = timestamp;

        signed_payment
    }

    /// Calculate liquidation price given available margin
    /// Returns 0 if position is empty or margin is insufficient to calculate
    pub fn liquidation_price(&self, available_margin: i64, maintenance_rate_bps: i64) -> Price {
        if self.size == 0 || self.entry_price == 0 {
            return 0;
        }

        // Notional at entry = |size| * entry_price / 1e8
        let notional_at_entry = (self.size.abs() * self.entry_price) / 100_000_000;
        if notional_at_entry == 0 {
            return 0;
        }

        // Maintenance margin = notional * maintenance_rate / 10000
        // At liquidation: equity = maintenance_margin
        // equity = margin + unrealized_pnl
        // For long: pnl = size * (liq_price - entry) / 1e8
        // At liquidation: margin + size * (liq - entry) / 1e8 = notional_at_liq * maint / 10000
        // Simplified: liq_price where margin covers losses down to maintenance

        // margin_ratio = available_margin * 1e8 / (|size| * entry_price)
        // For long: liq = entry * (1 - margin_ratio + maintenance_rate)
        // For short: liq = entry * (1 + margin_ratio - maintenance_rate)

        let margin_ratio_bps = if notional_at_entry > 0 {
            (available_margin * 10000) / notional_at_entry
        } else {
            0
        };

        if self.size > 0 {
            // Long position: liquidated when price drops
            // liq_price = entry * (10000 - margin_ratio_bps + maintenance_rate_bps) / 10000
            let factor = 10000 - margin_ratio_bps + maintenance_rate_bps;
            if factor <= 0 {
                return 0; // Well-margined, won't liquidate
            }
            (self.entry_price * factor) / 10000
        } else {
            // Short position: liquidated when price rises
            // liq_price = entry * (10000 + margin_ratio_bps - maintenance_rate_bps) / 10000
            let factor = 10000 + margin_ratio_bps - maintenance_rate_bps;
            (self.entry_price * factor) / 10000
        }
    }
}

/// Trader account
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub address: Address,
    /// Free collateral (in cents, e.g., USDC)
    pub balance: i64,
    /// Collateral locked in positions
    pub locked: i64,
    /// Positions by symbol
    pub positions: HashMap<Symbol, Position>,
    /// Next expected nonce (for replay protection)
    #[serde(default)]
    pub nonce: u64,
}

impl Account {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            balance: 0,
            locked: 0,
            positions: HashMap::new(),
            nonce: 0,
        }
    }

    /// Check if nonce is valid (must be exactly current nonce)
    pub fn validate_nonce(&self, nonce: u64) -> bool {
        nonce == self.nonce
    }

    /// Increment nonce after successful transaction
    pub fn increment_nonce(&mut self) {
        self.nonce += 1;
    }

    /// Get current nonce (for API responses)
    pub fn current_nonce(&self) -> u64 {
        self.nonce
    }

    /// Total equity = balance + locked + unrealized PnL
    pub fn equity(&self, mark_prices: &HashMap<Symbol, Price>) -> i64 {
        let unrealized: i64 = self.positions
            .iter()
            .map(|(symbol, pos)| {
                mark_prices.get(symbol)
                    .map(|&mark| pos.unrealized_pnl(mark))
                    .unwrap_or(0)
            })
            .sum();

        self.balance + self.locked + unrealized
    }

    /// Available margin for new orders
    pub fn available_margin(&self) -> i64 {
        self.balance
    }

    /// Calculate total maintenance margin required across all positions
    /// maintenance_rate_bps: maintenance margin rate in basis points (500 = 5%)
    pub fn maintenance_margin_required(&self, mark_prices: &HashMap<Symbol, Price>, maintenance_rate_bps: i64) -> i64 {
        self.positions
            .iter()
            .filter(|(_, pos)| pos.size != 0)
            .map(|(symbol, pos)| {
                let mark = mark_prices.get(symbol).copied().unwrap_or(pos.entry_price);
                let notional = pos.notional(mark);
                (notional * maintenance_rate_bps) / 10000
            })
            .sum()
    }

    /// Check if account should be liquidated
    /// Returns true if equity < maintenance margin required
    pub fn is_liquidatable(&self, mark_prices: &HashMap<Symbol, Price>, maintenance_rate_bps: i64) -> bool {
        // No positions = not liquidatable
        if self.positions.values().all(|p| p.size == 0) {
            return false;
        }

        let equity = self.equity(mark_prices);
        let maintenance = self.maintenance_margin_required(mark_prices, maintenance_rate_bps);

        equity < maintenance
    }

    /// Get position for a symbol (or empty)
    pub fn position(&self, symbol: &str) -> Position {
        self.positions.get(symbol).cloned().unwrap_or_default()
    }

    /// Update position after a fill
    pub fn apply_fill(
        &mut self,
        symbol: &str,
        side_is_buy: bool,
        fill_size: Size,
        fill_price: Price,
    ) {
        let pos = self.positions.entry(symbol.to_string()).or_default();

        let fill_size_signed = if side_is_buy { fill_size } else { -fill_size };

        if pos.size == 0 {
            // Opening new position
            pos.size = fill_size_signed;
            pos.entry_price = fill_price;
        } else if (pos.size > 0) == side_is_buy {
            // Adding to position - update average entry
            let old_notional = pos.size.abs() * pos.entry_price;
            let add_notional = fill_size * fill_price;
            let new_size = pos.size + fill_size_signed;
            pos.entry_price = (old_notional + add_notional) / new_size.abs();
            pos.size = new_size;
        } else {
            // Reducing position - realize PnL
            let close_size = fill_size.min(pos.size.abs());
            let pnl_per_unit = if pos.size > 0 {
                fill_price - pos.entry_price // Long: profit when sell higher
            } else {
                pos.entry_price - fill_price // Short: profit when buy lower
            };
            let realized = (close_size * pnl_per_unit) / 100_000_000;
            pos.realized_pnl += realized;
            self.balance += realized;

            pos.size += fill_size_signed;

            // If flipped sides
            if (pos.size > 0) != (pos.size - fill_size_signed > 0) && pos.size != 0 {
                pos.entry_price = fill_price;
            }
        }

        // Clean up zero positions
        if pos.size == 0 {
            pos.entry_price = 0;
        }
    }
}

/// Manages all accounts
pub struct AccountManager {
    accounts: HashMap<Address, Account>,
}

impl AccountManager {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
        }
    }

    /// Create AccountManager from a list of accounts (for recovery from snapshot)
    pub fn from_accounts(accounts: Vec<Account>) -> Self {
        let accounts_map = accounts
            .into_iter()
            .map(|a| (a.address.clone(), a))
            .collect();
        Self { accounts: accounts_map }
    }

    /// Get all accounts (for snapshot)
    pub fn all_accounts(&self) -> Vec<Account> {
        self.accounts.values().cloned().collect()
    }

    /// Get or create account (no faucet - use `get_or_create_with_faucet` for dev mode)
    pub fn get_or_create(&mut self, address: &str) -> &mut Account {
        let addr_lower = address.to_lowercase();
        self.accounts
            .entry(addr_lower.clone())
            .or_insert_with(|| Account::new(&addr_lower))
    }

    /// Get or create account with optional faucet funding (for API layer)
    pub fn get_or_create_with_faucet(&mut self, address: &str, faucet_amount: i64) -> &mut Account {
        let addr_lower = address.to_lowercase();
        self.accounts
            .entry(addr_lower.clone())
            .or_insert_with(|| {
                let mut account = Account::new(&addr_lower);
                if faucet_amount > 0 {
                    account.balance = faucet_amount;
                    tracing::info!(
                        address = %addr_lower,
                        balance = faucet_amount,
                        "New account created with faucet funds"
                    );
                }
                account
            })
    }

    /// Get account (read-only)
    pub fn get(&self, address: &str) -> Option<&Account> {
        self.accounts.get(&address.to_lowercase())
    }

    /// Deposit funds
    pub fn deposit(&mut self, address: &str, amount: i64) -> Result<(), AccountError> {
        if amount <= 0 {
            return Err(AccountError::InvalidAmount);
        }
        let account = self.get_or_create(address);
        account.balance += amount;
        Ok(())
    }

    /// Withdraw funds
    pub fn withdraw(&mut self, address: &str, amount: i64) -> Result<(), AccountError> {
        if amount <= 0 {
            return Err(AccountError::InvalidAmount);
        }
        let account = self.get_or_create(address);
        if account.balance < amount {
            return Err(AccountError::InsufficientBalance);
        }
        account.balance -= amount;
        Ok(())
    }

    /// Lock collateral for an order
    pub fn lock_collateral(&mut self, address: &str, amount: i64) -> Result<(), AccountError> {
        let account = self.get_or_create(address);
        if account.balance < amount {
            return Err(AccountError::InsufficientBalance);
        }
        account.balance -= amount;
        account.locked += amount;
        Ok(())
    }

    /// Unlock collateral (order cancelled/filled)
    pub fn unlock_collateral(&mut self, address: &str, amount: i64) {
        if let Some(account) = self.accounts.get_mut(&address.to_lowercase()) {
            let to_unlock = amount.min(account.locked);
            account.locked -= to_unlock;
            account.balance += to_unlock;
        }
    }

    /// Apply a fill to both maker and taker
    pub fn apply_fill(
        &mut self,
        maker: &str,
        taker: &str,
        symbol: &str,
        taker_is_buyer: bool,
        size: Size,
        price: Price,
        maker_fee: i64,
        taker_fee: i64,
    ) {
        // Calculate fees (in cents)
        let notional = (size * price) / 100_000_000;
        let maker_fee_amount = (notional * maker_fee) / 10000; // basis points
        let taker_fee_amount = (notional * taker_fee) / 10000;

        // Apply to maker (opposite side of taker)
        let maker_account = self.get_or_create(maker);
        maker_account.apply_fill(symbol, !taker_is_buyer, size, price);
        maker_account.balance -= maker_fee_amount;

        // Apply to taker
        let taker_account = self.get_or_create(taker);
        taker_account.apply_fill(symbol, taker_is_buyer, size, price);
        taker_account.balance -= taker_fee_amount;
    }

    /// Get all accounts with positions in a symbol
    pub fn accounts_with_position(&self, symbol: &str) -> Vec<&Account> {
        self.accounts
            .values()
            .filter(|a| a.positions.get(symbol).map(|p| p.size != 0).unwrap_or(false))
            .collect()
    }

    /// Check if account can open position (has margin)
    pub fn can_open_position(
        &self,
        address: &str,
        notional: i64,
        leverage: i64,
    ) -> bool {
        let required_margin = notional / leverage;
        self.accounts
            .get(address)
            .map(|a| a.balance >= required_margin)
            .unwrap_or(false)
    }

    /// Validate and consume nonce atomically
    pub fn use_nonce(&mut self, address: &str, nonce: u64) -> Result<(), AccountError> {
        let account = self.get_or_create(address);
        if !account.validate_nonce(nonce) {
            return Err(AccountError::InvalidNonce {
                expected: account.nonce,
                got: nonce,
            });
        }
        account.increment_nonce();
        Ok(())
    }

    /// Get current nonce for an address
    pub fn get_nonce(&self, address: &str) -> u64 {
        self.accounts.get(&address.to_lowercase()).map(|a| a.nonce).unwrap_or(0)
    }
}

impl Default for AccountManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Account errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum AccountError {
    #[error("invalid amount")]
    InvalidAmount,
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("account not found")]
    NotFound,
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deposit_withdraw() {
        let mut mgr = AccountManager::new();

        mgr.deposit("alice", 10000).unwrap();
        assert_eq!(mgr.get("alice").unwrap().balance, 10000);

        mgr.withdraw("alice", 3000).unwrap();
        assert_eq!(mgr.get("alice").unwrap().balance, 7000);

        assert!(mgr.withdraw("alice", 10000).is_err());
    }

    #[test]
    fn test_position_pnl() {
        let mut pos = Position::default();

        // Long 1 BTC at $50,000
        pos.size = 100_000_000; // 1 BTC in satoshis
        pos.entry_price = 5_000_000; // $50,000 in cents

        // Mark at $51,000 -> $1,000 profit
        let pnl = pos.unrealized_pnl(5_100_000);
        assert_eq!(pnl, 100_000); // $1,000 in cents
    }

    #[test]
    fn test_fill_increases_position() {
        let mut account = Account::new("trader");

        // Buy 1 BTC at $50,000
        account.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        let pos = account.position("BTC-USDT");
        assert_eq!(pos.size, 100_000_000);
        assert_eq!(pos.entry_price, 5_000_000);

        // Buy 1 more BTC at $52,000 -> avg = $51,000
        account.apply_fill("BTC-USDT", true, 100_000_000, 5_200_000);

        let pos = account.position("BTC-USDT");
        assert_eq!(pos.size, 200_000_000);
        assert_eq!(pos.entry_price, 5_100_000);
    }

    #[test]
    fn test_fill_reduces_position() {
        let mut account = Account::new("trader");
        account.balance = 100_000_000; // $1M

        // Long 2 BTC at $50,000
        account.apply_fill("BTC-USDT", true, 200_000_000, 5_000_000);

        // Sell 1 BTC at $51,000 -> realize $1,000 profit
        account.apply_fill("BTC-USDT", false, 100_000_000, 5_100_000);

        let pos = account.position("BTC-USDT");
        assert_eq!(pos.size, 100_000_000); // 1 BTC left
        assert_eq!(pos.realized_pnl, 100_000); // $1,000 realized
    }

    #[test]
    fn test_nonce_validation() {
        let mut mgr = AccountManager::new();

        // First nonce should be 0
        assert_eq!(mgr.get_nonce("alice"), 0);

        // Use nonce 0
        assert!(mgr.use_nonce("alice", 0).is_ok());
        assert_eq!(mgr.get_nonce("alice"), 1);

        // Replay should fail
        assert!(mgr.use_nonce("alice", 0).is_err());

        // Wrong nonce should fail
        assert!(mgr.use_nonce("alice", 5).is_err());

        // Next nonce should work
        assert!(mgr.use_nonce("alice", 1).is_ok());
        assert_eq!(mgr.get_nonce("alice"), 2);
    }

    #[test]
    fn test_liquidation_price_long() {
        let mut pos = Position::default();
        // Long 1 BTC at $50,000
        pos.size = 100_000_000; // 1 BTC
        pos.entry_price = 5_000_000; // $50,000

        // With $5,000 margin (10% of notional) and 5% maintenance (500 bps)
        // margin_ratio = 10%, so factor = 10000 - 1000 + 500 = 9500
        // liq_price = 50000 * 9500 / 10000 = $47,500
        let liq = pos.liquidation_price(500_000, 500);
        assert_eq!(liq, 4_750_000); // $47,500
    }

    #[test]
    fn test_liquidation_price_short() {
        let mut pos = Position::default();
        // Short 1 BTC at $50,000
        pos.size = -100_000_000; // -1 BTC
        pos.entry_price = 5_000_000; // $50,000

        // With $5,000 margin (10% of notional) and 5% maintenance (500 bps)
        // margin_ratio = 10%, so factor = 10000 + 1000 - 500 = 10500
        // liq_price = 50000 * 10500 / 10000 = $52,500
        let liq = pos.liquidation_price(500_000, 500);
        assert_eq!(liq, 5_250_000); // $52,500
    }

    #[test]
    fn test_is_liquidatable() {
        let mut account = Account::new("trader");
        account.balance = 500_000; // $5,000

        // Long 1 BTC at $50,000
        account.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        let mut mark_prices = HashMap::new();

        // At entry price, equity = $5,000, maintenance = $2,500 (5% of $50k)
        mark_prices.insert("BTC-USDT".to_string(), 5_000_000);
        assert!(!account.is_liquidatable(&mark_prices, 500)); // Not liquidatable

        // Price drops to $48,000, unrealized PnL = -$2,000
        // equity = 5000 - 2000 = $3,000, maintenance = $2,400 (5% of $48k)
        mark_prices.insert("BTC-USDT".to_string(), 4_800_000);
        assert!(!account.is_liquidatable(&mark_prices, 500)); // Still OK

        // Price drops to $46,000, unrealized PnL = -$4,000
        // equity = 5000 - 4000 = $1,000, maintenance = $2,300 (5% of $46k)
        mark_prices.insert("BTC-USDT".to_string(), 4_600_000);
        assert!(account.is_liquidatable(&mark_prices, 500)); // Liquidatable!
    }
}
