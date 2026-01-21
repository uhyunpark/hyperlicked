//! Auto-Deleveraging (ADL)
//!
//! Last-resort mechanism when liquidation losses exceed the insurance fund.
//! Instead of the insurance fund going negative, losses are distributed to
//! profitable positions on the opposite side of the market.
//!
//! ## How ADL Works
//!
//! 1. Trigger: Liquidation loss exceeds insurance fund balance
//! 2. Target: Profitable positions on the opposite side
//! 3. Priority: Most profitable (largest unrealized PnL) first
//! 4. Distribution: Proportional to position size
//! 5. Outcome: Profitable positions reduced to cover the loss

use crate::app::accounts::AccountManager;
use crate::types::Price;

/// ADL-specific errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum ADLError {
    #[error("no counterparty positions available")]
    NoCounterparties,
    #[error("insufficient size to cover loss")]
    InsufficientSize,
    #[error("account not found: {0}")]
    AccountNotFound(String),
    #[error("no mark price for symbol: {0}")]
    NoMarkPrice(String),
}

/// A candidate for auto-deleveraging
#[derive(Debug, Clone)]
pub struct ADLCandidate {
    /// Account address
    pub address: String,
    /// Position size (always positive here, direction known from context)
    pub size: i64,
    /// Entry price
    pub entry_price: i64,
    /// Unrealized PnL (positive = profitable)
    pub unrealized_pnl: i64,
}

/// Result of a single ADL operation against one counterparty
#[derive(Debug, Clone)]
pub struct ADLResult {
    /// Account that was deleveraged
    pub address: String,
    /// Symbol of the position
    pub symbol: String,
    /// Size reduced (absolute value)
    pub size_reduced: i64,
    /// Price at which position was closed
    pub close_price: i64,
    /// Realized PnL from the ADL (typically positive for the deleveraged party)
    pub realized_pnl: i64,
    /// Address of the liquidated account that triggered ADL
    pub triggering_liquidation: String,
    /// Timestamp
    pub timestamp: u64,
}

/// Summary of ADL operations for a single liquidation
#[derive(Debug, Clone)]
pub struct ADLSummary {
    /// All ADL events for this liquidation
    pub events: Vec<ADLResult>,
    /// Total loss absorbed by ADL
    pub total_absorbed: i64,
}

/// Find profitable positions on the opposite side of a symbol
///
/// Returns candidates sorted by unrealized PnL (most profitable first)
pub fn find_adl_candidates(
    accounts: &AccountManager,
    symbol: &str,
    mark_price: Price,
    target_shorts: bool, // true = find profitable shorts, false = find profitable longs
    exclude_address: &str,
) -> Vec<ADLCandidate> {
    let mut candidates = Vec::new();

    for account in accounts.all_accounts() {
        // Skip the liquidated account
        if account.address == exclude_address {
            continue;
        }

        if let Some(position) = account.positions.get(symbol) {
            // Skip zero positions
            if position.size == 0 {
                continue;
            }

            let is_short = position.size < 0;

            // Only include positions on the target side
            if target_shorts != is_short {
                continue;
            }

            let abs_size = position.size.abs();

            // Calculate unrealized PnL
            let pnl = if is_short {
                // Short: profit when price drops
                // PnL = (entry - mark) * size / 100_000_000
                (position.entry_price - mark_price) * abs_size / 100_000_000
            } else {
                // Long: profit when price rises
                // PnL = (mark - entry) * size / 100_000_000
                (mark_price - position.entry_price) * abs_size / 100_000_000
            };

            // Only include profitable positions
            if pnl > 0 {
                candidates.push(ADLCandidate {
                    address: account.address.clone(),
                    size: abs_size,
                    entry_price: position.entry_price,
                    unrealized_pnl: pnl,
                });
            }
        }
    }

    // Sort by unrealized PnL descending (most profitable first)
    candidates.sort_by(|a, b| b.unrealized_pnl.cmp(&a.unrealized_pnl));

    candidates
}

