//! Trigger Order Tests
//!
//! Tests for Stop Loss and Take Profit order execution.

use super::AppState;
use crate::app::trigger::{TriggerError, TriggerOrderStatus, TriggerType};
use crate::app::{orderbook::Side, OrderType, Transaction};

fn setup_state_with_position() -> AppState {
    let mut state = AppState::new();

    // Deposit for alice
    state
        .execute_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000, // $1M
        })
        .unwrap();

    // Deposit for bob
    state
        .execute_tx(Transaction::Deposit {
            trader: "bob".into(),
            amount: 100_000_000,
        })
        .unwrap();

    // Bob places ask at $50,000
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 100_000_000, // 1 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // Alice buys -> creates long position
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 50_000_000, // 0.5 BTC
            order_type: OrderType::Ioc,
            reduce_only: false,
        })
        .unwrap();

    state
}

#[test]
fn test_place_stop_loss() {
    let mut state = setup_state_with_position();

    // Verify alice has a long position
    let pos = state.account("alice").unwrap().position("BTC-USDT");
    assert!(pos.size > 0);

    // Place stop loss below current price
    let result = state.execute_place_trigger_order(
        "alice".into(),
        "BTC-USDT".into(),
        TriggerType::StopLoss,
        4_500_000, // $45,000 (below $50,000 mark)
        pos.size,
        None,
        None,
    );

    assert!(result.is_ok());
    let id = result.unwrap();

    // Verify trigger order exists
    let trigger = state.trigger_order(&id).unwrap();
    assert_eq!(trigger.status, TriggerOrderStatus::Pending);
    assert_eq!(trigger.trigger_price, 4_500_000);
}

#[test]
fn test_place_take_profit() {
    let mut state = setup_state_with_position();

    let pos = state.account("alice").unwrap().position("BTC-USDT");

    // Place take profit above current price
    let result = state.execute_place_trigger_order(
        "alice".into(),
        "BTC-USDT".into(),
        TriggerType::TakeProfit,
        5_500_000, // $55,000 (above $50,000 mark)
        pos.size,
        None,
        None,
    );

    assert!(result.is_ok());
}

#[test]
fn test_invalid_sl_price() {
    let mut state = setup_state_with_position();

    let pos = state.account("alice").unwrap().position("BTC-USDT");

    // Try to place stop loss ABOVE current price (invalid for long)
    let result = state.execute_place_trigger_order(
        "alice".into(),
        "BTC-USDT".into(),
        TriggerType::StopLoss,
        5_500_000, // Above mark price - invalid!
        pos.size,
        None,
        None,
    );

    assert!(matches!(result, Err(TriggerError::InvalidTriggerPrice)));
}

#[test]
fn test_cancel_trigger_order() {
    let mut state = setup_state_with_position();

    let pos = state.account("alice").unwrap().position("BTC-USDT");

    let id = state
        .execute_place_trigger_order(
            "alice".into(),
            "BTC-USDT".into(),
            TriggerType::StopLoss,
            4_500_000,
            pos.size,
            None,
            None,
        )
        .unwrap();

    // Cancel it
    let result = state.execute_cancel_trigger_order("alice".into(), id.clone());
    assert!(result.is_ok());

    // Verify status is cancelled
    let trigger = state.trigger_order(&id).unwrap();
    assert_eq!(trigger.status, TriggerOrderStatus::Cancelled);
}

#[test]
fn test_trigger_executes_on_price_drop() {
    let mut state = setup_state_with_position();

    let pos_size = state.account("alice").unwrap().position("BTC-USDT").size;

    // Place stop loss
    let id = state
        .execute_place_trigger_order(
            "alice".into(),
            "BTC-USDT".into(),
            TriggerType::StopLoss,
            4_500_000,
            pos_size,
            None,
            None,
        )
        .unwrap();

    // Price is at $50,000 - trigger should NOT fire
    let fills = state.process_triggers();
    assert!(fills.is_empty());
    assert_eq!(
        state.trigger_order(&id).unwrap().status,
        TriggerOrderStatus::Pending
    );

    // Drop mark price below trigger
    state.set_mark_price("BTC-USDT", 4_400_000); // $44,000

    // Need a counterparty to fill the order
    // Bob places a bid to match alice's stop loss sell
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 4_400_000,
            size: 50_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // Now process triggers
    let fills = state.process_triggers();

    // Should have executed
    assert!(!fills.is_empty());
    assert_eq!(
        state.trigger_order(&id).unwrap().status,
        TriggerOrderStatus::Triggered
    );

    // Alice's position should be reduced/closed
    let new_pos = state.account("alice").unwrap().position("BTC-USDT");
    assert!(new_pos.size < pos_size);
}
