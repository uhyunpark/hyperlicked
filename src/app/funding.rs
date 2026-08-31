//! Funding Rate Engine
//!
//! Implements perpetual funding mechanism to keep perp prices anchored to index.
//! Funding payments occur hourly between longs and shorts based on the premium.
//!
//! ## Formula (Hyperliquid-style)
//!
//! ```text
//! Premium Index = (Mid Price - Index Price) / Index Price
//! Funding Rate = Avg(Premium Index) + clamp(Interest - Premium, -0.05%, 0.05%)
//! Payment = |position_size| × index_price × funding_rate
//! ```
//!
//! ## Bootstrap Mode
//!
//! Without an external oracle, we use mark price as index price and calculate
//! premium from orderbook mid-price. This creates a self-stabilizing mechanism.

use super::accounts::AccountManager;
use super::orderbook::OrderBook;
use crate::types::Price;
use serde::{Deserialize, Serialize};

/// Funding errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum FundingError {
    #[error("invalid index price: {0}")]
    InvalidIndexPrice(i64),
    #[error("no orderbook for symbol: {0}")]
    NoOrderbook(String),
    #[error("funding rate out of bounds: {0}")]
    RateOutOfBounds(i64),
}

/// Per-user funding payment for WebSocket notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserFundingPayment {
    /// Trader address
    pub address: String,
    /// Symbol
    pub symbol: String,
    /// Payment in cents (positive = received, negative = paid)
    pub payment: i64,
    /// Position size at time of funding
    pub position_size: i64,
    /// Funding rate in basis points
    pub funding_rate_bps: i64,
    /// Timestamp
    pub timestamp: u64,
}

/// Result of funding application
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingResult {
    /// Symbol this funding was applied to
    pub symbol: String,
    /// Funding rate in basis points (positive = longs pay shorts)
    pub funding_rate_bps: i64,
    /// Signed total attributed to longs (positive = paid, negative = received)
    pub longs_paid: i64,
    /// Signed total attributed to shorts (positive = received, negative = paid)
    pub shorts_received: i64,
    /// Timestamp of funding
    pub timestamp: u64,
    /// Per-user funding payments (for WebSocket notifications)
    pub user_payments: Vec<UserFundingPayment>,
}

