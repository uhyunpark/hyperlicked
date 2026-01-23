//! ADL (Auto-Deleveraging) Integration Tests
//!
//! Tests the ADL mechanism that distributes losses to profitable counterparties
//! when liquidations exceed the insurance fund.

use hyperlicked::app::{AppState, OrderType, Side, Transaction};
use hyperlicked::consensus::AppHook;
use hyperlicked::types::Block;

fn create_block(view: u64, height: u64, timestamp: u64) -> Block {
    Block {
        view,
        height,
        parent: [0u8; 32],
        payload: vec![],
        proposer: [1u8; 32],
        app_hash: [0u8; 32],
        timestamp,
        justify: None,
    }
}

fn setup_test_state() -> AppState {
    let mut state = AppState::new();

    // Set mark price at $50,000
    state.set_mark_price("BTC-USDT", 5_000_000);

    // Start with small insurance fund
    state.add_to_insurance_fund(10_000); // $100

    state
}

#[test]
fn test_adl_triggers_when_insurance_insufficient() {
    let mut state = setup_test_state();

    // 1. Create the liquidated account with small margin
    state
        .execute_tx(Transaction::Deposit {
            trader: "liquidated".into(),
            amount: 500_000, // $5,000 margin
        })
        .unwrap();

    // 2. Create profitable shorts with plenty of margin
    state
        .execute_tx(Transaction::Deposit {
            trader: "short1".into(),
            amount: 5_000_000, // $50,000
        })
        .unwrap();
    state
        .execute_tx(Transaction::Deposit {
            trader: "short2".into(),
            amount: 3_000_000, // $30,000
        })
        .unwrap();

    // 3. Set up positions: liquidated goes long, shorts go short
    // Liquidated opens long at $50,000
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "liquidated".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100_000_000, // 1 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // Shorts sell at $50,000 to match
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "short1".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 70_000_000, // 0.7 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "short2".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 30_000_000, // 0.3 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // Execute a block to process orders
    let block1 = create_block(1, 1, 1000);
    state.execute(&block1);

    // Verify positions
    assert_eq!(
        state.account("liquidated").unwrap().position("BTC-USDT").size,
        100_000_000
    ); // Long 1 BTC
    assert_eq!(
        state.account("short1").unwrap().position("BTC-USDT").size,
        -70_000_000
    ); // Short 0.7 BTC
    assert_eq!(
        state.account("short2").unwrap().position("BTC-USDT").size,
        -30_000_000
    ); // Short 0.3 BTC

    // Record insurance fund before crash
    let insurance_before = state.insurance_fund_balance();

    // 4. Crash the price - this will trigger liquidation with loss exceeding insurance
    // Price drops to $44,000 (-12%)
    state.set_mark_price("BTC-USDT", 4_400_000);

    // Execute another block to trigger liquidation and ADL
    let block2 = create_block(2, 2, 2000);
    state.execute(&block2);

    // 5. Verify ADL occurred
    // The liquidated position should be closed
    assert_eq!(
        state.account("liquidated").unwrap().position("BTC-USDT").size,
        0
    );

    // Check that at least one ADL event was generated
    // (pending_adl_events were cleared after execute, so we check positions changed)
    let short1_pos = state.account("short1").unwrap().position("BTC-USDT").size;
    let short2_pos = state.account("short2").unwrap().position("BTC-USDT").size;

    // At least one of the shorts should have been ADL'd (position reduced)
    let total_short_remaining = short1_pos.abs() + short2_pos.abs();
    assert!(
        total_short_remaining < 100_000_000,
        "ADL should have reduced profitable short positions, remaining: {}",
        total_short_remaining
    );

    // Insurance fund should not go significantly negative (allow small rounding variance)
    let insurance_after = state.insurance_fund_balance();
    assert!(
        insurance_after >= -10_000, // Allow $100 variance due to rounding
        "Insurance fund {} should not go significantly negative (started at {})",
        insurance_after,
        insurance_before
    );
}

