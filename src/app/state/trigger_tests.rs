//! Trigger Order Tests
//!
//! Tests for Stop Loss and Take Profit order execution.

use super::AppState;
use crate::app::trigger::{
    TriggerCondition, TriggerError, TriggerEventType, TriggerOrderStatus,
    TriggerOrderValidationError, TriggerType,
};
use crate::app::{orderbook::Side, OrderType, Transaction};
use crate::types::{CommitmentV2, EventRecord, EventType};

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

fn state_with_trigger() -> (AppState, String) {
    let mut state = setup_state_with_position();
    let position_size = state.account("alice").unwrap().position("BTC-USDT").size;
    let id = state
        .execute_place_trigger_order(
            "alice".into(),
            "BTC-USDT".into(),
            TriggerType::StopLoss,
            4_500_000,
            position_size,
            Some(4_400_000),
            Some("client-1".into()),
        )
        .unwrap();
    (state, id)
}

#[test]
fn validate_trigger_orders_accepts_imported_order_after_position_disappears() {
    let (mut state, _) = state_with_trigger();
    state
        .accounts_mut()
        .get_or_create("alice")
        .positions
        .get_mut("BTC-USDT")
        .unwrap()
        .size = 0;

    // Recovery may admit a still-pending order whose position changed after
    // placement. Trigger execution handles that case and cleans it up.
    assert!(state.validate_trigger_orders().is_ok());
}

#[test]
fn validate_trigger_orders_rejects_malformed_primary_fields() {
    let (state, id) = state_with_trigger();

    let mut bad = state.clone();
    bad.trigger_orders.get_mut(&id).unwrap().size = 0;
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::InvalidSize { .. })
    ));

    let mut bad = state.clone();
    bad.trigger_orders.get_mut(&id).unwrap().trigger_price = 0;
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::InvalidTriggerPrice { .. })
    ));

    let mut bad = state.clone();
    bad.trigger_orders.get_mut(&id).unwrap().limit_price = Some(0);
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::InvalidLimitPrice { .. })
    ));

    let mut bad = state.clone();
    bad.trigger_orders.get_mut(&id).unwrap().reduce_only = false;
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::NotReduceOnly { .. })
    ));

    let mut bad = state.clone();
    bad.trigger_orders.get_mut(&id).unwrap().condition = TriggerCondition::PriceAbove;
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::ConditionMismatch { .. })
    ));

    let mut bad = state.clone();
    bad.trigger_orders.get_mut(&id).unwrap().status = TriggerOrderStatus::Cancelled;
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::InvalidStatus { .. })
    ));

    let mut bad = state.clone();
    bad.trigger_orders.get_mut(&id).unwrap().cloid = Some(String::new());
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::EmptyCloid { .. })
    ));
}

#[test]
fn validate_trigger_orders_rejects_invalid_id_sequence_and_market() {
    let (state, id) = state_with_trigger();

    let mut bad = state.clone();
    let mut order = bad.trigger_orders.remove(&id).unwrap();
    order.id = "T0".into();
    bad.trigger_orders.insert("T0".into(), order);
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::InvalidId { .. })
    ));

    let mut bad = state.clone();
    bad.trigger_seq = 0;
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::SequenceBehind { .. })
    ));

    let mut bad = state.clone();
    bad.configs.remove("BTC-USDT");
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::MarketNotFound { .. })
    ));

    let mut bad = state.clone();
    bad.orderbooks.remove("BTC-USDT");
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::OrderbookNotFound { .. })
    ));

    let mut bad = state.clone();
    bad.trigger_orders.get_mut(&id).unwrap().id = "wrong-id".into();
    assert!(matches!(
        bad.validate_trigger_orders(),
        Err(TriggerOrderValidationError::OrderIdMismatch { .. })
    ));
}

#[test]
fn trigger_placement_rejects_invalid_primary_fields_before_mutation() {
    let state = setup_state_with_position();

    for (size, limit_price, cloid, expected) in [
        (0, None, None, TriggerError::InvalidSize),
        (1, Some(0), None, TriggerError::InvalidLimitPrice),
        (1, None, Some(String::new()), TriggerError::InvalidCloid),
    ] {
        let mut candidate = state.clone();
        let before_sequence = candidate.trigger_seq;
        let before_orders = candidate.trigger_orders.len();
        let result = candidate.execute_place_trigger_order(
            "alice".into(),
            "BTC-USDT".into(),
            TriggerType::StopLoss,
            4_500_000,
            size,
            limit_price,
            cloid,
        );

        assert_eq!(result.unwrap_err().to_string(), expected.to_string());
        assert_eq!(candidate.trigger_seq, before_sequence);
        assert_eq!(candidate.trigger_orders.len(), before_orders);
    }
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

    // Verify order has been cleaned up from indexes
    assert!(state.trigger_order(&id).is_none());
    assert!(state.validate_trigger_indexes().is_ok());
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

    // Verify order has been cleaned up from indexes after trigger
    assert!(state.trigger_order(&id).is_none());
    assert!(state.validate_trigger_indexes().is_ok());

    // Alice's position should be reduced/closed
    let new_pos = state.account("alice").unwrap().position("BTC-USDT");
    assert!(new_pos.size < pos_size);
}

