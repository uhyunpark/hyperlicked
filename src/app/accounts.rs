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
}

impl Account {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            balance: 0,
            locked: 0,
            positions: HashMap::new(),
        }
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

    /// Get or create account
    pub fn get_or_create(&mut self, address: &str) -> &mut Account {
        self.accounts
            .entry(address.to_string())
            .or_insert_with(|| Account::new(address))
    }

    /// Get account (read-only)
    pub fn get(&self, address: &str) -> Option<&Account> {
        self.accounts.get(address)
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
        if let Some(account) = self.accounts.get_mut(address) {
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
}