/// Calculate how much size to reduce from each candidate to cover the loss
///
/// Distribution is proportional to position size among candidates.
/// Returns (address, size_to_reduce) pairs.
pub fn calculate_adl_distribution(
    candidates: &[ADLCandidate],
    mark_price: Price,
    loss_to_cover: i64,
) -> Vec<(String, i64)> {
    if candidates.is_empty() || loss_to_cover <= 0 || mark_price == 0 {
        return vec![];
    }

    let mut distribution = Vec::new();
    let mut remaining_loss = loss_to_cover;

    for candidate in candidates {
        if remaining_loss <= 0 {
            break;
        }

        // Calculate max value this position can absorb
        // Value = size * mark_price / 100_000_000 (convert to cents)
        let max_value = candidate.size * mark_price / 100_000_000;

        // How much of this position do we need?
        let value_needed = remaining_loss.min(max_value);
        let size_to_reduce = if max_value > 0 {
            (value_needed * 100_000_000) / mark_price
        } else {
            0
        };

        if size_to_reduce > 0 {
            distribution.push((candidate.address.clone(), size_to_reduce));
            remaining_loss -= value_needed;
        }
    }

    distribution
}

/// Execute ADL by reducing positions
///
/// Returns the ADL events for event emission.
pub fn execute_adl(
    accounts: &mut AccountManager,
    symbol: &str,
    distribution: &[(String, i64)],
    mark_price: Price,
    target_shorts: bool,
    triggering_address: &str,
    timestamp: u64,
) -> Vec<ADLResult> {
    let mut results = Vec::new();

    for (address, size_to_reduce) in distribution {
        let account = accounts.get_or_create(address);

        let position = account.position(symbol);
        if position.size == 0 {
            continue;
        }

        let is_short = position.size < 0;
        let entry_price = position.entry_price;

        // Calculate realized PnL for the ADL'd position
        let realized_pnl = if is_short {
            (entry_price - mark_price) * size_to_reduce / 100_000_000
        } else {
            (mark_price - entry_price) * size_to_reduce / 100_000_000
        };

        // Apply fill to close part of the position
        // For shorts being ADL'd: buy to close (is_buy = true)
        // For longs being ADL'd: sell to close (is_buy = false)
        account.apply_fill(symbol, target_shorts, *size_to_reduce, mark_price);

        tracing::warn!(
            address,
            symbol,
            size_reduced = size_to_reduce,
            mark_price,
            realized_pnl,
            triggering = triggering_address,
            "ADL executed"
        );

        results.push(ADLResult {
            address: address.clone(),
            symbol: symbol.to_string(),
            size_reduced: *size_to_reduce,
            close_price: mark_price,
            realized_pnl,
            triggering_liquidation: triggering_address.to_string(),
            timestamp,
        });
    }

    results
}

