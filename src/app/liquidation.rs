//! Liquidation Engine
//!
//! Checks accounts after each block and liquidates underwater positions.
//! Proceeds go to the insurance fund.
//!
//! ## Event-Driven Mode
//!
//! Use `check_and_liquidate_from_queue` with a `LiquidationQueue` for
//! O(k) checks per block instead of O(n) where k << n.
//!
//! ## Circuit Breaker
//!
//! To prevent liquidation cascades, use `check_and_liquidate_limited` which
//! limits the number of liquidations per block.

use std::collections::HashMap;

use crate::types::Price;

use super::accounts::AccountManager;
use super::liquidation_queue::LiquidationQueue;
use super::state::MAINTENANCE_MARGIN_BPS;
use super::Symbol;

/// Default maximum liquidations per block (circuit breaker)
pub const MAX_LIQUIDATIONS_PER_BLOCK: usize = 100;

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
    /// PnL from closing this position (positive = profit, negative = loss)
    pub pnl: i64,
    /// Whether the position was long or short
    pub was_long: bool,
    /// Remaining account balance transferred to insurance fund (only on last position)
    /// Positive = contribution to insurance fund, negative = insurance fund loss
    #[allow(dead_code)]
    pub insurance_fund_delta: i64,
}

/// Batch result from limited liquidation check (circuit breaker)
#[derive(Debug, Clone)]
pub struct LiquidationBatch {
    /// Liquidations performed
    pub results: Vec<LiquidationResult>,
    /// True if there are more accounts to check (hit limit)
    pub has_more: bool,
    /// Number of accounts checked (for metrics)
    pub checked_count: usize,
}

/// Check all accounts and liquidate underwater positions
///
/// Returns list of liquidations performed and total PnL for insurance fund
pub fn check_and_liquidate(
    accounts: &mut AccountManager,
    mark_prices: &HashMap<Symbol, Price>,
) -> Vec<LiquidationResult> {
    check_and_liquidate_limited(accounts, mark_prices, usize::MAX).results
}

/// Check accounts and liquidate with a maximum limit (circuit breaker)
///
/// Returns a LiquidationBatch containing results and whether more accounts remain.
/// Use this to prevent liquidation cascades from causing long block times.
///
/// Parameters:
/// - `max_liquidations`: Maximum number of accounts to liquidate per call
pub fn check_and_liquidate_limited(
    accounts: &mut AccountManager,
    mark_prices: &HashMap<Symbol, Price>,
    max_liquidations: usize,
) -> LiquidationBatch {
    let mut results = Vec::new();
    let mut liquidated_count = 0;

    // Collect addresses that need liquidation
    // (can't modify while iterating)
    let mut addresses_to_check: Vec<String> = accounts
        .all_accounts()
        .iter()
        .filter(|a| a.is_liquidatable(mark_prices, MAINTENANCE_MARGIN_BPS))
        .map(|a| a.address.clone())
        .collect();

    // CRITICAL: Sort for deterministic ordering across validators.
    // HashMap iteration order is non-deterministic, which would cause
    // consensus failures if different validators process liquidations
    // in different orders. DO NOT remove this sort.
    addresses_to_check.sort();

    let total_liquidatable = addresses_to_check.len();
    let checked_count = total_liquidatable;

    // Process accounts up to limit using partial liquidation
    for address in addresses_to_check {
        if liquidated_count >= max_liquidations {
            break;
        }

        // Use partial liquidation to close only enough positions to restore margin health
        let liquidations = liquidate_account_partial(accounts, &address, mark_prices);
        if !liquidations.is_empty() {
            liquidated_count += 1;
        }
        results.extend(liquidations);
    }

    let has_more = liquidated_count >= max_liquidations && liquidated_count < total_liquidatable;

    if !results.is_empty() {
        let total_pnl: i64 = results.iter().map(|r| r.pnl).sum();
        if has_more {
            tracing::warn!(
                count = results.len(),
                total_pnl,
                limit = max_liquidations,
                remaining = total_liquidatable - liquidated_count,
                "Liquidation limit reached - more accounts pending"
            );
        } else {
            tracing::info!(
                count = results.len(),
                total_pnl,
                "Liquidations processed"
            );
        }
    }

    LiquidationBatch {
        results,
        has_more,
        checked_count,
    }
}

/// Event-driven liquidation: check only accounts from the queue
///
/// This is more efficient than `check_and_liquidate` because it only
/// checks accounts that the queue has identified as potentially at risk.
///
/// Returns list of liquidations performed and updates the queue.
pub fn check_and_liquidate_from_queue(
    accounts: &mut AccountManager,
    mark_prices: &HashMap<Symbol, Price>,
    queue: &mut LiquidationQueue,
) -> Vec<LiquidationResult> {
    let mut results = Vec::new();

    // Get accounts to check this block (ordered by risk)
    let addresses_to_check = queue.get_accounts_to_check();

    for address in addresses_to_check {
        // Double-check if actually liquidatable (prices may have changed)
        let is_liquidatable = accounts
            .get(&address)
            .map(|a| a.is_liquidatable(mark_prices, MAINTENANCE_MARGIN_BPS))
            .unwrap_or(false);

        if is_liquidatable {
            let liquidations = liquidate_account(accounts, &address, mark_prices);

            // Remove liquidated account from queue
            if !liquidations.is_empty() {
                queue.remove_account(&address);
            }

            results.extend(liquidations);
        } else {
            // Account is healthy now, re-add with updated health
            queue.update_account(&address, accounts, mark_prices);
        }
    }

    if !results.is_empty() {
        let total_pnl: i64 = results.iter().map(|r| r.pnl).sum();
        tracing::info!(
            count = results.len(),
            total_pnl,
            "Liquidations processed (event-driven)"
        );
    }

    results
}

