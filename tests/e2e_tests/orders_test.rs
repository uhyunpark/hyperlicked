//! Order Tests
//!
//! Tests for order placement, cancellation, and order types.

use crate::e2e::helpers::*;

/// Test that GTC order rests on book when no match
#[test]
fn test_gtc_order_rests_on_book() {
    let mut ctx = TestContext::with_default_traders();

    // Place a bid that won't match (no asks)
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .gtc()
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Order should be on the book
    assert_book_state(&ctx.state, BTC_SYMBOL, Some(DEFAULT_BTC_PRICE), None);
    assert_bid_size_at(&ctx.state, BTC_SYMBOL, DEFAULT_BTC_PRICE, ONE_BTC);

    // No position yet (no match)
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}

/// Test that IOC order cancels unfilled portion
#[test]
fn test_ioc_cancels_unfilled() {
    let mut ctx = TestContext::with_default_traders();

    // Place IOC bid with no asks to match
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .ioc()
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Book should be empty (IOC was cancelled)
    assert_empty_book(&ctx.state, BTC_SYMBOL);

    // No position (no fill)
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}

/// Test that ALO order is rejected if it would cross the spread
#[test]
fn test_alo_rejected_if_crosses() {
    let mut ctx = TestContext::with_default_traders();

    // Place an ask at $50,000
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Try to place ALO bid that would match
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE) // Same price, would cross
                .with_size(ONE_BTC)
                .alo()
                .build(),
        )
        .expect("order should be accepted to mempool");

    ctx.execute_block();

    // Book should still have only Bob's ask (Alice's ALO was rejected)
    assert_book_state(&ctx.state, BTC_SYMBOL, None, Some(DEFAULT_BTC_PRICE));

    // No positions (no match)
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_no_position(&ctx.state, TRADER_BOB, BTC_SYMBOL);
}

/// Test that cancel removes order from book
#[test]
fn test_cancel_removes_from_book() {
    let mut ctx = TestContext::with_default_traders();

    // Place a bid
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Verify order is on book
    assert_book_state(&ctx.state, BTC_SYMBOL, Some(DEFAULT_BTC_PRICE), None);

    // Get the order ID (format: {symbol}_{seq})
    let order_id = "BTC-USDT_1";

    // Cancel the order
    ctx.state
        .submit_tx(CancelBuilder::new(TRADER_ALICE, order_id).build())
        .expect("cancel should be accepted");

    ctx.execute_block();

    // Book should be empty
    assert_empty_book(&ctx.state, BTC_SYMBOL);
}

/// Test ALO order that doesn't cross is accepted
#[test]
fn test_alo_accepted_if_no_cross() {
    let mut ctx = TestContext::with_default_traders();

    // Place an ask at $51,000 (won't cross with $50,000 bid)
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(5_100_000) // $51,000
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Verify Bob's ask is on book first
    assert_book_state(&ctx.state, BTC_SYMBOL, None, Some(5_100_000));

    // Place ALO bid at $50,000 (doesn't cross since best ask is $51,000)
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE) // $50,000
                .with_size(ONE_BTC)
                .alo()
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Both orders should be on book (ALO accepted because spread exists)
    assert_book_state(
        &ctx.state,
        BTC_SYMBOL,
        Some(DEFAULT_BTC_PRICE),
        Some(5_100_000),
    );
}

/// Test multiple orders at same price level
#[test]
fn test_multiple_orders_same_price() {
    let mut ctx = TestContext::with_traders(&[
        (TRADER_ALICE, DEFAULT_DEPOSIT),
        (TRADER_BOB, DEFAULT_DEPOSIT),
        (TRADER_CHARLIE, DEFAULT_DEPOSIT),
    ]);

    // Place three bids at same price
    for trader in &[TRADER_ALICE, TRADER_BOB, TRADER_CHARLIE] {
        ctx.state
            .submit_tx(
                OrderBuilder::bid(trader)
                    .at_price(DEFAULT_BTC_PRICE)
                    .with_size(ONE_BTC)
                    .build(),
            )
            .expect("order should be accepted");
    }

    ctx.execute_block();

    // Total size at that price should be 3 BTC
    assert_bid_size_at(&ctx.state, BTC_SYMBOL, DEFAULT_BTC_PRICE, 3 * ONE_BTC);
}

/// Test cancelling non-existent order
#[test]
fn test_cancel_nonexistent_order() {
    let mut ctx = TestContext::with_default_traders();

    // Try to cancel an order that doesn't exist
    ctx.state
        .submit_tx(CancelBuilder::new(TRADER_ALICE, "nonexistent").build())
        .expect("cancel should be accepted to mempool");

    // Should execute without panic (tx fails silently)
    ctx.execute_block();
}
