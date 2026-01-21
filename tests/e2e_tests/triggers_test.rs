//! Trigger Order Tests
//!
//! Tests for stop-loss and take-profit trigger orders.

use hyperlicked::app::{Transaction, TriggerType};

use crate::e2e::helpers::*;

/// Test that stop-loss triggers on price drop (long position)
#[test]
fn test_stop_loss_triggers_long() {
    let mut ctx = TestContext::with_default_traders();

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

    // Verify long position
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);

    // Place stop loss at $48,000 (below current price)
    ctx.state
        .submit_tx(
            TriggerOrderBuilder::stop_loss(TRADER_ALICE)
                .at_trigger_price(4_800_000) // $48,000
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Verify trigger order exists
    assert_trigger_order_count(&ctx.state, TRADER_ALICE, 1);

    // Price drops below trigger
    ctx.set_mark_price(BTC_SYMBOL, 4_700_000); // $47,000

    // Bob provides liquidity to fill the stop loss
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(4_700_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should be closed (stop loss executed)
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}

/// Test that take-profit triggers on price rise (long position)
#[test]
fn test_take_profit_triggers_long() {
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

    // Place take profit at $52,000 (above current price)
    ctx.state
        .submit_tx(
            TriggerOrderBuilder::take_profit(TRADER_ALICE)
                .at_trigger_price(5_200_000) // $52,000
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    assert_trigger_order_count(&ctx.state, TRADER_ALICE, 1);

    // Price rises above trigger
    ctx.set_mark_price(BTC_SYMBOL, 5_300_000); // $53,000

    // Bob provides liquidity
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(5_300_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should be closed (take profit executed)
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}

/// Test that cancelled trigger doesn't execute
#[test]
fn test_cancel_trigger_doesnt_execute() {
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

    // Place stop loss
    ctx.state
        .submit_tx(
            TriggerOrderBuilder::stop_loss(TRADER_ALICE)
                .at_trigger_price(4_800_000)
                .with_size(ONE_BTC)
                .with_cloid("sl-1")
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Cancel the trigger order by cloid
    ctx.state
        .submit_tx(Transaction::CancelTriggerOrderByCloid {
            trader: TRADER_ALICE.into(),
            symbol: BTC_SYMBOL.into(),
            cloid: "sl-1".into(),
        })
        .unwrap();
    ctx.execute_block();

    // Price drops below what was the trigger price
    ctx.set_mark_price(BTC_SYMBOL, 4_700_000);

    // Bob provides liquidity
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_BOB)
                .at_price(4_700_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should still exist (cancelled trigger didn't execute)
    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, ONE_BTC);
}

/// Test stop loss for short position (triggers on price rise)
#[test]
fn test_stop_loss_triggers_short() {
    let mut ctx = TestContext::with_default_traders();

    // Alice goes short
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

    // Place stop loss at $52,000 (above current price - losing direction for short)
    ctx.state
        .submit_tx(Transaction::PlaceTriggerOrder {
            trader: TRADER_ALICE.into(),
            symbol: BTC_SYMBOL.into(),
            trigger_type: TriggerType::StopLoss,
            trigger_price: 5_200_000, // $52,000
            size: ONE_BTC,
            limit_price: None,
            cloid: None,
        })
        .unwrap();
    ctx.execute_block();

    // Price rises above trigger (bad for short)
    ctx.set_mark_price(BTC_SYMBOL, 5_300_000);

    // Alice provides liquidity for her own closing order (Bob's ask)
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(5_300_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should be closed
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}

/// Test take profit for short position (triggers on price drop)
#[test]
fn test_take_profit_triggers_short() {
    let mut ctx = TestContext::with_default_traders();

    // Alice goes short
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

    assert_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL, -ONE_BTC);

    // Place take profit at $48,000 (below current price - winning direction for short)
    ctx.state
        .submit_tx(Transaction::PlaceTriggerOrder {
            trader: TRADER_ALICE.into(),
            symbol: BTC_SYMBOL.into(),
            trigger_type: TriggerType::TakeProfit,
            trigger_price: 4_800_000,
            size: ONE_BTC,
            limit_price: None,
            cloid: None,
        })
        .unwrap();
    ctx.execute_block();

    // Price drops below trigger (good for short)
    ctx.set_mark_price(BTC_SYMBOL, 4_700_000);

    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(4_700_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Position should be closed
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}

/// Test trigger order fails if position is closed before trigger
#[test]
fn test_trigger_fails_if_position_closed() {
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

    // Place stop loss
    ctx.state
        .submit_tx(
            TriggerOrderBuilder::stop_loss(TRADER_ALICE)
                .at_trigger_price(4_800_000)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Alice manually closes her position
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

    // Position is now closed
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);

    // Price drops to trigger level
    ctx.set_mark_price(BTC_SYMBOL, 4_700_000);

    // Execute block - trigger should fail since no position
    ctx.execute_block();

    // Position should remain zero (trigger failed gracefully)
    assert_no_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
}
