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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct FundingResult {
    /// Symbol this funding was applied to
    pub symbol: String,
    /// Funding rate in basis points (positive = longs pay shorts)
    pub funding_rate_bps: i64,
    /// Total amount paid by longs
    pub longs_paid: i64,
    /// Total amount received by shorts (should equal longs_paid ideally)
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
    let mut longs_paid: i64 = 0;
    let mut shorts_received: i64 = 0;
    let mut user_payments: Vec<UserFundingPayment> = Vec::new();

    // Get addresses with positions (need to collect to avoid borrow issues)
    let addresses: Vec<String> = accounts
        .all_accounts()
        .iter()
        .filter(|a| a.positions.get(symbol).map(|p| p.size != 0).unwrap_or(false))
        .map(|a| a.address.clone())
        .collect();

    // Apply funding to each account
    for address in addresses {
        let account = accounts.get_or_create(&address);
        if let Some(position) = account.positions.get_mut(symbol) {
            let position_size = position.size;
            let payment = position.apply_funding(funding_rate_bps, index_price, timestamp);

            // Cap negative payments at available balance (can't pay more than you have)
            let actual_payment = if payment < 0 {
                payment.max(-account.balance)
            } else {
                payment
            };

            // Track totals using actual payment
            if position_size > 0 {
                // Long position
                longs_paid += -actual_payment;
            } else {
                // Short position
                shorts_received += actual_payment;
            }

            // Apply to balance
            account.balance += actual_payment;

            // Record per-user payment for WebSocket notification
            user_payments.push(UserFundingPayment {
                address: address.clone(),
                symbol: symbol.to_string(),
                payment: actual_payment,
                position_size,
                funding_rate_bps,
                timestamp,
            });
        }
    }

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
        assert!(long.balance >= 0, "Balance should not go negative, got {}", long.balance);

        // Total paid by longs should be capped
        assert!(result.longs_paid <= 100, "Longs paid {} should be capped at balance 100", result.longs_paid);
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
        assert!(blended > 150 && blended < 250, "Expected ~199, got {}", blended);
    }

    #[test]
    fn test_oracle_weighted_premium_full_oracle() {
        // bid=51000, ask=52000 -> mid=51500
        let book = make_orderbook_with_spread(5_100_000, 5_200_000);

        // 100% oracle weight - should only use oracle price
        let blended = sample_premium_with_oracle(&book, 5_000_000, 5_100_000, 10000);

        // Oracle premium = (51500 - 51000) / 51000 * 10000 ≈ 98 bps
        assert!(blended > 80 && blended < 120, "Expected ~98, got {}", blended);
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