#[test]
fn trigger_state_and_artifact_roots_ignore_market_insertion_order() {
    fn run(order: &[&str]) -> ([u8; 32], [u8; 32]) {
        let mut state = AppState::new();
        state.timestamp = 1;
        for symbol in order {
            state.add_market(crate::app::MarketConfig {
                symbol: (*symbol).to_string(),
                ..crate::app::MarketConfig::default()
            });
        }

        let positions = [
            ("alice", "BTC-USDT", 5_000_000),
            ("bob", "ETH-USDT", 5_000_000),
            ("carol", "SOL-USDT", 5_000_000),
        ];
        for (address, symbol, entry_price) in positions {
            let account = state.accounts_mut().get_or_create(address);
            account.balance = 100_000_000;
            account.apply_fill(symbol, true, 10_000_000, entry_price);
            state.set_mark_price(symbol, entry_price);
            state
                .execute_place_trigger_order(
                    address.to_string(),
                    symbol.to_string(),
                    TriggerType::StopLoss,
                    entry_price - 100_000,
                    10_000_000,
                    None,
                    None,
                )
                .expect("trigger placement succeeds");
        }

        for (symbol, mark_price) in [
            ("BTC-USDT", 4_800_000),
            ("ETH-USDT", 4_800_000),
            ("SOL-USDT", 4_800_000),
        ] {
            state.set_mark_price(symbol, mark_price);
        }
        state.process_triggers();

        let updates = state.take_pending_order_updates();
        let fills = state.take_pending_fills();
        let triggers = state.take_pending_trigger_events();
        let mut events = Vec::new();
        for update in &updates {
            events.push(
                EventRecord::from_bincode(events.len() as u32, EventType::ORDER_UPDATE, update)
                    .expect("order update encodes"),
            );
        }
        for fill in &fills {
            events.push(
                EventRecord::from_bincode(events.len() as u32, EventType::FILL, fill)
                    .expect("fill encodes"),
            );
        }
        for trigger in &triggers {
            events.push(
                EventRecord::from_bincode(events.len() as u32, EventType::TRIGGER, trigger)
                    .expect("trigger encodes"),
            );
        }
        let artifact_root = CommitmentV2::new_with_system_events(Vec::new(), events)
            .expect("commitment validates")
            .root()
            .expect("commitment root computes");
        (state.compute_state_hash(), artifact_root)
    }

    let first = run(&["ETH-USDT", "SOL-USDT"]);
    let second = run(&["SOL-USDT", "ETH-USDT"]);
    assert_eq!(first, second);
}

#[test]
fn rebuild_trigger_indexes_repairs_sorted_vectors() {
    let mut state = setup_state_with_position();
    let position_size = state.account("alice").unwrap().position("BTC-USDT").size;
    let first = state
        .execute_place_trigger_order(
            "alice".into(),
            "BTC-USDT".into(),
            TriggerType::StopLoss,
            4_500_000,
            position_size / 2,
            None,
            Some("first".into()),
        )
        .unwrap();
    let second = state
        .execute_place_trigger_order(
            "alice".into(),
            "BTC-USDT".into(),
            TriggerType::StopLoss,
            4_400_000,
            position_size / 2,
            None,
            Some("second".into()),
        )
        .unwrap();

    state
        .trigger_orders_by_trader
        .get_mut("alice")
        .unwrap()
        .reverse();
    state
        .trigger_orders_by_symbol
        .get_mut("BTC-USDT")
        .unwrap()
        .reverse();
    assert!(state.validate_trigger_indexes().is_err());

    state.rebuild_trigger_indexes().unwrap();
    assert_eq!(
        state.trigger_orders_by_trader.get("alice").unwrap(),
        &vec![first.clone(), second.clone()]
    );
    assert_eq!(
        state.trigger_orders_by_symbol.get("BTC-USDT").unwrap(),
        &vec![first, second]
    );
    assert!(state.validate_trigger_indexes().is_ok());
}

#[test]
fn rebuild_trigger_indexes_uses_numeric_trigger_id_order() {
    let mut state = setup_state_with_position();
    let position_size = state.account("alice").unwrap().position("BTC-USDT").size;
    for index in 0..12 {
        state
            .execute_place_trigger_order(
                "alice".into(),
                "BTC-USDT".into(),
                TriggerType::StopLoss,
                4_500_000 - index,
                position_size / 20,
                None,
                None,
            )
            .unwrap();
    }

    state
        .trigger_orders_by_trader
        .get_mut("alice")
        .unwrap()
        .clear();
    state
        .trigger_orders_by_symbol
        .get_mut("BTC-USDT")
        .unwrap()
        .clear();

    state.rebuild_trigger_indexes().unwrap();
    let expected: Vec<_> = (1..=12).map(|index| format!("T{index}")).collect();
    assert_eq!(
        state.trigger_orders_by_trader.get("alice").unwrap(),
        &expected
    );
    assert_eq!(
        state.trigger_orders_by_symbol.get("BTC-USDT").unwrap(),
        &expected
    );
    assert!(state.validate_trigger_indexes().is_ok());
}

