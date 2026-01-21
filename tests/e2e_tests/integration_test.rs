//! Integration Tests
//!
//! Full flow scenarios testing multiple features together.

use hyperlicked::app::Transaction;

use crate::e2e::helpers::*;

/// Test full trade lifecycle: deposit → trade → close → withdraw
#[test]
fn test_full_trade_lifecycle() {
    let mut ctx = TestContext::new();

    // === Phase 1: Deposits ===
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_ALICE).amount(DEFAULT_DEPOSIT).build())
        .unwrap();
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_BOB).amount(DEFAULT_DEPOSIT).build())
        .unwrap();
    ctx.execute_block();

    assert_balance(&ctx.state, TRADER_ALICE, DEFAULT_DEPOSIT);
    assert_balance(&ctx.state, TRADER_BOB, DEFAULT_DEPOSIT);

    // === Phase 2: Open Positions ===
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

    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
    assert_position(&ctx.state, TRADER_BOB, BTC_SYMBOL, -ONE_BTC);

    // === Phase 3: Price Move & Close ===
    // Price rises to $51,000 - Alice in profit, Bob in loss
    ctx.set_mark_price(BTC_SYMBOL, 5_100_000);

    // Both close positions
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_ALICE)
                .at_price(5_100_000)
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

    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_no_position(&ctx.state, TRADER_BOB, BTC_SYMBOL);

    // === Phase 4: Withdraw ===
    let alice_balance = ctx.balance(TRADER_ALICE);
    let bob_balance = ctx.balance(TRADER_BOB);

    // Alice should have profit, Bob should have loss
    // PnL = (51000 - 50000) * 1 = $1000 = 100,000 cents
    // Minus fees, so roughly alice gained, bob lost

    // Both withdraw all
    ctx.state
        .submit_tx(
            WithdrawBuilder::new(TRADER_ALICE)
                .amount(alice_balance)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            WithdrawBuilder::new(TRADER_BOB)
                .amount(bob_balance)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Balances should be zero
    assert_balance(&ctx.state, TRADER_ALICE, 0);
    assert_balance(&ctx.state, TRADER_BOB, 0);
}

