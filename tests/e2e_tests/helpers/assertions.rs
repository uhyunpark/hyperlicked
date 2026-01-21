//! Test Assertions
//!
//! Custom assertion helpers for clear, descriptive test failures.

use hyperlicked::app::AppState;

// =============================================================================
// Position Assertions
// =============================================================================

/// Assert that a trader has a specific position size
///
/// # Panics
/// Panics with descriptive message if position doesn't match
pub fn assert_position(state: &AppState, trader: &str, symbol: &str, expected_size: i64) {
    let actual = state
        .account(trader)
        .map(|a| a.position(symbol).size)
        .unwrap_or(0);

    assert_eq!(
        actual, expected_size,
        "Position mismatch for {} in {}: expected {} sats, got {} sats",
        trader, symbol, expected_size, actual
    );
}

/// Assert that a trader has a specific entry price
pub fn assert_entry_price(state: &AppState, trader: &str, symbol: &str, expected_price: i64) {
    let actual = state
        .account(trader)
        .map(|a| a.position(symbol).entry_price)
        .unwrap_or(0);

    assert_eq!(
        actual, expected_price,
        "Entry price mismatch for {} in {}: expected {} cents, got {} cents",
        trader, symbol, expected_price, actual
    );
}

/// Assert that a trader has zero position in a symbol
pub fn assert_no_position(state: &AppState, trader: &str, symbol: &str) {
    assert_position(state, trader, symbol, 0);
}

/// Assert that a position is long (positive size)
pub fn assert_long_position(state: &AppState, trader: &str, symbol: &str) {
    let size = state
        .account(trader)
        .map(|a| a.position(symbol).size)
        .unwrap_or(0);

    assert!(
        size > 0,
        "Expected {} to have long position in {}, got {} sats",
        trader, symbol, size
    );
}

/// Assert that a position is short (negative size)
pub fn assert_short_position(state: &AppState, trader: &str, symbol: &str) {
    let size = state
        .account(trader)
        .map(|a| a.position(symbol).size)
        .unwrap_or(0);

    assert!(
        size < 0,
        "Expected {} to have short position in {}, got {} sats",
        trader, symbol, size
    );
}

// =============================================================================
// Balance Assertions
// =============================================================================

/// Assert that a trader has a specific balance
pub fn assert_balance(state: &AppState, trader: &str, expected: i64) {
    let actual = state.account(trader).map(|a| a.balance).unwrap_or(0);

    assert_eq!(
        actual, expected,
        "Balance mismatch for {}: expected {} cents, got {} cents",
        trader, expected, actual
    );
}

/// Assert that a trader's balance is within a range
///
/// Useful for tests where exact fees are hard to predict
pub fn assert_balance_range(state: &AppState, trader: &str, min: i64, max: i64) {
    let actual = state.account(trader).map(|a| a.balance).unwrap_or(0);

    assert!(
        actual >= min && actual <= max,
        "Balance for {} not in expected range: expected [{}, {}], got {}",
        trader, min, max, actual
    );
}

/// Assert that a trader's balance changed by a specific amount
pub fn assert_balance_delta(state: &AppState, trader: &str, initial: i64, expected_delta: i64) {
    let actual = state.account(trader).map(|a| a.balance).unwrap_or(0);
    let actual_delta = actual - initial;

    assert_eq!(
        actual_delta, expected_delta,
        "Balance delta mismatch for {}: expected {} cents, got {} cents",
        trader, expected_delta, actual_delta
    );
}

/// Assert that an account exists
pub fn assert_account_exists(state: &AppState, trader: &str) {
    assert!(
        state.account(trader).is_some(),
        "Expected account {} to exist",
        trader
    );
}

/// Assert that an account does not exist
pub fn assert_account_not_exists(state: &AppState, trader: &str) {
    assert!(
        state.account(trader).is_none(),
        "Expected account {} to not exist",
        trader
    );
}

// =============================================================================
// Orderbook Assertions
// =============================================================================

