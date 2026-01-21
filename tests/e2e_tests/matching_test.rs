//! Matching Tests
//!
//! Tests for order matching, price-time priority, and partial fills.

use crate::e2e::helpers::*;

/// Test basic bid-ask matching
#[test]
fn test_basic_bid_ask_match() {
    let mut ctx = TestContext::with_default_traders();

    // Alice bids at $50,000
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    // Bob asks at $50,000 (crosses Alice's bid)
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Both should have positions
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC); // Long 1 BTC
    assert_position(&ctx.state, TRADER_BOB, BTC_SYMBOL, -ONE_BTC); // Short 1 BTC

    // Book should be empty
    assert_empty_book(&ctx.state, BTC_SYMBOL);
}

/// Test taker crossing maker at better price
#[test]
fn test_taker_crosses_at_better_price() {
    let mut ctx = TestContext::with_default_traders();

    // Alice bids at $50,000
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Bob asks at $49,000 (better for buyer)
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(4_900_000) // $49,000
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Should match at the maker's (Alice's) price of $50,000
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
    assert_entry_price(&ctx.state, TRADER_ALICE, BTC_SYMBOL, DEFAULT_BTC_PRICE);

    assert_position(&ctx.state, TRADER_BOB, BTC_SYMBOL, -ONE_BTC);
    assert_entry_price(&ctx.state, TRADER_BOB, BTC_SYMBOL, DEFAULT_BTC_PRICE);
}

/// Test price-time priority (FIFO within price level)
#[test]
fn test_price_time_priority() {
    let mut ctx = TestContext::with_traders(&[
        (TRADER_ALICE, DEFAULT_DEPOSIT),
        (TRADER_BOB, DEFAULT_DEPOSIT),
        (TRADER_CHARLIE, DEFAULT_DEPOSIT),
    ]);

    // Alice bids first at $50,000
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    // Bob bids second at same price
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Charlie sells 1 BTC - should match Alice first
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_CHARLIE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Alice should have been filled (first in line)
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);

    // Bob's order should still be on book (second in line)
    assert_no_position(&ctx.state, TRADER_BOB, BTC_SYMBOL);
    assert_bid_size_at(&ctx.state, BTC_SYMBOL, DEFAULT_BTC_PRICE, ONE_BTC);

    // Charlie is short
    assert_position(&ctx.state, TRADER_CHARLIE, BTC_SYMBOL, -ONE_BTC);
}

/// Test partial fill
#[test]
fn test_partial_fill() {
    let mut ctx = TestContext::with_default_traders();

    // Alice bids for 2 BTC
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(2 * ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Bob sells only 1 BTC
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Alice should have 1 BTC position
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);

    // Bob should be short 1 BTC
    assert_position(&ctx.state, TRADER_BOB, BTC_SYMBOL, -ONE_BTC);

    // 1 BTC remaining on book
    assert_bid_size_at(&ctx.state, BTC_SYMBOL, DEFAULT_BTC_PRICE, ONE_BTC);
}

/// Test self-trade prevention (same trader orders shouldn't match)
#[test]
fn test_self_trade_prevention() {
    let mut ctx = TestContext::with_default_traders();

    // Alice places a bid
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Alice places an ask at same price (would match herself)
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Self-trade should be prevented, both orders on book
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_bid_size_at(&ctx.state, BTC_SYMBOL, DEFAULT_BTC_PRICE, ONE_BTC);
    assert_ask_size_at(&ctx.state, BTC_SYMBOL, DEFAULT_BTC_PRICE, ONE_BTC);
}

/// Test price improvement - best price matched first
#[test]
fn test_price_improvement() {
    let mut ctx = TestContext::with_traders(&[
        (TRADER_ALICE, DEFAULT_DEPOSIT),
        (TRADER_BOB, DEFAULT_DEPOSIT),
        (TRADER_CHARLIE, DEFAULT_DEPOSIT),
    ]);

    // Alice bids at $50,000
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    // Bob bids higher at $51,000
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(5_100_000) // $51,000
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Charlie sells - should match Bob's higher bid first
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_CHARLIE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Bob should be filled (better price)
    assert_position(&ctx.state, TRADER_BOB, BTC_SYMBOL, ONE_BTC);
    assert_entry_price(&ctx.state, TRADER_BOB, BTC_SYMBOL, 5_100_000);

    // Alice's order should still be on book
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_bid_size_at(&ctx.state, BTC_SYMBOL, DEFAULT_BTC_PRICE, ONE_BTC);
}

/// Test IOC partial fill
#[test]
fn test_ioc_partial_fill() {
    let mut ctx = TestContext::with_default_traders();

    // Alice bids for 1 BTC
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Bob tries to sell 2 BTC with IOC
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(2 * ONE_BTC)
                .ioc()
                .build(),
        )
        .expect("order should be accepted");

    ctx.execute_block();

    // Should fill 1 BTC, cancel rest
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
    assert_position(&ctx.state, TRADER_BOB, BTC_SYMBOL, -ONE_BTC);

    // No remaining orders
    assert_empty_book(&ctx.state, BTC_SYMBOL);
}