/// Main entry point: process ADL if needed for a liquidation
///
/// Returns ADLSummary if ADL was triggered, None otherwise.
pub fn process_adl_if_needed(
    accounts: &mut AccountManager,
    symbol: &str,
    liquidation_pnl: i64,      // Negative value = loss
    insurance_fund: i64,
    mark_price: Price,
    liquidated_was_long: bool, // true = liquidated position was long
    liquidated_address: &str,
    timestamp: u64,
) -> Option<ADLSummary> {
    // ADL only triggers when insurance fund can't cover the loss
    // liquidation_pnl is negative (loss), insurance_fund + pnl < 0 means shortfall
    let shortfall = -(insurance_fund + liquidation_pnl);
    if shortfall <= 0 {
        return None; // Insurance fund can cover the loss
    }

    tracing::warn!(
        symbol,
        liquidation_pnl,
        insurance_fund,
        shortfall,
        "ADL triggered - insurance fund insufficient"
    );

    // Find profitable positions on the opposite side
    // If liquidated was long, target profitable shorts (they won)
    // If liquidated was short, target profitable longs
    let target_shorts = liquidated_was_long;

    let candidates = find_adl_candidates(
        accounts,
        symbol,
        mark_price,
        target_shorts,
        liquidated_address,
    );

    if candidates.is_empty() {
        tracing::error!(symbol, "ADL failed: no counterparty positions available");
        return None;
    }

    // Calculate distribution
    let distribution = calculate_adl_distribution(&candidates, mark_price, shortfall);

    if distribution.is_empty() {
        tracing::error!(symbol, "ADL failed: could not calculate distribution");
        return None;
    }

    // Execute ADL
    let events = execute_adl(
        accounts,
        symbol,
        &distribution,
        mark_price,
        target_shorts,
        liquidated_address,
        timestamp,
    );

    let total_absorbed: i64 = events
        .iter()
        .map(|e| e.size_reduced * e.close_price / 100_000_000)
        .sum();

    tracing::info!(
        symbol,
        events_count = events.len(),
        total_absorbed,
        shortfall,
        "ADL completed"
    );

    Some(ADLSummary {
        events,
        total_absorbed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::accounts::AccountManager;

    fn setup_accounts() -> AccountManager {
        let mut accounts = AccountManager::new();

        // Liquidated account (will have underwater long position)
        let liquidated = accounts.get_or_create("liquidated");
        liquidated.balance = 100_000; // $1000
        liquidated.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000); // Long 1 BTC at $50k

        // Profitable short 1: large position
        let short1 = accounts.get_or_create("short1");
        short1.balance = 1_000_000;
        short1.apply_fill("BTC-USDT", false, 200_000_000, 5_000_000); // Short 2 BTC at $50k

        // Profitable short 2: smaller position
        let short2 = accounts.get_or_create("short2");
        short2.balance = 500_000;
        short2.apply_fill("BTC-USDT", false, 100_000_000, 5_000_000); // Short 1 BTC at $50k

        accounts
    }

    #[test]
    fn test_find_adl_candidates_shorts() {
        let accounts = setup_accounts();

        // Price dropped to $45k - shorts are profitable
        let candidates = find_adl_candidates(
            &accounts,
            "BTC-USDT",
            4_500_000,
            true, // target shorts
            "liquidated",
        );

        assert_eq!(candidates.len(), 2);
        // Both shorts have positive unrealized PnL
        assert!(candidates[0].unrealized_pnl > 0);
        assert!(candidates[1].unrealized_pnl > 0);
        // Sorted by PnL (most profitable first)
        assert!(candidates[0].unrealized_pnl >= candidates[1].unrealized_pnl);
    }

    #[test]
    fn test_find_adl_candidates_excludes_losing() {
        let accounts = setup_accounts();

        // Price rose to $55k - shorts are losing, shouldn't be candidates
        let candidates = find_adl_candidates(
            &accounts,
            "BTC-USDT",
            5_500_000,
            true,
            "liquidated",
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn test_calculate_distribution() {
        let candidates = vec![
            ADLCandidate {
                address: "short1".to_string(),
                size: 200_000_000,
                entry_price: 5_000_000,
                unrealized_pnl: 1_000_000,
            },
            ADLCandidate {
                address: "short2".to_string(),
                size: 100_000_000,
                entry_price: 5_000_000,
                unrealized_pnl: 500_000,
            },
        ];

        // Need to cover $500 loss at $45k mark price
        let distribution = calculate_adl_distribution(&candidates, 4_500_000, 50_000);

        assert!(!distribution.is_empty());
        // Total value should approximately cover the loss (allowing for rounding)
        let total_value: i64 = distribution
            .iter()
            .map(|(_, size)| size * 4_500_000 / 100_000_000)
            .sum();
        // Allow 1% variance due to integer rounding
        assert!(total_value >= 49_000, "Total value {} should be close to 50,000", total_value);
    }

    #[test]
    fn test_adl_not_triggered_when_insurance_sufficient() {
        let mut accounts = setup_accounts();

        // Small loss that insurance can cover
        let result = process_adl_if_needed(
            &mut accounts,
            "BTC-USDT",
            -50_000,   // $500 loss
            100_000,   // $1000 insurance fund
            4_500_000, // Mark price
            true,      // Liquidated was long
            "liquidated",
            0,
        );

        assert!(result.is_none());
    }

    #[test]
    fn test_adl_triggered_when_insurance_insufficient() {
        let mut accounts = setup_accounts();

        // Large loss that exceeds insurance
        let result = process_adl_if_needed(
            &mut accounts,
            "BTC-USDT",
            -500_000,  // $5000 loss
            100_000,   // $1000 insurance fund
            4_500_000, // Mark price $45k (shorts profitable)
            true,      // Liquidated was long
            "liquidated",
            12345,
        );

        assert!(result.is_some());
        let summary = result.unwrap();
        assert!(!summary.events.is_empty());
        assert!(summary.total_absorbed > 0);

        // Verify positions were reduced
        let short1 = accounts.get("short1").unwrap();
        let pos1 = short1.positions.get("BTC-USDT").unwrap();
        // Position should be reduced (was -200_000_000)
        assert!(pos1.size.abs() < 200_000_000);
    }
}
