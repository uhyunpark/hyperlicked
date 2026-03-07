//! Risk Limits Tests
//!
//! Tests for withdrawal margin guards and position size limits.

use crate::e2e::helpers::*;

// =============================================================================
// Withdraw Margin Guard Tests
// =============================================================================

/// Test that withdrawal is blocked when it would leave account under-margined
#[test]
fn test_withdraw_blocked_when_undermargined() {
    let mut ctx = TestContext::new();

    // Fund Alice with small deposit and open a position
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_ALICE).amount(SMALL_DEPOSIT).build())
        .unwrap();
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_BOB).amount(DEFAULT_DEPOSIT).build())
        .unwrap();
    ctx.execute_block();

    ctx.set_mark_price(BTC_SYMBOL, DEFAULT_BTC_PRICE);

    // Open a position (Alice buys, Bob sells)
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(HALF_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(HALF_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Verify position exists
    assert_long_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);

    let balance_before = ctx.balance(TRADER_ALICE);

    // Try to withdraw most of the remaining balance (should fail due to margin req)
    ctx.state
        .submit_tx(WithdrawBuilder::new(TRADER_ALICE).amount(balance_before).build())
        .unwrap();
    ctx.execute_block();

    // Balance should be unchanged (withdrawal rolled back)
    assert_balance(&ctx.state, TRADER_ALICE, balance_before);
}

/// Test that withdrawal succeeds when equity stays above maintenance margin
#[test]
fn test_withdraw_allowed_when_well_margined() {
    let mut ctx = TestContext::with_default_traders();

    ctx.set_mark_price(BTC_SYMBOL, DEFAULT_BTC_PRICE);

    // Open a small position relative to balance
    let small_size = ONE_BTC / 100; // 0.01 BTC
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(small_size)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(small_size)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    assert_long_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);

    // Withdraw a small amount - should succeed since position is tiny vs balance
    let small_withdraw = 1000; // $10
    let balance_before = ctx.balance(TRADER_ALICE);
    ctx.state
        .submit_tx(WithdrawBuilder::new(TRADER_ALICE).amount(small_withdraw).build())
        .unwrap();
    ctx.execute_block();

    assert_balance(&ctx.state, TRADER_ALICE, balance_before - small_withdraw);
}

/// Test that withdrawal always succeeds when no positions are open
#[test]
fn test_withdraw_no_positions_always_ok() {
    let mut ctx = TestContext::with_traders(&[(TRADER_ALICE, DEFAULT_DEPOSIT)]);

    let withdraw_amount = DEFAULT_DEPOSIT / 2;
    ctx.state
        .submit_tx(WithdrawBuilder::new(TRADER_ALICE).amount(withdraw_amount).build())
        .unwrap();
    ctx.execute_block();

    assert_balance(&ctx.state, TRADER_ALICE, DEFAULT_DEPOSIT - withdraw_amount);
}

// =============================================================================
// Position Size Limit Tests
// =============================================================================

/// Test that an order exceeding max_position_size is rejected
#[test]
fn test_position_exceeds_max_rejected() {
    let mut ctx = TestContext::with_default_traders();

    // Add a market with tight position limit: 2 BTC max
    let max_pos = 200_000_000; // 2 BTC in satoshis
    ctx.state
        .submit_tx(
            AddMarketBuilder::new(ADMIN_ADDRESS, ETH_SYMBOL)
                .with_mark_price(DEFAULT_ETH_PRICE)
                .with_max_position_size(max_pos)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    ctx.set_mark_price(ETH_SYMBOL, DEFAULT_ETH_PRICE);

    // Try to place order larger than max position (3 BTC)
    let too_large = 300_000_000;
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .symbol(ETH_SYMBOL)
                .at_price(DEFAULT_ETH_PRICE)
                .with_size(too_large)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .symbol(ETH_SYMBOL)
                .at_price(DEFAULT_ETH_PRICE)
                .with_size(too_large)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should NOT have been created (order rejected)
    assert_no_position(&ctx.state, TRADER_ALICE, ETH_SYMBOL);
}

/// Test that an order at exactly max_position_size succeeds
#[test]
fn test_position_at_max_succeeds() {
    let mut ctx = TestContext::with_default_traders();

    let max_pos = 200_000_000; // 2 BTC
    ctx.state
        .submit_tx(
            AddMarketBuilder::new(ADMIN_ADDRESS, ETH_SYMBOL)
                .with_mark_price(DEFAULT_ETH_PRICE)
                .with_max_position_size(max_pos)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    ctx.set_mark_price(ETH_SYMBOL, DEFAULT_ETH_PRICE);

    // Place order at exactly max position size
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .symbol(ETH_SYMBOL)
                .at_price(DEFAULT_ETH_PRICE)
                .with_size(max_pos)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .symbol(ETH_SYMBOL)
                .at_price(DEFAULT_ETH_PRICE)
                .with_size(max_pos)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    assert_position(&ctx.state, TRADER_ALICE, ETH_SYMBOL, max_pos);
}

/// Test that incremental orders pushing past max are rejected
#[test]
fn test_position_incremental_exceeds_max() {
    let mut ctx = TestContext::with_default_traders();

    let max_pos = 200_000_000; // 2 BTC
    ctx.state
        .submit_tx(
            AddMarketBuilder::new(ADMIN_ADDRESS, ETH_SYMBOL)
                .with_mark_price(DEFAULT_ETH_PRICE)
                .with_max_position_size(max_pos)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    ctx.set_mark_price(ETH_SYMBOL, DEFAULT_ETH_PRICE);

    // First order: 1.5 BTC (within limit)
    let first_size = 150_000_000;
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .symbol(ETH_SYMBOL)
                .at_price(DEFAULT_ETH_PRICE)
                .with_size(first_size)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .symbol(ETH_SYMBOL)
                .at_price(DEFAULT_ETH_PRICE)
                .with_size(first_size)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    assert_position(&ctx.state, TRADER_ALICE, ETH_SYMBOL, first_size);

    // Second order: 1 BTC (would push to 2.5 BTC, over limit)
    let second_size = 100_000_000;
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .symbol(ETH_SYMBOL)
                .at_price(DEFAULT_ETH_PRICE)
                .with_size(second_size)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .symbol(ETH_SYMBOL)
                .at_price(DEFAULT_ETH_PRICE)
                .with_size(second_size)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should still be 1.5 BTC (second order rejected)
    assert_position(&ctx.state, TRADER_ALICE, ETH_SYMBOL, first_size);
}
