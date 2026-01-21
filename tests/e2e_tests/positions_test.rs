//! Position Tests
//!
//! Tests for position lifecycle: opening, sizing, PnL realization, and closing.

use crate::e2e::helpers::*;

/// Test that buying creates a long position
#[test]
fn test_buy_creates_long() {
    let mut ctx = TestContext::with_default_traders();

    // Alice bids
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    // Bob sells to Alice
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Alice should be long
    assert_long_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
    assert_entry_price(&ctx.state, TRADER_ALICE, BTC_SYMBOL, DEFAULT_BTC_PRICE);
}

/// Test that selling creates a short position
#[test]
fn test_sell_creates_short() {
    let mut ctx = TestContext::with_default_traders();

    // Alice asks (sell)
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    // Bob bids to buy from Alice
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Alice should be short
    assert_short_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, -ONE_BTC);
    assert_entry_price(&ctx.state, TRADER_ALICE, BTC_SYMBOL, DEFAULT_BTC_PRICE);
}

/// Test reducing position realizes PnL
#[test]
fn test_reduce_realizes_pnl() {
    let mut ctx = TestContext::with_default_traders();

    let initial_alice_balance = ctx.balance(TRADER_ALICE);

    // Alice goes long 1 BTC at $50,000
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Alice closes at $51,000 (profit)
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_ALICE)
                .at_price(5_100_000) // $51,000
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(5_100_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Alice should have no position
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);

    // Alice's balance should reflect profit
    // Profit = (51000 - 50000) * 1 BTC = $1000 = 100_000 cents
    // (minus fees which are small)
    let alice_balance = ctx.balance(TRADER_ALICE);
    let expected_profit = 100_000; // $1000 in cents

    // Balance should have increased (roughly by profit, minus fees)
    assert!(
        alice_balance > initial_alice_balance,
        "Alice should have profit: {} vs {}",
        alice_balance,
        initial_alice_balance
    );
}

/// Test closing full position zeros size
#[test]
fn test_close_zeros_size() {
    let mut ctx = TestContext::with_default_traders();

    // Alice goes long
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);

    // Alice sells her entire position
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should be zero
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, 0);
}

/// Test adding to existing position updates average entry
#[test]
fn test_add_to_position_updates_entry() {
    let mut ctx = TestContext::with_default_traders();

    // Alice buys 1 BTC at $50,000
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    assert_entry_price(&ctx.state, TRADER_ALICE, BTC_SYMBOL, DEFAULT_BTC_PRICE);

    // Alice buys another 1 BTC at $52,000
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(5_200_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(5_200_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should be 2 BTC
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, 2 * ONE_BTC);

    // Average entry should be $51,000
    assert_entry_price(&ctx.state, TRADER_ALICE, BTC_SYMBOL, 5_100_000);
}

/// Test partial close of position
#[test]
fn test_partial_close() {
    let mut ctx = TestContext::with_default_traders();

    // Alice goes long 2 BTC
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(2 * ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(2 * ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, 2 * ONE_BTC);

    // Alice sells 1 BTC (partial close)
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Should have 1 BTC remaining
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
}

/// Test flip from long to short
#[test]
fn test_position_flip() {
    let mut ctx = TestContext::with_default_traders();

    // Alice goes long 1 BTC
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    assert_long_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);

    // Alice sells 2 BTC (flip to short)
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(2 * ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(2 * ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Alice should now be short 1 BTC
    assert_short_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, -ONE_BTC);
}

/// Test reduce_only order
#[test]
fn test_reduce_only_order() {
    let mut ctx = TestContext::with_default_traders();

    // Alice goes long 1 BTC
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Alice places reduce_only sell
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .reduce_only()
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should be closed
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}