/// Partially liquidate an underwater account
///
/// SECURITY: Partial liquidation closes only enough positions to restore
/// margin health. This is fairer to users and reduces insurance fund impact.
/// Closes positions largest-first (by notional) until equity >= maintenance margin.
fn liquidate_account_partial(
    accounts: &mut AccountManager,
    address: &str,
    mark_prices: &HashMap<Symbol, Price>,
) -> Vec<LiquidationResult> {
    let mut results = Vec::new();

    // Get positions to liquidate with notional values, sorted largest-first
    let positions: Vec<(String, i64, i64, i64)> = {
        let account = match accounts.get(address) {
            Some(a) => a,
            None => return results,
        };

        let mut pos_with_notional: Vec<_> = account
            .positions
            .iter()
            .filter(|(_, pos)| pos.size != 0)
            .map(|(symbol, pos)| {
                let mark = mark_prices.get(symbol).copied().unwrap_or(pos.entry_price);
                // Use i128 to prevent overflow in notional calculation
                let notional = ((pos.size.abs() as i128 * mark as i128) / 100_000_000)
                    .clamp(0, i64::MAX as i128) as i64;
                (symbol.clone(), pos.size, pos.entry_price, notional)
            })
            .collect();

        // Sort by notional descending (close largest positions first)
        // CRITICAL: Add secondary sort by symbol for deterministic ordering.
        // If two positions have identical notional, HashMap iteration order
        // would be non-deterministic across validators, causing consensus failures.
        pos_with_notional.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));
        pos_with_notional
    };

    // Liquidate positions until health is restored
    for (symbol, size, entry_price, _notional) in positions {
        // Check if margin health is restored (after first liquidation at minimum)
        if !results.is_empty() {
            let account = match accounts.get(address) {
                Some(a) => a,
                None => break,
            };
            let equity = account.equity(mark_prices);
            let maintenance = account.maintenance_margin_required(mark_prices, MAINTENANCE_MARGIN_BPS);

            if equity >= maintenance {
                tracing::info!(
                    address,
                    equity,
                    maintenance,
                    positions_closed = results.len(),
                    "Partial liquidation: margin health restored"
                );
                break;
            }
        }

        let mark_price = match mark_prices.get(&symbol) {
            Some(&p) => p,
            None => continue, // Skip if no mark price
        };

        let was_long = size > 0;
        let abs_size = size.abs();

        // Calculate PnL: close at mark price
        // Use i128 to prevent overflow with large positions
        let price_diff = if was_long {
            mark_price - entry_price
        } else {
            entry_price - mark_price
        };
        let pnl_i128 = (abs_size as i128 * price_diff as i128) / 100_000_000;
        let pnl = pnl_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64;

        // Close the position by applying opposite fill
        let account = accounts.get_or_create(address);
        account.apply_fill(&symbol, !was_long, abs_size, mark_price);

        tracing::warn!(
            address,
            symbol,
            size,
            mark_price,
            pnl,
            "Position liquidated (partial)"
        );

        results.push(LiquidationResult {
            address: address.to_string(),
            symbol,
            size: abs_size,
            price: mark_price,
            pnl,
            was_long,
            insurance_fund_delta: 0, // Partial liquidation doesn't zero accounts
        });
    }

    // Only if fully liquidated (no positions remain), zero out and capture insurance delta
    let account = accounts.get_or_create(address);
    let has_positions = account.positions.values().any(|p| p.size != 0);

    if !has_positions && !results.is_empty() {
        let remaining = account.balance.saturating_add(account.locked);
        if let Some(last) = results.last_mut() {
            last.insurance_fund_delta = remaining;
        }
        if remaining != 0 {
            account.balance = 0;
            account.locked = 0;
        }
    }

    results
}

