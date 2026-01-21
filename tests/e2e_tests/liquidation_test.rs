//! Liquidation Tests
//!
//! Tests for margin checks and automatic liquidation.

use crate::e2e::helpers::*;

/// Test that healthy accounts are not liquidated
#[test]
fn test_healthy_not_liquidated() {
    let mut ctx = TestContext::with_default_traders();

    // Alice goes long with plenty of margin
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

    // Verify position exists
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);

    // Price drops slightly but Alice has lots of margin
    ctx.set_mark_price(BTC_SYMBOL, 4_900_000); // $49,000

    // Execute another block to trigger liquidation check
    ctx.execute_block();

    // Position should still exist (not liquidated)
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
}

/// Test underwater long position gets liquidated on price drop
#[test]
fn test_underwater_long_liquidated() {
    let mut ctx = TestContext::new();

    // Alice deposits only $5,000 (small margin)
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_ALICE).amount(SMALL_DEPOSIT).build())
        .unwrap();
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_BOB).amount(DEFAULT_DEPOSIT).build())
        .unwrap();
    ctx.execute_block();

    // Alice goes long 1 BTC at $50,000 with only $5,000 margin
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

    // Verify position
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);

    // Price drops significantly - Alice should be liquidated
    // At $46,000, unrealized loss = $4,000, equity ~= $1,000
    // Maintenance margin (5% of $46k) = $2,300
    // Since equity < maintenance, should be liquidated
    ctx.set_mark_price(BTC_SYMBOL, 4_600_000); // $46,000

    ctx.execute_block();

    // Position should be closed (liquidated)
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);

    // Insurance fund should have received liquidation proceeds
    let insurance = ctx.state.insurance_fund_balance();
    // After liquidation, the remaining balance goes to insurance fund
    // This could be positive or negative depending on the loss
    assert!(insurance != 0, "Insurance fund should be affected");
}

/// Test underwater short position gets liquidated on price rise
#[test]
fn test_underwater_short_liquidated() {
    let mut ctx = TestContext::new();

    // Alice deposits only $5,000 (small margin)
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_ALICE).amount(SMALL_DEPOSIT).build())
        .unwrap();
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_BOB).amount(DEFAULT_DEPOSIT).build())
        .unwrap();
    ctx.execute_block();

    // Alice goes short 1 BTC at $50,000
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

    // Verify short position
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, -ONE_BTC);

    // Price rises significantly - Alice should be liquidated
    // At $54,000, unrealized loss = $4,000, equity ~= $1,000
    // Maintenance margin (5% of $54k) = $2,700
    ctx.set_mark_price(BTC_SYMBOL, 5_400_000); // $54,000

    ctx.execute_block();

    // Position should be closed (liquidated)
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}

/// Test liquidation proceeds go to insurance fund
#[test]
fn test_liquidation_to_insurance() {
    let mut ctx = TestContext::new();

    let initial_insurance = ctx.state.insurance_fund_balance();

    // Setup: Alice with minimal margin
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_ALICE).amount(SMALL_DEPOSIT).build())
        .unwrap();
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_BOB).amount(DEFAULT_DEPOSIT).build())
        .unwrap();
    ctx.execute_block();

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

    // Trigger liquidation
    ctx.set_mark_price(BTC_SYMBOL, 4_600_000);
    ctx.execute_block();

    // Insurance fund should have changed
    let final_insurance = ctx.state.insurance_fund_balance();
    assert_ne!(
        initial_insurance, final_insurance,
        "Insurance fund should change after liquidation"
    );
}

/// Test multiple positions liquidated in same block
#[test]
fn test_multiple_liquidations_same_block() {
    let mut ctx = TestContext::new();

    // Setup three traders with minimal margin
    for trader in &[TRADER_ALICE, TRADER_BOB, TRADER_CHARLIE] {
        ctx.state
            .submit_tx(DepositBuilder::new(trader).amount(SMALL_DEPOSIT).build())
            .unwrap();
    }

    // Need a counterparty with lots of funds
    ctx.state
        .submit_tx(DepositBuilder::new("market_maker").amount(10 * DEFAULT_DEPOSIT).build())
        .unwrap();
    ctx.execute_block();

    // All three go long
    for trader in &[TRADER_ALICE, TRADER_BOB, TRADER_CHARLIE] {
        ctx.state
            .submit_tx(
                OrderBuilder::bid(trader)
                    .at_price(DEFAULT_BTC_PRICE)
                    .with_size(ONE_BTC)
                    .build(),
            )
            .unwrap();
    }

    // Market maker provides liquidity
    ctx.state
        .submit_tx(
            OrderBuilder::ask("market_maker")
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(3 * ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Verify all positions
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
    assert_position(&ctx.state, TRADER_BOB, BTC_SYMBOL, ONE_BTC);
    assert_position(&ctx.state, TRADER_CHARLIE, BTC_SYMBOL, ONE_BTC);

    // Price crashes - all should be liquidated
    ctx.set_mark_price(BTC_SYMBOL, 4_600_000);
    ctx.execute_block();

    // All positions should be closed
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_no_position(&ctx.state, TRADER_BOB, BTC_SYMBOL);
    assert_no_position(&ctx.state, TRADER_CHARLIE, BTC_SYMBOL);
}

/// Test position not liquidated at exact maintenance margin
#[test]
fn test_not_liquidated_at_maintenance_boundary() {
    let mut ctx = TestContext::new();

    // Calculate deposit for position to be exactly at maintenance margin
    // Position: 1 BTC at $50,000
    // At $48,000: unrealized loss = $2,000
    // Maintenance = 5% * $48,000 = $2,400
    // To be safe (not liquidated), equity must be >= $2,400
    // equity = balance + unrealized = balance - $2,000 >= $2,400
    // balance >= $4,400
    // So deposit $5,000 should be safe
    let deposit = 500_000; // $5,000

    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_ALICE).amount(deposit).build())
        .unwrap();
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_BOB).amount(DEFAULT_DEPOSIT).build())
        .unwrap();
    ctx.execute_block();

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

    // Price drops to where Alice is close to but not at liquidation
    // At $48,000: unrealized loss = $2,000, equity = $3,000
    // Maintenance = 5% * $48,000 = $2,400
    // equity ($3,000) > maintenance ($2,400) -> should be safe
    ctx.set_mark_price(BTC_SYMBOL, 4_800_000); // $48,000
    ctx.execute_block();

    // Position should still exist
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
}