/// Test liquidation cascade: multiple accounts liquidated as price crashes
#[test]
fn test_liquidation_cascade() {
    let mut ctx = TestContext::new();

    // Setup multiple traders with minimal margin
    for trader in &[TRADER_ALICE, TRADER_BOB, TRADER_CHARLIE] {
        ctx.state
            .submit_tx(DepositBuilder::new(trader).amount(SMALL_DEPOSIT).build())
            .unwrap();
    }

    // Market maker with lots of capital
    ctx.state
        .submit_tx(DepositBuilder::new("market_maker").amount(10 * DEFAULT_DEPOSIT).build())
        .unwrap();
    ctx.execute_block();

    // All three go long at $50,000
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

    ctx.state
        .submit_tx(
            OrderBuilder::ask("market_maker")
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(3 * ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // All have positions
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
    assert_position(&ctx.state, TRADER_BOB, BTC_SYMBOL, ONE_BTC);
    assert_position(&ctx.state, TRADER_CHARLIE, BTC_SYMBOL, ONE_BTC);

    let initial_insurance = ctx.state.insurance_fund_balance();

    // Price crash - all should be liquidated
    ctx.set_mark_price(BTC_SYMBOL, 4_600_000); // $46,000
    ctx.execute_block();

    // All positions should be closed
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_no_position(&ctx.state, TRADER_BOB, BTC_SYMBOL);
    assert_no_position(&ctx.state, TRADER_CHARLIE, BTC_SYMBOL);

    // Insurance fund should be affected
    let final_insurance = ctx.state.insurance_fund_balance();
    assert_ne!(
        initial_insurance, final_insurance,
        "Insurance fund should change after liquidation cascade"
    );
}

/// Test positions across multiple funding periods
#[test]
fn test_funding_over_intervals() {
    let mut ctx = TestContext::with_default_traders();

    // Open positions
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

    let alice_initial = ctx.balance(TRADER_ALICE);
    let bob_initial = ctx.balance(TRADER_BOB);

    let funding_interval = 3600000; // 1 hour

    // Advance through 3 funding intervals
    for i in 1..=3 {
        ctx.execute_block_at(ctx.timestamp() + funding_interval + 1000);

        let last_funding = ctx.state.last_funding_time(BTC_SYMBOL);
        assert!(
            last_funding > 0,
            "Funding should occur at interval {}",
            i
        );
    }

    // Positions should still exist
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
    assert_position(&ctx.state, TRADER_BOB, BTC_SYMBOL, -ONE_BTC);

    // Balances may have changed due to funding
    let alice_final = ctx.balance(TRADER_ALICE);
    let bob_final = ctx.balance(TRADER_BOB);

    // The sum of changes should be zero (funding is zero-sum)
    let total_delta = (alice_final - alice_initial) + (bob_final - bob_initial);
    assert_eq!(total_delta, 0, "Funding should be zero-sum across traders");
}

/// Test complex scenario: trade, trigger, partial close
#[test]
fn test_trade_with_triggers() {
    let mut ctx = TestContext::with_default_traders();

    // Alice opens long position
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(2 * ONE_BTC) // 2 BTC
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

    // Place stop loss for half the position
    ctx.state
        .submit_tx(
            TriggerOrderBuilder::stop_loss(TRADER_ALICE)
                .at_trigger_price(4_800_000) // $48,000
                .with_size(ONE_BTC) // Only half
                .with_cloid("partial-sl")
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Place take profit for the other half
    ctx.state
        .submit_tx(
            TriggerOrderBuilder::take_profit(TRADER_ALICE)
                .at_trigger_price(5_200_000) // $52,000
                .with_size(ONE_BTC)
                .with_cloid("partial-tp")
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Two trigger orders
    assert_trigger_order_count(&ctx.state, TRADER_ALICE, 2);

    // Price drops - stop loss triggers
    ctx.set_mark_price(BTC_SYMBOL, 4_700_000);
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(4_700_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should be reduced to 1 BTC
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);

    // Now price rises - take profit should trigger
    ctx.set_mark_price(BTC_SYMBOL, 5_300_000);
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(5_300_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should be fully closed
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}

/// Test mempool 3-bucket ordering in a single block
#[test]
fn test_mempool_ordering_in_block() {
    let mut ctx = TestContext::new();

    // Submit transactions in "wrong" order: order, cancel, deposit
    // Mempool should reorder to: deposit, cancel, order

    // Place order first (would fail without deposit)
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();

    // Cancel (for non-existent order)
    ctx.state
        .submit_tx(CancelBuilder::new(TRADER_ALICE, "nonexistent").build())
        .unwrap();

    // Deposit last
    ctx.state
        .submit_tx(DepositBuilder::new(TRADER_ALICE).amount(DEFAULT_DEPOSIT).build())
        .unwrap();

    // Execute block - deposit should process first, enabling the order
    ctx.execute_block();

    // Deposit was processed first, so order should succeed
    // (If order was processed before deposit, it would fail due to no margin)
    assert_balance(&ctx.state, TRADER_ALICE, DEFAULT_DEPOSIT - 0); // Balance reduced only by fees if order rests

    // Order should be on book (deposit processed first, giving margin)
    let best_bid = ctx.best_bid(BTC_SYMBOL);
    assert!(
        best_bid.is_some(),
        "Order should be on book (deposit processed first)"
    );
}

/// Test concurrent trading between multiple pairs of traders
#[test]
fn test_multiple_traders_concurrent() {
    let mut ctx = TestContext::new();

    let traders = ["t1", "t2", "t3", "t4", "t5", "t6"];

    // Fund all traders
    for trader in &traders {
        ctx.state
            .submit_tx(DepositBuilder::new(trader).amount(DEFAULT_DEPOSIT).build())
            .unwrap();
    }
    ctx.execute_block();

    // Pair up traders: t1-t2, t3-t4, t5-t6
    // Each pair trades 1 BTC
    for (buyer, seller) in [("t1", "t2"), ("t3", "t4"), ("t5", "t6")] {
        ctx.state
            .submit_tx(
                OrderBuilder::bid(buyer)
                    .at_price(DEFAULT_BTC_PRICE)
                    .with_size(ONE_BTC)
                    .build(),
            )
            .unwrap();
        ctx.state
            .submit_tx(
                OrderBuilder::ask(seller)
                    .at_price(DEFAULT_BTC_PRICE)
                    .with_size(ONE_BTC)
                    .build(),
            )
            .unwrap();
    }
    ctx.execute_block();

    // Verify all positions
    for (buyer, seller) in [("t1", "t2"), ("t3", "t4"), ("t5", "t6")] {
        assert_position(&ctx.state, buyer, BTC_SYMBOL, ONE_BTC);
        assert_position(&ctx.state, seller, BTC_SYMBOL, -ONE_BTC);
    }

    // Book should be empty (all matched)
    assert_empty_book(&ctx.state, BTC_SYMBOL);
}