/// Sample premium index from orderbook
///
/// Premium = (mid_price - index_price) / index_price × 10000 (in bps)
///
/// Returns premium in basis points (100 = 1%)
pub fn sample_premium(book: &OrderBook, index_price: Price) -> i64 {
    if index_price == 0 {
        return 0;
    }

    // Get mid price from orderbook
    let best_bid = book.best_bid();
    let best_ask = book.best_ask();

    // If no market, premium is 0
    if best_bid.is_none() || best_ask.is_none() {
        return 0;
    }

    let bid = best_bid.unwrap();
    let ask = best_ask.unwrap();

    // Mid price = (bid + ask) / 2
    let mid_price = (bid + ask) / 2;

    // Premium in bps = (mid - index) / index * 10000
    // Use i128 to prevent overflow: price_diff × 10000 can exceed i64
    let price_diff = mid_price as i128 - index_price as i128;
    let premium_i128 = (price_diff * 10000) / index_price as i128;
    premium_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// Sample premium with oracle weighting for manipulation resistance
///
/// Blends oracle-derived premium with mid-price premium to reduce
/// susceptibility to orderbook manipulation with thin liquidity.
///
/// Formula:
/// ```text
/// blended_premium = (oracle_weight * oracle_premium + (10000 - oracle_weight) * mid_premium) / 10000
/// ```
///
/// Parameters:
/// - `book`: The orderbook for mid-price calculation
/// - `index_price`: External index/oracle price
/// - `oracle_price`: Oracle spot price (can be same as index_price if no separate oracle)
/// - `oracle_weight_bps`: Weight for oracle premium in basis points (5000 = 50%)
///
/// Returns premium in basis points (100 = 1%)
pub fn sample_premium_with_oracle(
    book: &OrderBook,
    index_price: Price,
    oracle_price: Price,
    oracle_weight_bps: i64,
) -> i64 {
    if index_price == 0 || oracle_price == 0 {
        return 0;
    }

    // Clamp oracle weight to [0, 10000]
    let weight = oracle_weight_bps.clamp(0, 10000);

    // Get mid-price premium
    let mid_premium = sample_premium(book, index_price);

    // Calculate oracle-derived premium
    // This is the premium if we used oracle price as the reference
    // oracle_premium = (mark - oracle) / oracle * 10000
    // where mark is derived from the orderbook
    let best_bid = book.best_bid();
    let best_ask = book.best_ask();

    let oracle_premium = if best_bid.is_some() && best_ask.is_some() {
        let bid = best_bid.unwrap();
        let ask = best_ask.unwrap();
        let mid_price = (bid + ask) / 2;
        // Use i128 to prevent overflow
        let price_diff = mid_price as i128 - oracle_price as i128;
        let premium_i128 = (price_diff * 10000) / oracle_price as i128;
        premium_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    } else {
        0
    };

    // Blend: weighted average
    // blended = (weight * oracle_premium + (10000 - weight) * mid_premium) / 10000
    // Use i128 to prevent overflow in weighted sum
    let weighted_oracle = weight as i128 * oracle_premium as i128;
    let weighted_mid = (10000 - weight) as i128 * mid_premium as i128;
    let blended_i128 = (weighted_oracle + weighted_mid) / 10000;
    blended_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// Calculate funding rate from average premium
///
/// Formula: funding_rate = avg_premium + clamp(interest - avg_premium, -50, 50)
///
/// This creates a dampening effect:
/// - If premium is very high, the clamp limits additional interest
/// - If premium is low, interest rate adds a baseline
///
/// Returns funding rate in basis points
pub fn calculate_funding_rate(
    avg_premium_bps: i64,
    interest_rate_bps: i64,
    max_rate_bps: i64,
) -> i64 {
    // Clamp component: interest - premium, clamped to [-50, 50] bps
    let clamp_value = (interest_rate_bps - avg_premium_bps).clamp(-50, 50);

    // Total funding rate
    let funding_rate = avg_premium_bps + clamp_value;

    // Cap the total rate
    funding_rate.clamp(-max_rate_bps, max_rate_bps)
}

/// A funding entry collected during the settlement preflight.
///
/// Funding is first calculated for every position and then settled in one
/// deterministic pass.  Keeping the nominal and capped amounts separate is
/// important: an insolvent payer must not make an unbacked receiver credit.
#[derive(Debug)]
struct FundingEntry {
    address: String,
    position_size: i64,
    payer_capacity: i128,
    receiver_capacity: i128,
    settled_payment: i64,
}

fn clamp_i128_to_i64(value: i128) -> i64 {
    value.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// Return the maximum amount that can be collected from a negative payment.
///
/// A payer may only spend positive free collateral.  The cumulative funding
/// field is bounded as well, so it is included in the cap to keep metadata
/// exactly representative of the settled amount even at integer limits.
fn payer_capacity(nominal_payment: i64, balance: i64, cumulative_funding: i64) -> i128 {
    if nominal_payment >= 0 {
        return 0;
    }

    let nominal_amount = -(nominal_payment as i128);
    let balance_capacity = (balance as i128).max(0);
    let metadata_capacity = cumulative_funding as i128 - i64::MIN as i128;

    nominal_amount
        .min(balance_capacity)
        .min(metadata_capacity.max(0))
}

/// Return the maximum amount that can be credited to a positive payment.
///
/// Both the account balance and cumulative funding are i64 values.  Capping
/// by their remaining headroom prevents a valid funding event from panicking
/// or silently saturating either consensus field.
fn receiver_capacity(nominal_payment: i64, balance: i64, cumulative_funding: i64) -> i128 {
    if nominal_payment <= 0 {
        return 0;
    }

    let nominal_amount = nominal_payment as i128;
    let balance_capacity = i64::MAX as i128 - balance as i128;
    let metadata_capacity = i64::MAX as i128 - cumulative_funding as i128;

    nominal_amount
        .min(balance_capacity.max(0))
        .min(metadata_capacity.max(0))
}

/// Allocate `target` units proportionally to non-negative `weights`.
///
/// Every allocation is bounded by its corresponding weight.  Integer
/// division leaves a remainder smaller than the number of entries; those
/// units go to the largest fractional remainders, with the original (already
/// address-sorted) index as the deterministic tie-breaker.
fn allocate_pro_rata(weights: &[i128], target: i128) -> Vec<i64> {
    let total_weight = weights.iter().fold(0i128, |total, weight| {
        total.saturating_add((*weight).max(0))
    });
    let target = target.clamp(0, total_weight);
    let mut allocations = vec![0i64; weights.len()];

    if target == 0 || total_weight == 0 {
        return allocations;
    }

    // Monetary values are i64 and the number of accounts in a state is
    // bounded by available memory, so this product fits in i128 in normal
    // operation.  If an artificially enormous state exceeds that bound,
    // preserve conservation with a deterministic bounded fallback rather
    // than panicking during block execution.
    let mut remainders = Vec::with_capacity(weights.len());
    let mut distributed = 0i128;
    for (index, weight) in weights.iter().enumerate() {
        let weight = (*weight).max(0);
        let Some(numerator) = target.checked_mul(weight) else {
            let mut remaining = target;
            for (index, weight) in weights.iter().enumerate() {
                let amount = remaining.min((*weight).max(0));
                allocations[index] = clamp_i128_to_i64(amount);
                remaining -= amount;
                if remaining == 0 {
                    break;
                }
            }
            return allocations;
        };
        let whole = numerator / total_weight;
        let remainder = numerator % total_weight;
        allocations[index] = clamp_i128_to_i64(whole);
        distributed = distributed.saturating_add(whole);
        remainders.push((remainder, index));
    }

    let remainder_units = target.saturating_sub(distributed);
    remainders.sort_by(
        |(left_remainder, left_index), (right_remainder, right_index)| {
            right_remainder
                .cmp(left_remainder)
                .then_with(|| left_index.cmp(right_index))
        },
    );

    // At most one unit is left for each non-zero weight after flooring.  The
    // guard also keeps the cast safe if a malformed/intermediate state ever
    // causes the arithmetic above to saturate.
    let remainder_units = remainder_units.min(remainders.len() as i128) as usize;
    for (_, index) in remainders.into_iter().take(remainder_units) {
        allocations[index] = allocations[index].saturating_add(1);
    }

    allocations
}

/// Apply funding to all positions in a symbol
///
/// Iterates through all accounts with positions in the symbol and applies
/// the funding payment based on position size and direction.
///
/// Returns FundingResult with totals and per-user payments
pub fn apply_funding(
    accounts: &mut AccountManager,
    symbol: &str,
    funding_rate_bps: i64,
    index_price: Price,
    timestamp: u64,
) -> FundingResult {
    // Build the complete preflight from immutable account snapshots.  The
    // AccountManager uses a HashMap internally, so sort explicitly before any
    // allocation or event construction.
    let mut entries: Vec<FundingEntry> = accounts
        .all_accounts()
        .into_iter()
        .filter_map(|account| {
            let position = account.positions.get(symbol)?;
            if position.size == 0 {
                return None;
            }

            let nominal_payment = position.funding_payment(funding_rate_bps, index_price);
            Some(FundingEntry {
                address: account.address,
                position_size: position.size,
                payer_capacity: payer_capacity(
                    nominal_payment,
                    account.balance,
                    position.cumulative_funding,
                ),
                receiver_capacity: receiver_capacity(
                    nominal_payment,
                    account.balance,
                    position.cumulative_funding,
                ),
                settled_payment: 0,
            })
        })
        .collect();
    entries.sort_by(|left, right| left.address.cmp(&right.address));

    let payer_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.payer_capacity > 0)
        .map(|(index, _)| index)
        .collect();
    let receiver_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.receiver_capacity > 0)
        .map(|(index, _)| index)
        .collect();

    let payer_weights: Vec<i128> = payer_indices
        .iter()
        .map(|index| entries[*index].payer_capacity)
        .collect();
    let receiver_weights: Vec<i128> = receiver_indices
        .iter()
        .map(|index| entries[*index].receiver_capacity)
        .collect();

    // No counterpart means no transfer.  If one side is insolvent or cannot
    // represent another credit, only the amount backed by both sides moves.
    let payer_total = payer_weights
        .iter()
        .fold(0i128, |total, amount| total.saturating_add(*amount));
    let receiver_total = receiver_weights
        .iter()
        .fold(0i128, |total, amount| total.saturating_add(*amount));
    // FundingResult and each account's balance use i64 amounts.  Keep one
    // event's aggregate within that representation so its totals remain
    // exact even when many individually valid accounts participate.
    let settlement_total = payer_total.min(receiver_total).min(i64::MAX as i128);

    let payer_allocations = allocate_pro_rata(&payer_weights, settlement_total);
    for (allocation_index, entry_index) in payer_indices.iter().enumerate() {
        entries[*entry_index].settled_payment = -payer_allocations[allocation_index];
    }

    let receiver_allocations = allocate_pro_rata(&receiver_weights, settlement_total);
    for (allocation_index, entry_index) in receiver_indices.iter().enumerate() {
        entries[*entry_index].settled_payment = receiver_allocations[allocation_index];
    }

    let mut longs_paid: i128 = 0;
    let mut shorts_received: i128 = 0;
    let mut user_payments: Vec<UserFundingPayment> = Vec::with_capacity(entries.len());

    // Commit the preflight result.  The planned capacities make this checked
    // addition safe; the i128 clamp is a final defensive guard for malformed
    // state and keeps execution from panicking on integer overflow.
    for entry in entries {
        let account = accounts.get_or_create(&entry.address);
        if let Some(position) = account.positions.get_mut(symbol) {
            let balance = account.balance as i128;
            let actual_payment = clamp_i128_to_i64(
                (entry.settled_payment as i128)
                    .clamp(i64::MIN as i128 - balance, i64::MAX as i128 - balance),
            );

            account.balance = account
                .balance
                .checked_add(actual_payment)
                .unwrap_or_else(|| {
                    if actual_payment < 0 {
                        i64::MIN
                    } else {
                        i64::MAX
                    }
                });
            position.record_funding(actual_payment, timestamp);

            // Preserve the existing signed side-total semantics: for a
            // positive rate longs_paid/shorts_received are positive, while a
            // reversed rate makes the corresponding side total negative.
            if entry.position_size > 0 {
                longs_paid = longs_paid.saturating_sub(actual_payment as i128);
            } else {
                shorts_received = shorts_received.saturating_add(actual_payment as i128);
            }

            user_payments.push(UserFundingPayment {
                address: entry.address,
                symbol: symbol.to_string(),
                payment: actual_payment,
                position_size: entry.position_size,
                funding_rate_bps,
                timestamp,
            });
        }
    }

    let longs_paid = clamp_i128_to_i64(longs_paid);
    let shorts_received = clamp_i128_to_i64(shorts_received);

    tracing::info!(
        symbol,
        funding_rate_bps,
        longs_paid,
        shorts_received,
        "Funding applied"
    );

    FundingResult {
        symbol: symbol.to_string(),
        funding_rate_bps,
        longs_paid,
        shorts_received,
        timestamp,
        user_payments,
    }
}

/// Calculate average of premium samples
///
/// Returns average in basis points
pub fn average_premium(samples: &[i64]) -> i64 {
    if samples.is_empty() {
        return 0;
    }
    let sum: i64 = samples.iter().sum();
    sum / samples.len() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::MarketConfig;

    fn make_orderbook_with_spread(bid: i64, ask: i64) -> OrderBook {
        use super::super::orderbook::{Order, OrderType, Side};

        let mut book = OrderBook::new("BTC-USDT".to_string());
        let config = MarketConfig::default();

        // Place bid order
        let bid_order = Order {
            id: "bid1".to_string(),
            trader: "maker".to_string(),
            symbol: "BTC-USDT".to_string(),
            side: Side::Bid,
            price: bid,
            size: 100_000_000,
            original_size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
            timestamp: 0,
            locked_margin: 0,
        };
        let _ = book.place(bid_order, &config);

        // Place ask order
        let ask_order = Order {
            id: "ask1".to_string(),
            trader: "maker".to_string(),
            symbol: "BTC-USDT".to_string(),
            side: Side::Ask,
            price: ask,
            size: 100_000_000,
            original_size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
            timestamp: 0,
            locked_margin: 0,
        };
        let _ = book.place(ask_order, &config);

        book
    }

    #[test]
    fn test_sample_premium_balanced() {
        // bid=49500, ask=50500 -> mid=50000
        let book = make_orderbook_with_spread(4_950_000, 5_050_000);

        // Index at $50,000 -> mid = 50000, premium = 0
        let premium = sample_premium(&book, 5_000_000);
        assert_eq!(premium, 0); // Mid equals index
    }

    #[test]
    fn test_sample_premium_positive() {
        // bid=50500, ask=51500 -> mid=51000
        let book = make_orderbook_with_spread(5_050_000, 5_150_000);

        // Index at $50,000 -> mid = 51000, premium = 2%
        let premium = sample_premium(&book, 5_000_000);
        assert_eq!(premium, 200); // 2% = 200 bps
    }

    #[test]
    fn test_calculate_funding_rate_basic() {
        // Premium = 10 bps (0.1%), interest = 1 bps
        // clamp(1 - 10, -50, 50) = -9
        // funding = 10 + (-9) = 1 bps
        let rate = calculate_funding_rate(10, 1, 400);
        assert_eq!(rate, 1);
    }

    #[test]
    fn test_calculate_funding_rate_clamp() {
        // Premium = 100 bps (1%), interest = 1 bps
        // clamp(1 - 100, -50, 50) = -50
        // funding = 100 + (-50) = 50 bps
        let rate = calculate_funding_rate(100, 1, 400);
        assert_eq!(rate, 50);
    }

    #[test]
    fn test_calculate_funding_rate_max_cap() {
        // Very high premium, should be capped at max
        let rate = calculate_funding_rate(500, 1, 400);
        assert_eq!(rate, 400);
    }

    #[test]
    fn test_apply_funding_transfers() {
        let mut accounts = AccountManager::new();

        // Long trader: 1 BTC at $50k with $10k balance
        let long = accounts.get_or_create("long_trader");
        long.balance = 1_000_000; // $10k
        long.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        // Short trader: 1 BTC at $50k with $10k balance
        let short = accounts.get_or_create("short_trader");
        short.balance = 1_000_000; // $10k
        short.apply_fill("BTC-USDT", false, 100_000_000, 5_000_000);

        // Apply 1% funding rate (100 bps) - longs pay shorts
        let result = apply_funding(
            &mut accounts,
            "BTC-USDT",
            100,       // 1% = 100 bps
            5_000_000, // $50k index
            1000,      // timestamp
        );

        // Payment per side = 1 BTC * $50k * 1% = $500
        assert_eq!(result.longs_paid, 500_00); // $500 in cents
        assert_eq!(result.shorts_received, 500_00);

        // Long should have paid $500
        let long = accounts.get("long_trader").unwrap();
        assert_eq!(long.balance, 1_000_000 - 500_00); // $10k - $500

        // Short should have received $500
        let short = accounts.get("short_trader").unwrap();
        assert_eq!(short.balance, 1_000_000 + 500_00); // $10k + $500
    }

    #[test]
    fn test_average_premium() {
        let samples = vec![10, 20, 30, 40];
        assert_eq!(average_premium(&samples), 25);

        let empty: Vec<i64> = vec![];
        assert_eq!(average_premium(&empty), 0);
    }

    #[test]
    fn test_funding_cap_at_balance() {
        let mut accounts = AccountManager::new();

        // Long trader with very little balance
        let long = accounts.get_or_create("poor_long");
        long.balance = 100; // Only $1
        long.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        // Short trader with plenty of balance
        let short = accounts.get_or_create("short_trader");
        short.balance = 1_000_000;
        short.apply_fill("BTC-USDT", false, 100_000_000, 5_000_000);

        // Apply 1% funding rate - longs pay shorts
        // Long would owe $500 but only has $1
        let result = apply_funding(&mut accounts, "BTC-USDT", 100, 5_000_000, 1000);

        // Long should only pay what they have
        let long = accounts.get("poor_long").unwrap();
        assert!(
            long.balance >= 0,
            "Balance should not go negative, got {}",
            long.balance
        );

        // Total paid by longs should be capped
        assert!(
            result.longs_paid <= 100,
            "Longs paid {} should be capped at balance 100",
            result.longs_paid
        );
    }

    #[test]
    fn funding_settlement_conserves_insolvent_payer_amount() {
        let mut accounts = AccountManager::new();

        let long = accounts.get_or_create("poor_long");
        long.balance = 100;
        long.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        let short = accounts.get_or_create("short_trader");
        short.balance = 1_000_000;
        short.apply_fill("BTC-USDT", false, 100_000_000, 5_000_000);

        let total_before: i128 = accounts
            .all_accounts()
            .iter()
            .map(|account| account.balance as i128)
            .sum();
        let result = apply_funding(&mut accounts, "BTC-USDT", 100, 5_000_000, 1000);
        let total_after: i128 = accounts
            .all_accounts()
            .iter()
            .map(|account| account.balance as i128)
            .sum();

        // The payer's nominal $500 obligation is capped at $1, and only that
        // backed amount is credited to the receiver.
        assert_eq!(result.longs_paid, 100);
        assert_eq!(result.shorts_received, 100);
        assert_eq!(total_before, total_after);
        assert_eq!(
            result.user_payments.iter().map(|p| p.payment).sum::<i64>(),
            0
        );
        assert_eq!(
            accounts
                .get("poor_long")
                .unwrap()
                .position("BTC-USDT")
                .cumulative_funding,
            -100
        );
        assert_eq!(
            accounts
                .get("short_trader")
                .unwrap()
                .position("BTC-USDT")
                .cumulative_funding,
            100
        );
    }

    #[test]
    fn funding_settlement_handles_reversed_rate_without_minting() {
        let mut accounts = AccountManager::new();

        let long = accounts.get_or_create("long_trader");
        long.balance = 0;
        long.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        let short = accounts.get_or_create("poor_short");
        short.balance = 100;
        short.apply_fill("BTC-USDT", false, 100_000_000, 5_000_000);

        let result = apply_funding(&mut accounts, "BTC-USDT", -100, 5_000_000, 1000);

        // Negative funding reverses the direction: the short pays the backed
        // $1 and the long receives exactly $1.
        assert_eq!(result.longs_paid, -100);
        assert_eq!(result.shorts_received, -100);
        assert_eq!(
            result.user_payments.iter().map(|p| p.payment).sum::<i64>(),
            0
        );
        assert_eq!(accounts.get("long_trader").unwrap().balance, 100);
        assert_eq!(accounts.get("poor_short").unwrap().balance, 0);
        assert_eq!(
            accounts
                .get("long_trader")
                .unwrap()
                .position("BTC-USDT")
                .cumulative_funding,
            100
        );
        assert_eq!(
            accounts
                .get("poor_short")
                .unwrap()
                .position("BTC-USDT")
                .cumulative_funding,
            -100
        );
    }

    #[test]
    fn funding_settlement_assigns_rounding_remainder_by_address() {
        let mut accounts = AccountManager::new();

        // At this size/price/rate the nominal payment is exactly one cent.
        let payer = accounts.get_or_create("payer");
        payer.balance = 1;
        payer.apply_fill("BTC-USDT", true, 200_000, 5_000_000);

        let receiver_b = accounts.get_or_create("receiver_b");
        receiver_b.apply_fill("BTC-USDT", false, 200_000, 5_000_000);
        let receiver_a = accounts.get_or_create("receiver_a");
        receiver_a.apply_fill("BTC-USDT", false, 200_000, 5_000_000);

        let result = apply_funding(&mut accounts, "BTC-USDT", 1, 5_000_000, 1000);

        // One cent cannot be split evenly.  Equal fractional remainders are
        // resolved by canonical address order, so receiver_a gets the unit.
        assert_eq!(result.user_payments[0].address, "payer");
        assert_eq!(result.user_payments[1].address, "receiver_a");
        assert_eq!(result.user_payments[2].address, "receiver_b");
        assert_eq!(result.user_payments[0].payment, -1);
        assert_eq!(result.user_payments[1].payment, 1);
        assert_eq!(result.user_payments[2].payment, 0);
        assert_eq!(result.longs_paid, 1);
        assert_eq!(result.shorts_received, 1);
        assert_eq!(
            result.user_payments.iter().map(|p| p.payment).sum::<i64>(),
            0
        );
        assert_eq!(accounts.get("payer").unwrap().balance, 0);
        assert_eq!(accounts.get("receiver_a").unwrap().balance, 1);
        assert_eq!(accounts.get("receiver_b").unwrap().balance, 0);
    }

    #[test]
    fn pro_rata_allocation_is_bounded_and_conserves_remainder() {
        let weights = [2, 3, 5];
        let allocations = allocate_pro_rata(&weights, 7);

        assert_eq!(
            allocations
                .iter()
                .map(|amount| *amount as i128)
                .sum::<i128>(),
            7
        );
        assert!(allocations
            .iter()
            .zip(weights)
            .all(|(allocation, weight)| { *allocation >= 0 && i128::from(*allocation) <= weight }));
        // 7 * [2, 3, 5] / 10 = [1.4, 2.1, 3.5].  The leftover unit goes to
        // the largest fractional remainder.
        assert_eq!(allocations, vec![1, 2, 4]);
    }

    #[test]
    fn funding_settlement_does_not_credit_from_negative_balance() {
        let mut accounts = AccountManager::new();

        let long = accounts.get_or_create("insolvent_long");
        long.balance = -100;
        long.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        let short = accounts.get_or_create("short_trader");
        short.balance = 1_000_000;
        short.apply_fill("BTC-USDT", false, 100_000_000, 5_000_000);

        let result = apply_funding(&mut accounts, "BTC-USDT", 100, 5_000_000, 1000);

        assert_eq!(result.longs_paid, 0);
        assert_eq!(result.shorts_received, 0);
        assert_eq!(
            result.user_payments.iter().map(|p| p.payment).sum::<i64>(),
            0
        );
        assert_eq!(accounts.get("insolvent_long").unwrap().balance, -100);
        assert_eq!(accounts.get("short_trader").unwrap().balance, 1_000_000);
        assert_eq!(
            accounts
                .get("insolvent_long")
                .unwrap()
                .position("BTC-USDT")
                .last_funding_timestamp,
            1000
        );
    }

    #[test]
    fn funding_result_root_is_independent_of_account_insertion_order() {
        use crate::types::{CommitmentV2, EventRecord, EventType};

        fn accounts_in_order(addresses: &[&str]) -> AccountManager {
            let mut accounts = AccountManager::new();
            for address in addresses {
                let account = accounts.get_or_create(address);
                account.balance = 1_000_000;
                account.apply_fill(
                    "BTC-USDT",
                    address.starts_with("long"),
                    100_000_000,
                    5_000_000,
                );
            }
            accounts
        }

        fn result_root(result: &FundingResult) -> [u8; 32] {
            let event = EventRecord::from_bincode(0, EventType::FUNDING, result)
                .expect("funding payload encodes");
            CommitmentV2::new_with_system_events(Vec::new(), vec![event])
                .expect("commitment validates")
                .root()
                .expect("commitment root computes")
        }

        let mut first = accounts_in_order(&["long_b", "short_a", "long_a"]);
        let mut second = accounts_in_order(&["long_a", "long_b", "short_a"]);

        let first_result = apply_funding(&mut first, "BTC-USDT", 100, 5_000_000, 1000);
        let second_result = apply_funding(&mut second, "BTC-USDT", 100, 5_000_000, 1000);

        assert_eq!(result_root(&first_result), result_root(&second_result));
        assert_eq!(
            first_result
                .user_payments
                .iter()
                .map(|payment| payment.address.as_str())
                .collect::<Vec<_>>(),
            vec!["long_a", "long_b", "short_a"]
        );
        for address in ["long_a", "long_b", "short_a"] {
            assert_eq!(
                first.get(address).map(|account| account.balance),
                second.get(address).map(|account| account.balance)
            );
            assert_eq!(
                first
                    .get(address)
                    .map(|account| account.position("BTC-USDT").cumulative_funding),
                second
                    .get(address)
                    .map(|account| account.position("BTC-USDT").cumulative_funding)
            );
        }
    }

    #[test]
    fn test_oracle_weighted_premium_equal_weights() {
        // bid=51000, ask=52000 -> mid=51500 (3% above 50k)
        let book = make_orderbook_with_spread(5_100_000, 5_200_000);

        // Index at $50k, oracle at $50k
        // Mid premium = (51500 - 50000) / 50000 * 10000 = 300 bps (3%)
        let mid_premium = sample_premium(&book, 5_000_000);
        assert_eq!(mid_premium, 300);

        // With oracle weight = 5000 (50%), oracle = index = $50k
        // Both give same premium, so blended should equal mid_premium
        let blended = sample_premium_with_oracle(&book, 5_000_000, 5_000_000, 5000);
        assert_eq!(blended, 300);
    }

    #[test]
    fn test_oracle_weighted_premium_different_oracle() {
        // bid=51000, ask=52000 -> mid=51500
        let book = make_orderbook_with_spread(5_100_000, 5_200_000);

        // Index at $50k (for mid_premium reference)
        // Oracle at $51k (market already priced in some movement)
        // Mid premium (vs index): (51500 - 50000) / 50000 * 10000 = 300 bps
        // Oracle premium (vs oracle): (51500 - 51000) / 51000 * 10000 ≈ 98 bps

        // With 50% oracle weight: (50% * 98 + 50% * 300) / 100 ≈ 199 bps
        let blended = sample_premium_with_oracle(&book, 5_000_000, 5_100_000, 5000);
        // Due to integer math, should be close to 199
        assert!(
            blended > 150 && blended < 250,
            "Expected ~199, got {}",
            blended
        );
    }

    #[test]
    fn test_oracle_weighted_premium_full_oracle() {
        // bid=51000, ask=52000 -> mid=51500
        let book = make_orderbook_with_spread(5_100_000, 5_200_000);

        // 100% oracle weight - should only use oracle price
        let blended = sample_premium_with_oracle(&book, 5_000_000, 5_100_000, 10000);

        // Oracle premium = (51500 - 51000) / 51000 * 10000 ≈ 98 bps
        assert!(
            blended > 80 && blended < 120,
            "Expected ~98, got {}",
            blended
        );
    }

    #[test]
    fn test_oracle_weighted_premium_zero_oracle() {
        // bid=51000, ask=52000 -> mid=51500
        let book = make_orderbook_with_spread(5_100_000, 5_200_000);

        // 0% oracle weight - should only use mid price
        let blended = sample_premium_with_oracle(&book, 5_000_000, 5_100_000, 0);

        // Should equal regular mid premium
        let mid_premium = sample_premium(&book, 5_000_000);
        assert_eq!(blended, mid_premium);
    }
}