#[test]
fn trigger_id_order_is_total_for_runtime_and_imported_ids() {
    let mut ids = vec!["T2", "legacy", "T10", "T1", "T01", "other"];
    ids.sort_by(|left, right| super::compare_trigger_ids(left, right));
    assert_eq!(ids, vec!["T01", "T1", "T2", "T10", "legacy", "other"]);
}

#[test]
fn process_triggers_uses_numeric_trigger_id_order() {
    let mut state = setup_state_with_position();
    let position_size = state.account("alice").unwrap().position("BTC-USDT").size;
    for index in 0..12 {
        state
            .execute_place_trigger_order(
                "alice".into(),
                "BTC-USDT".into(),
                TriggerType::StopLoss,
                4_500_000 - index,
                position_size / 20,
                None,
                None,
            )
            .unwrap();
    }
    // Discard placement events so the remaining trigger events expose only
    // execution order.
    state.take_pending_trigger_events();

    state.set_mark_price("BTC-USDT", 4_400_000);
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 4_400_000,
            size: position_size,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();
    state.process_triggers();

    let executed_ids: Vec<_> = state
        .take_pending_trigger_events()
        .into_iter()
        .filter_map(|event| {
            if matches!(event.event_type, TriggerEventType::Triggered { .. }) {
                Some(event.id)
            } else {
                None
            }
        })
        .collect();
    let expected: Vec<_> = (1..=12).map(|index| format!("T{index}")).collect();
    assert_eq!(executed_ids, expected);
    assert!(state.validate_trigger_indexes().is_ok());
}

#[test]
fn rebuild_trigger_indexes_rejects_corruption_without_partial_mutation() {
    let mut state = setup_state_with_position();
    let position_size = state.account("alice").unwrap().position("BTC-USDT").size;
    let id = state
        .execute_place_trigger_order(
            "alice".into(),
            "BTC-USDT".into(),
            TriggerType::StopLoss,
            4_500_000,
            position_size,
            None,
            Some("same-cloid".into()),
        )
        .unwrap();
    let before_trader = state.trigger_orders_by_trader.clone();
    let before_symbol = state.trigger_orders_by_symbol.clone();
    let before_cloid = state.trigger_orders_by_cloid.clone();

    state.trigger_orders.get_mut(&id).unwrap().id = "wrong-id".into();
    assert!(matches!(
        state.rebuild_trigger_indexes(),
        Err(crate::app::state::TriggerIndexError::OrderIdMismatch)
    ));
    assert_eq!(state.trigger_orders_by_trader, before_trader);
    assert_eq!(state.trigger_orders_by_symbol, before_symbol);
    assert_eq!(state.trigger_orders_by_cloid, before_cloid);
}

#[test]
fn rebuild_trigger_indexes_rejects_duplicate_cloid_and_stale_sequence() {
    let mut state = setup_state_with_position();
    let position_size = state.account("alice").unwrap().position("BTC-USDT").size;
    let id = state
        .execute_place_trigger_order(
            "alice".into(),
            "BTC-USDT".into(),
            TriggerType::StopLoss,
            4_500_000,
            position_size,
            None,
            Some("same-cloid".into()),
        )
        .unwrap();
    let mut duplicate = state.trigger_orders.get(&id).unwrap().clone();
    duplicate.id = "T2".into();
    state.trigger_orders.insert("T2".into(), duplicate);
    state.trigger_seq = 2;

    let before_trader = state.trigger_orders_by_trader.clone();
    let before_symbol = state.trigger_orders_by_symbol.clone();
    let before_cloid = state.trigger_orders_by_cloid.clone();
    assert!(matches!(
        state.rebuild_trigger_indexes(),
        Err(crate::app::state::TriggerIndexError::DuplicateCloid)
    ));
    assert_eq!(state.trigger_orders_by_trader, before_trader);
    assert_eq!(state.trigger_orders_by_symbol, before_symbol);
    assert_eq!(state.trigger_orders_by_cloid, before_cloid);

    state.trigger_orders.remove("T2");
    state.trigger_seq = 0;
    assert!(matches!(
        state.rebuild_trigger_indexes(),
        Err(crate::app::state::TriggerIndexError::TriggerSequenceBehind)
    ));
    assert_eq!(state.trigger_orders_by_trader, before_trader);
    assert_eq!(state.trigger_orders_by_symbol, before_symbol);
    assert_eq!(state.trigger_orders_by_cloid, before_cloid);
}

#[test]
fn validate_trigger_indexes_rejects_unknown_references() {
    let mut state = setup_state_with_position();
    state
        .trigger_orders_by_trader
        .entry("alice".into())
        .or_default()
        .push("missing".into());

    assert!(matches!(
        state.validate_trigger_indexes(),
        Err(crate::app::state::TriggerIndexError::IndexMismatch)
    ));
}