/// Assert the best bid/ask prices on the orderbook
pub fn assert_book_state(
    state: &AppState,
    symbol: &str,
    expected_best_bid: Option<i64>,
    expected_best_ask: Option<i64>,
) {
    let book = state.orderbook(symbol).expect("orderbook should exist");

    let actual_bid = book.best_bid();
    let actual_ask = book.best_ask();

    assert_eq!(
        actual_bid, expected_best_bid,
        "Best bid mismatch for {}: expected {:?}, got {:?}",
        symbol, expected_best_bid, actual_bid
    );

    assert_eq!(
        actual_ask, expected_best_ask,
        "Best ask mismatch for {}: expected {:?}, got {:?}",
        symbol, expected_best_ask, actual_ask
    );
}

/// Assert that the orderbook has no orders
pub fn assert_empty_book(state: &AppState, symbol: &str) {
    assert_book_state(state, symbol, None, None);
}

/// Assert size at a specific bid price level
pub fn assert_bid_size_at(state: &AppState, symbol: &str, price: i64, expected_size: i64) {
    let book = state.orderbook(symbol).expect("orderbook should exist");
    let levels = book.bid_levels(100);

    let actual_size = levels
        .iter()
        .find(|l| l.price == price)
        .map(|l| l.size)
        .unwrap_or(0);

    assert_eq!(
        actual_size, expected_size,
        "Bid size at {} for {}: expected {}, got {}",
        price, symbol, expected_size, actual_size
    );
}

/// Assert size at a specific ask price level
pub fn assert_ask_size_at(state: &AppState, symbol: &str, price: i64, expected_size: i64) {
    let book = state.orderbook(symbol).expect("orderbook should exist");
    let levels = book.ask_levels(100);

    let actual_size = levels
        .iter()
        .find(|l| l.price == price)
        .map(|l| l.size)
        .unwrap_or(0);

    assert_eq!(
        actual_size, expected_size,
        "Ask size at {} for {}: expected {}, got {}",
        price, symbol, expected_size, actual_size
    );
}

// =============================================================================
// Insurance Fund Assertions
// =============================================================================

/// Assert the insurance fund balance
pub fn assert_insurance_fund(state: &AppState, expected: i64) {
    let actual = state.insurance_fund_balance();

    assert_eq!(
        actual, expected,
        "Insurance fund mismatch: expected {} cents, got {} cents",
        expected, actual
    );
}

// =============================================================================
// Trigger Order Assertions
// =============================================================================

/// Assert that a trader has a specific number of trigger orders
pub fn assert_trigger_order_count(state: &AppState, trader: &str, expected_count: usize) {
    let actual_count = state.trigger_orders_by_trader(trader).len();

    assert_eq!(
        actual_count, expected_count,
        "Trigger order count mismatch for {}: expected {}, got {}",
        trader, expected_count, actual_count
    );
}

/// Assert that a trigger order exists with a given ID
pub fn assert_trigger_order_exists(state: &AppState, trigger_id: &str) {
    assert!(
        state.trigger_order(trigger_id).is_some(),
        "Expected trigger order {} to exist",
        trigger_id
    );
}

// =============================================================================
// Staking Assertions
// =============================================================================

/// Assert validator exists
pub fn assert_validator_exists(state: &AppState, operator: &str) {
    let staking = state.staking();
    let operator_string = operator.to_string();
    assert!(
        staking.get_validator(&operator_string).is_some(),
        "Expected validator {} to exist",
        operator
    );
}

/// Assert validator stake amount
pub fn assert_validator_stake(state: &AppState, operator: &str, expected_stake: i64) {
    let staking = state.staking();
    let operator_string = operator.to_string();
    let validator = staking
        .get_validator(&operator_string)
        .expect("validator should exist");

    assert_eq!(
        validator.total_stake, expected_stake,
        "Validator {} stake mismatch: expected {}, got {}",
        operator, expected_stake, validator.total_stake
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assert_position_zero() {
        let state = AppState::new();
        assert_position(&state, "nonexistent", "BTC-USDT", 0);
    }

    #[test]
    fn test_assert_empty_book() {
        let state = AppState::new();
        // After creating state, there should be a BTC-USDT orderbook but it's empty
        assert_empty_book(&state, "BTC-USDT");
    }
}