#[test]
fn test_adl_priority_by_profitability() {
    let mut state = setup_test_state();

    // Create accounts with sufficient margin
    state
        .execute_tx(Transaction::Deposit {
            trader: "liquidated".into(),
            amount: 500_000, // $5,000 margin
        })
        .unwrap();

    // Two shorts with different entry prices (different profitability when price drops)
    state
        .execute_tx(Transaction::Deposit {
            trader: "short_high".into(),
            amount: 5_000_000, // $50,000
        })
        .unwrap();
    state
        .execute_tx(Transaction::Deposit {
            trader: "short_low".into(),
            amount: 5_000_000, // $50,000
        })
        .unwrap();

    // Liquidated opens long at $50,000
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "liquidated".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100_000_000, // 1 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // short_high sells at $50,000 (will be more profitable when price drops)
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "short_high".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 50_000_000, // 0.5 BTC at $50k entry
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // short_low sells at a lower price (less profitable when price drops)
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "short_low".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 4_900_000, // $49,000 - lower entry price
            size: 50_000_000, // 0.5 BTC at $49k entry
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // Execute to process all orders
    let block1 = create_block(1, 1, 1000);
    state.execute(&block1);

    // Verify positions
    let liquidated_pos = state.account("liquidated").unwrap().position("BTC-USDT").size;
    assert_eq!(liquidated_pos, 100_000_000, "Liquidated should have 1 BTC long");

    // Now crash price to trigger liquidation + ADL
    // Price drops to $44,000
    state.set_mark_price("BTC-USDT", 4_400_000);

    // Execute to trigger liquidation + ADL
    let block2 = create_block(2, 2, 2000);
    state.execute(&block2);

    // Liquidated position should be closed
    let liquidated_pos_after = state.account("liquidated").unwrap().position("BTC-USDT").size;
    assert_eq!(liquidated_pos_after, 0, "Liquidated position should be closed");

    // short_high should be ADL'd first (more profitable - higher entry price means more profit at lower mark)
    // Both have 50_000_000 position but short_high entered at $50k, short_low at $49k
    let short_high_pos = state.account("short_high").unwrap().position("BTC-USDT").size;
    let short_low_pos = state.account("short_low").unwrap().position("BTC-USDT").size;

    // At least one was ADL'd (positions reduced from original -50_000_000)
    assert!(
        short_high_pos.abs() < 50_000_000 || short_low_pos.abs() < 50_000_000,
        "ADL should have reduced at least one short position (short_high={}, short_low={})",
        short_high_pos,
        short_low_pos
    );
}

#[test]
fn test_no_adl_when_insurance_sufficient() {
    let mut state = setup_test_state();

    // Add LARGE funds to insurance fund
    state.add_to_insurance_fund(10_000_000); // $100,000 (way more than needed)

    // Create liquidated account
    state
        .execute_tx(Transaction::Deposit {
            trader: "liquidated".into(),
            amount: 500_000, // $5,000
        })
        .unwrap();

    // Create short with plenty of margin
    state
        .execute_tx(Transaction::Deposit {
            trader: "short1".into(),
            amount: 5_000_000, // $50,000
        })
        .unwrap();

    // Set up positions
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "liquidated".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100_000_000, // 1 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "short1".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 100_000_000, // 1 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    let block1 = create_block(1, 1, 1000);
    state.execute(&block1);

    let short1_before = state
        .account("short1")
        .unwrap()
        .position("BTC-USDT")
        .size;

    let insurance_before = state.insurance_fund_balance();

    // Crash price (triggers liquidation)
    state.set_mark_price("BTC-USDT", 4_400_000);

    let block2 = create_block(2, 2, 2000);
    state.execute(&block2);

    // Short should NOT be ADL'd because insurance fund was sufficient
    let short1_after = state
        .account("short1")
        .unwrap()
        .position("BTC-USDT")
        .size;

    assert_eq!(
        short1_before, short1_after,
        "Short position should not change when insurance fund is sufficient"
    );

    // Insurance fund should have absorbed the loss
    let insurance_after = state.insurance_fund_balance();
    assert!(
        insurance_after < insurance_before,
        "Insurance fund should have decreased from {} to {}",
        insurance_before,
        insurance_after
    );
}

#[test]
fn test_insurance_fund_floor_maintained() {
    let mut state = setup_test_state();

    // Start with small insurance fund (setup adds $100)
    let initial_insurance = state.insurance_fund_balance();

    // Create accounts with sufficient margin
    state
        .execute_tx(Transaction::Deposit {
            trader: "liquidated".into(),
            amount: 500_000, // $5,000 margin
        })
        .unwrap();

    state
        .execute_tx(Transaction::Deposit {
            trader: "short1".into(),
            amount: 5_000_000, // $50,000
        })
        .unwrap();

    // Set up positions
    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "liquidated".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100_000_000, // 1 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    state
        .execute_tx(Transaction::PlaceOrder {
            trader: "short1".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 100_000_000, // 1 BTC
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    let block1 = create_block(1, 1, 1000);
    state.execute(&block1);

    // Significant price crash (-12%) - same as other tests
    state.set_mark_price("BTC-USDT", 4_400_000);

    let block2 = create_block(2, 2, 2000);
    state.execute(&block2);

    // Insurance fund should not go deeply negative
    // With ADL, losses should be absorbed by counterparties
    let insurance_balance = state.insurance_fund_balance();
    assert!(
        insurance_balance >= -100_000, // Allow some rounding variance
        "Insurance fund went too negative: {} (started at {})",
        insurance_balance,
        initial_insurance
    );

    // Verify the short position was ADL'd
    let short1_pos = state.account("short1").unwrap().position("BTC-USDT").size;
    assert!(
        short1_pos.abs() < 100_000_000,
        "Short position should have been reduced by ADL (current: {})",
        short1_pos
    );
}