/// Liquidate all positions for an underwater account (full liquidation)
fn liquidate_account(
    accounts: &mut AccountManager,
    address: &str,
    mark_prices: &HashMap<Symbol, Price>,
) -> Vec<LiquidationResult> {
    let mut results = Vec::new();

    // Get positions to liquidate
    // CRITICAL: Sort by symbol for deterministic ordering across validators.
    // HashMap iteration order is non-deterministic, which would cause
    // consensus failures if different validators process liquidations
    // in different orders. DO NOT remove this sort.
    let positions: Vec<(String, i64, i64)> = {
        let account = match accounts.get(address) {
            Some(a) => a,
            None => return results,
        };

        let mut pos: Vec<_> = account
            .positions
            .iter()
            .filter(|(_, pos)| pos.size != 0)
            .map(|(symbol, pos)| (symbol.clone(), pos.size, pos.entry_price))
            .collect();
        pos.sort_by(|a, b| a.0.cmp(&b.0));
        pos
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
        // Use i128 to prevent overflow with large positions
        let price_diff = if was_long {
            mark_price - entry_price
        } else {
            entry_price - mark_price
        };
        let pnl_i128 = (abs_size as i128 * price_diff as i128) / 100_000_000;
        let pnl = pnl_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64;

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
            insurance_fund_delta: 0, // Will be set on last position
        });
    }

    // After liquidation, transfer remaining balance to insurance fund
    // and zero out the account
    let account = accounts.get_or_create(address);
    let remaining = account.balance.saturating_add(account.locked);

    // SECURITY: Insurance fund delta represents the NET remaining balance after all positions
    // are liquidated. Individual position PnL is tracked separately in each LiquidationResult.
    // We capture the final account state on the LAST position because:
    // 1. For full liquidation (this function), all positions are closed at once
    // 2. The remaining balance only makes sense after all PnL is realized
    // 3. Partial liquidation (liquidate_account_partial) handles incremental accounting
    if !results.is_empty() {
        // Record insurance fund delta on the last liquidation result
        if let Some(last) = results.last_mut() {
            last.insurance_fund_delta = remaining;
        }

        // Zero out the account
        if remaining != 0 {
            account.balance = 0;
            account.locked = 0;
        }
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

    #[test]
    fn test_liquidation_circuit_breaker() {
        let mut accounts = AccountManager::new();

        // Create 5 underwater accounts
        for i in 0..5 {
            let account = accounts.get_or_create(&format!("trader{}", i));
            account.balance = 500_000; // $5,000
            account.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);
        }

        let mut mark_prices = HashMap::new();
        // Price drops to $46,000 - all should be liquidatable
        mark_prices.insert("BTC-USDT".to_string(), 4_600_000);

        // Limit to 2 liquidations
        let batch = check_and_liquidate_limited(&mut accounts, &mark_prices, 2);

        // Should have liquidated exactly 2 accounts
        // Each account has 1 position, so 2 results
        assert_eq!(batch.results.len(), 2);
        assert!(batch.has_more);
        assert_eq!(batch.checked_count, 5);

        // Process remaining 3
        let batch2 = check_and_liquidate_limited(&mut accounts, &mark_prices, 10);
        assert_eq!(batch2.results.len(), 3);
        assert!(!batch2.has_more);
    }

    #[test]
    fn test_liquidation_batch_no_limit() {
        let mut accounts = AccountManager::new();

        // Create 3 underwater accounts
        for i in 0..3 {
            let account = accounts.get_or_create(&format!("trader{}", i));
            account.balance = 500_000;
            account.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);
        }

        let mut mark_prices = HashMap::new();
        mark_prices.insert("BTC-USDT".to_string(), 4_600_000);

        // No limit (max usize)
        let batch = check_and_liquidate_limited(&mut accounts, &mark_prices, usize::MAX);

        assert_eq!(batch.results.len(), 3);
        assert!(!batch.has_more);
    }

    #[test]
    fn test_partial_liquidation_stops_when_healthy() {
        let mut accounts = AccountManager::new();

        // Create account with large balance and 2 positions
        // Position 1: 0.5 BTC long at $50,000 (notional = $25,000)
        // Position 2: 0.3 BTC long at $50,000 (notional = $15,000)
        let account = accounts.get_or_create("trader");
        account.balance = 1_200_000; // $12,000
        account.apply_fill("BTC-USDT", true, 50_000_000, 5_000_000); // 0.5 BTC
        account.apply_fill("ETH-USDT", true, 30_000_000, 5_000_000); // Using same price for simplicity

        let mut mark_prices = HashMap::new();

        // Price drops significantly - both positions underwater
        // BTC drops to $35,000 - unrealized PnL on 0.5 BTC = -$7,500
        // ETH drops to $35,000 - unrealized PnL on 0.3 ETH = -$4,500
        // Total unrealized = -$12,000
        // Equity = $12,000 - $12,000 = $0
        // Maintenance on $17,500 + $10,500 = $28,000 @ 5% = $1,400
        // So equity < maintenance, should liquidate
        mark_prices.insert("BTC-USDT".to_string(), 3_500_000);
        mark_prices.insert("ETH-USDT".to_string(), 3_500_000);

        assert!(accounts.get("trader").unwrap().is_liquidatable(&mark_prices, 500));

        // Run partial liquidation
        let results = check_and_liquidate(&mut accounts, &mark_prices);

        // Should have liquidated at least one position
        assert!(!results.is_empty());

        // The key invariant: after partial liquidation, either:
        // 1. Account is healthy (equity >= maintenance), OR
        // 2. Account has no positions left (fully liquidated)
        let account = accounts.get("trader").unwrap();
        let has_positions = account.positions.values().any(|p| p.size != 0);

        if has_positions {
            // If there are positions, should be healthy now
            assert!(!account.is_liquidatable(&mark_prices, 500));
        }
        // If no positions, that's fine too (fully liquidated)
    }
}
