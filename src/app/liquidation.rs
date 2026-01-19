//! Liquidation Engine
//!
//! Checks accounts after each block and liquidates underwater positions.
//! Proceeds go to the insurance fund.

use std::collections::HashMap;

use crate::types::Price;

use super::accounts::AccountManager;
use super::state::MAINTENANCE_MARGIN_BPS;
use super::Symbol;

/// Liquidation errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum LiquidationError {
    #[error("account not found: {0}")]
    AccountNotFound(String),
    #[error("no mark price for symbol: {0}")]
    NoMarkPrice(String),
    #[error("account not liquidatable")]
    NotLiquidatable,
}

/// Result of a liquidation
#[derive(Debug, Clone)]
pub struct LiquidationResult {
    /// Account address that was liquidated
    pub address: String,
    /// Symbol of the liquidated position
    pub symbol: String,
    /// Size that was closed (absolute value)
    pub size: i64,
    /// Price at which position was closed
    pub price: i64,
    /// PnL from liquidation (positive = profit to insurance fund)
    pub pnl: i64,
    /// Whether the position was long or short
    pub was_long: bool,
}

/// Check all accounts and liquidate underwater positions
///
/// Returns list of liquidations performed and total PnL for insurance fund
pub fn check_and_liquidate(
    accounts: &mut AccountManager,
    mark_prices: &HashMap<Symbol, Price>,
) -> Vec<LiquidationResult> {
    let mut results = Vec::new();

    // Collect addresses that need liquidation
    // (can't modify while iterating)
    let addresses_to_check: Vec<String> = accounts
        .all_accounts()
        .iter()
        .filter(|a| a.is_liquidatable(mark_prices, MAINTENANCE_MARGIN_BPS))
        .map(|a| a.address.clone())
        .collect();

    // Process each underwater account
    for address in addresses_to_check {
        let liquidations = liquidate_account(accounts, &address, mark_prices);
        results.extend(liquidations);
    }

    if !results.is_empty() {
        let total_pnl: i64 = results.iter().map(|r| r.pnl).sum();
        tracing::info!(
            count = results.len(),
            total_pnl,
            "Liquidations processed"
        );
    }

    results
}

/// Liquidate all positions for an underwater account
fn liquidate_account(
    accounts: &mut AccountManager,
    address: &str,
    mark_prices: &HashMap<Symbol, Price>,
) -> Vec<LiquidationResult> {
    let mut results = Vec::new();

    // Get positions to liquidate
    let positions: Vec<(String, i64, i64)> = {
        let account = match accounts.get(address) {
            Some(a) => a,
            None => return results,
        };

        account
            .positions
            .iter()
            .filter(|(_, pos)| pos.size != 0)
            .map(|(symbol, pos)| (symbol.clone(), pos.size, pos.entry_price))
            .collect()
    };

    // Liquidate each position
    for (symbol, size, entry_price) in positions {
        let mark_price = match mark_prices.get(&symbol) {
            Some(&p) => p,
            None => continue, // Skip if no mark price
        };

        let was_long = size > 0;
        let abs_size = size.abs();

        // Calculate PnL: close at mark price
        // Long: (mark - entry) * size
        // Short: (entry - mark) * size
        let price_diff = if was_long {
            mark_price - entry_price
        } else {
            entry_price - mark_price
        };
        let pnl = (abs_size * price_diff) / 100_000_000;

        // Close the position by applying opposite fill
        let account = accounts.get_or_create(address);
        account.apply_fill(&symbol, !was_long, abs_size, mark_price);

        // The account's balance now reflects the realized PnL
        // For insurance fund: we take whatever remains (could be negative)

        tracing::warn!(
            address,
            symbol,
            size,
            mark_price,
            pnl,
            "Position liquidated"
        );

        results.push(LiquidationResult {
            address: address.to_string(),
            symbol,
            size: abs_size,
            price: mark_price,
            pnl,
            was_long,
        });
    }

    // After liquidation, transfer remaining balance to insurance fund
    // and zero out the account
    let account = accounts.get_or_create(address);
    let remaining = account.balance + account.locked;

    // Remaining balance goes to insurance fund (returned via results)
    // We adjust the last liquidation's PnL to include remaining balance
    if !results.is_empty() && remaining != 0 {
        // Add remaining balance to the insurance fund via the PnL
        if let Some(last) = results.last_mut() {
            last.pnl += remaining;
        }
        // Zero out the account
        account.balance = 0;
        account.locked = 0;
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_liquidation_long_position() {
        let mut accounts = AccountManager::new();

        // Create account with $5,000 and long 1 BTC at $50,000
        let account = accounts.get_or_create("trader");
        account.balance = 500_000; // $5,000
        account.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        let mut mark_prices = HashMap::new();

        // Price drops to $46,000 - should be liquidatable
        mark_prices.insert("BTC-USDT".to_string(), 4_600_000);

        assert!(accounts.get("trader").unwrap().is_liquidatable(&mark_prices, 500));

        // Run liquidation
        let results = check_and_liquidate(&mut accounts, &mark_prices);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol, "BTC-USDT");
        assert!(results[0].was_long);
        // PnL should be negative (loss)
        // Loss = (46000 - 50000) * 1 = -$4,000
        assert!(results[0].pnl < 0);

        // Position should be closed
        let account = accounts.get("trader").unwrap();
        assert_eq!(account.position("BTC-USDT").size, 0);
    }

    #[test]
    fn test_liquidation_short_position() {
        let mut accounts = AccountManager::new();

        // Create account with $5,000 and short 1 BTC at $50,000
        let account = accounts.get_or_create("trader");
        account.balance = 500_000; // $5,000
        account.apply_fill("BTC-USDT", false, 100_000_000, 5_000_000);

        let mut mark_prices = HashMap::new();

        // Price rises to $54,000 - should be liquidatable
        mark_prices.insert("BTC-USDT".to_string(), 5_400_000);

        assert!(accounts.get("trader").unwrap().is_liquidatable(&mark_prices, 500));

        // Run liquidation
        let results = check_and_liquidate(&mut accounts, &mark_prices);

        assert_eq!(results.len(), 1);
        assert!(!results[0].was_long);
        assert!(results[0].pnl < 0); // Loss

        // Position should be closed
        let account = accounts.get("trader").unwrap();
        assert_eq!(account.position("BTC-USDT").size, 0);
    }

    #[test]
    fn test_no_liquidation_when_healthy() {
        let mut accounts = AccountManager::new();

        // Create well-margined account
        let account = accounts.get_or_create("trader");
        account.balance = 1_000_000; // $10,000
        account.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        let mut mark_prices = HashMap::new();
        mark_prices.insert("BTC-USDT".to_string(), 4_900_000); // Small drop

        assert!(!accounts.get("trader").unwrap().is_liquidatable(&mark_prices, 500));

        let results = check_and_liquidate(&mut accounts, &mark_prices);
        assert!(results.is_empty());
    }
}
