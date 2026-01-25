//! Tests for incremental state hashing
//!
//! Verifies that:
//! - State hashes are consistent (same inputs = same outputs)
//! - Hashes change when state changes
//! - Performance with many accounts
//!
//! Note: When incremental_hash feature is enabled, the hash algorithm differs
//! from the full hash. These tests verify consistency, not algorithm equality.

use hyperlicked::app::{
    orderbook::{OrderType, Side},
    Transaction,
};
use hyperlicked::app::state::AppState;
use hyperlicked::consensus::AppHook;
use hyperlicked::types::Block;

fn create_test_block(height: u64) -> Block {
    Block {
        view: height,
        height,
        parent: [0u8; 32],
        payload: vec![],
        proposer: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000 + height * 100,
        justify: None,
    }
}

#[test]
fn test_hash_consistency() {
    let mut state = AppState::new();

    // Setup: multiple traders with deposits and orders
    for i in 0..10 {
        state.submit_tx(Transaction::Deposit {
            trader: format!("trader_{}", i),
            amount: 10_000_000_000,
        }).unwrap();
    }

    // Execute deposits
    let block = create_test_block(1);
    let hash1 = state.execute(&block);

    // Verify the hash is not zero (was actually computed)
    assert_ne!(hash1, [0u8; 32], "Hash should be computed");

    // Add some orders
    for i in 0..5 {
        state.submit_tx(Transaction::PlaceOrder {
            trader: format!("trader_{}", i),
            symbol: "BTC-USDT".into(),
            side: if i % 2 == 0 { Side::Bid } else { Side::Ask },
            price: 5_000_000 + (i as i64 * 1000),
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }).unwrap();
    }

    // Execute orders
    let block = create_test_block(2);
    let hash2 = state.execute(&block);

    // Verify hashes changed
    assert_ne!(hash1, hash2, "Hash should change after orders");
}

#[test]
fn test_stress_many_accounts() {
    let mut state = AppState::new();

    // Create many accounts
    let num_accounts = 100;
    for i in 0..num_accounts {
        state.submit_tx(Transaction::Deposit {
            trader: format!("trader_{}", i),
            amount: 10_000_000 + i as i64,
        }).unwrap();
    }

    // Execute block
    let block = create_test_block(1);
    let hash1 = state.execute(&block);

    // Verify hash is computed
    assert_ne!(hash1, [0u8; 32], "Hash should be computed with {} accounts", num_accounts);

    // Modify only a few accounts
    for i in 0..5 {
        state.submit_tx(Transaction::Deposit {
            trader: format!("trader_{}", i),
            amount: 1000,
        }).unwrap();
    }

    // Execute modification block
    let block = create_test_block(2);
    let hash2 = state.execute(&block);

    // Verify hash changed
    assert_ne!(hash1, hash2, "Hash should change after modifications");
}

#[test]
fn test_hash_stability_across_runs() {
    // Same operations should produce same hash
    fn run_scenario() -> [u8; 32] {
        let mut state = AppState::new();

        for i in 0..20 {
            state.submit_tx(Transaction::Deposit {
                trader: format!("trader_{}", i),
                amount: 1_000_000 * (i as i64 + 1),
            }).unwrap();
        }

        let block = create_test_block(1);
        state.execute(&block)
    }

    let hash1 = run_scenario();
    let hash2 = run_scenario();

    assert_eq!(hash1, hash2, "Same operations should produce same hash");
}

#[test]
fn test_trade_changes_hash() {
    let mut state = AppState::new();

    // Setup initial state
    state.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 100_000_000,
    }).unwrap();

    state.submit_tx(Transaction::Deposit {
        trader: "bob".into(),
        amount: 100_000_000,
    }).unwrap();

    let block = create_test_block(1);
    let hash1 = state.execute(&block);

    // Create a trade (modifies mark price / orderbook state)
    state.submit_tx(Transaction::PlaceOrder {
        trader: "alice".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Bid,
        price: 5_000_000,
        size: 100_000_000,
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    state.submit_tx(Transaction::PlaceOrder {
        trader: "bob".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Ask,
        price: 5_000_000,
        size: 100_000_000,
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    let block = create_test_block(2);
    let hash2 = state.execute(&block);

    // Verify hash changed (trade occurred)
    assert_ne!(hash1, hash2, "Hash should change after trade");
}

#[test]
fn test_multiple_symbols_hash_changes() {
    let mut state = AppState::new();

    // Add another market
    state.add_market(hyperlicked::app::MarketConfig {
        symbol: "ETH-USDT".into(),
        ..Default::default()
    });

    // Setup with two traders
    state.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 500_000_000,
    }).unwrap();

    state.submit_tx(Transaction::Deposit {
        trader: "bob".into(),
        amount: 500_000_000,
    }).unwrap();

    let block = create_test_block(1);
    let hash_initial = state.execute(&block);

    // Place orders that match (creates a fill, changes account state)
    state.submit_tx(Transaction::PlaceOrder {
        trader: "alice".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Bid,
        price: 5_000_000,
        size: 100_000_000,
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    state.submit_tx(Transaction::PlaceOrder {
        trader: "bob".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Ask,
        price: 5_000_000,
        size: 100_000_000,
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    let block = create_test_block(2);
    let hash_btc = state.execute(&block);

    // Verify hash changed from initial
    assert_ne!(hash_initial, hash_btc, "Hash should change after BTC trade");

    // Trade on ETH market
    state.submit_tx(Transaction::PlaceOrder {
        trader: "alice".into(),
        symbol: "ETH-USDT".into(),
        side: Side::Bid,
        price: 300_000,
        size: 100_000_000,
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    state.submit_tx(Transaction::PlaceOrder {
        trader: "bob".into(),
        symbol: "ETH-USDT".into(),
        side: Side::Ask,
        price: 300_000,
        size: 100_000_000,
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    let block = create_test_block(3);
    let hash_eth = state.execute(&block);

    // Verify hashes are different (positions changed)
    assert_ne!(hash_btc, hash_eth, "Hashes should differ after ETH trade");
}
