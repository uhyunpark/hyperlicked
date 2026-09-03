//! State Hash Consistency Test
//!
//! Tests that identical operations produce identical state hashes,
//! verifying cross-validator determinism.

use hyperlicked::app::{AppState, OrderType, Side, Transaction, TriggerType};
use hyperlicked::consensus::AppHook;
use hyperlicked::types::{Block, ConsensusContext};

/// Helper to execute a transaction on a state
fn execute_tx(state: &mut AppState, tx: Transaction) {
    state.execute_tx(tx).ok(); // Ignore errors for testing
}

/// Test that two independent states with identical operations produce identical hashes
#[test]
fn test_state_hash_determinism() {
    let mut state1 = AppState::new();
    let mut state2 = AppState::new();

    // Apply identical operations to both states
    let operations = vec![
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        },
        Transaction::Deposit {
            trader: "bob".into(),
            amount: 50_000_000,
        },
        Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 10_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        },
        Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 5_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        },
    ];

    for tx in operations {
        execute_tx(&mut state1, tx.clone());
        execute_tx(&mut state2, tx);
    }

    let hash1 = state1.compute_state_hash();
    let hash2 = state2.compute_state_hash();

    assert_eq!(
        hash1, hash2,
        "Identical operations should produce identical state hashes"
    );
}

/// Test that order of independent operations doesn't affect hash
/// (only true for commutative operations like independent deposits)
#[test]
fn test_deposit_order_independence() {
    let mut state1 = AppState::new();
    let mut state2 = AppState::new();

    // State 1: Alice then Bob
    execute_tx(
        &mut state1,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );
    execute_tx(
        &mut state1,
        Transaction::Deposit {
            trader: "bob".into(),
            amount: 200,
        },
    );

    // State 2: Bob then Alice
    execute_tx(
        &mut state2,
        Transaction::Deposit {
            trader: "bob".into(),
            amount: 200,
        },
    );
    execute_tx(
        &mut state2,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );

    // The authenticated schema-v3 root includes replay metadata. Execute the
    // same fixed-timestamp empty block first so the comparison covers only
    // the commutative account mutations.
    let context = ConsensusContext::new(0, [0u8; 32]);
    let genesis = Block::genesis(context);
    let block = Block {
        view: 1,
        height: 1,
        parent: genesis.hash(),
        timestamp: 1,
        ..genesis
    };
    let hash1 = state1.execute(&block);
    let hash2 = state2.execute(&block);

    assert_eq!(
        hash1, hash2,
        "Independent deposits should produce same state regardless of order"
    );
}

/// Test that different operations produce different hashes
#[test]
fn test_different_operations_different_hash() {
    let mut state1 = AppState::new();
    let mut state2 = AppState::new();

    // State 1: Alice deposits 100
    execute_tx(
        &mut state1,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );

    // State 2: Alice deposits 200
    execute_tx(
        &mut state2,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 200,
        },
    );

    let hash1 = state1.compute_state_hash();
    let hash2 = state2.compute_state_hash();

    assert_ne!(
        hash1, hash2,
        "Different amounts should produce different hashes"
    );
}

/// Test state hash includes account balances
#[test]
fn test_hash_includes_balances() {
    let mut state = AppState::new();
    let hash_empty = state.compute_state_hash();

    execute_tx(
        &mut state,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );
    let hash_with_deposit = state.compute_state_hash();

    assert_ne!(
        hash_empty, hash_with_deposit,
        "Deposit should change state hash"
    );
}

/// Test state hash includes positions
#[test]
fn test_hash_includes_positions() {
    let mut state = AppState::new();

    // Setup: deposits
    execute_tx(
        &mut state,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        },
    );
    execute_tx(
        &mut state,
        Transaction::Deposit {
            trader: "bob".into(),
            amount: 100_000_000,
        },
    );

    let hash_no_positions = state.compute_state_hash();

    // Bob places ask
    execute_tx(
        &mut state,
        Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 10_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        },
    );

    // Alice takes (creates positions for both)
    execute_tx(
        &mut state,
        Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 10_000_000,
            order_type: OrderType::Ioc,
            reduce_only: false,
        },
    );

    let hash_with_positions = state.compute_state_hash();

    assert_ne!(
        hash_no_positions, hash_with_positions,
        "Positions should change state hash"
    );
}

/// Test state hash includes insurance fund
#[test]
fn test_hash_includes_insurance_fund() {
    let mut state1 = AppState::new();
    let mut state2 = AppState::new();

    // Same operations
    execute_tx(
        &mut state1,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );
    execute_tx(
        &mut state2,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );

    // Manually modify insurance fund (simulating liquidation)
    state2.add_to_insurance_fund(1000);

    let hash1 = state1.compute_state_hash();
    let hash2 = state2.compute_state_hash();

    assert_ne!(
        hash1, hash2,
        "Insurance fund changes should affect state hash"
    );
}

/// Test state hash includes funding rates
#[test]
fn test_hash_includes_funding_rates() {
    let mut state1 = AppState::new();
    let mut state2 = AppState::new();

    execute_tx(
        &mut state1,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );
    execute_tx(
        &mut state2,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );

    // Set different funding rates
    state1.set_funding_rate("BTC-USDT", 10);
    state2.set_funding_rate("BTC-USDT", 20);

    let hash1 = state1.compute_state_hash();
    let hash2 = state2.compute_state_hash();

    assert_ne!(
        hash1, hash2,
        "Different funding rates should produce different hashes"
    );
}

/// Test state hash includes trigger orders
#[test]
fn test_hash_includes_trigger_orders() {
    let mut state1 = AppState::new();
    let mut state2 = AppState::new();

    // Setup identical base states
    execute_tx(
        &mut state1,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        },
    );
    execute_tx(
        &mut state2,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        },
    );
    execute_tx(
        &mut state1,
        Transaction::Deposit {
            trader: "bob".into(),
            amount: 100_000_000,
        },
    );
    execute_tx(
        &mut state2,
        Transaction::Deposit {
            trader: "bob".into(),
            amount: 100_000_000,
        },
    );

    // Create positions (bob sells, alice buys)
    execute_tx(
        &mut state1,
        Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 10_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        },
    );
    execute_tx(
        &mut state2,
        Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 10_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        },
    );

    execute_tx(
        &mut state1,
        Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 10_000_000,
            order_type: OrderType::Ioc,
            reduce_only: false,
        },
    );
    execute_tx(
        &mut state2,
        Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 10_000_000,
            order_type: OrderType::Ioc,
            reduce_only: false,
        },
    );

    let hash_before = state1.compute_state_hash_full();

    // Add trigger order to state1 only
    execute_tx(
        &mut state1,
        Transaction::PlaceTriggerOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            trigger_type: TriggerType::StopLoss,
            trigger_price: 4_500_000,
            size: 10_000_000,
            limit_price: None,
            cloid: None,
        },
    );

    // Use the legacy application hash explicitly in this focused regression.
    let hash_with_trigger = state1.compute_state_hash_full();
    let hash_without_trigger = state2.compute_state_hash_full();

    assert_ne!(
        hash_with_trigger, hash_without_trigger,
        "Trigger orders should affect state hash"
    );
    assert_ne!(
        hash_before, hash_with_trigger,
        "Adding trigger should change hash"
    );
}

/// Test block execution produces deterministic state hash
#[test]
fn test_block_execution_deterministic_hash() {
    let mut state1 = AppState::new();
    let mut state2 = AppState::new();

    // Create identical blocks
    let txs = vec![
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        },
        Transaction::Deposit {
            trader: "bob".into(),
            amount: 50_000_000,
        },
    ];
    let payload = bincode::serialize(&txs).unwrap();

    let context = ConsensusContext::new(0, [0u8; 32]);
    let block = Block {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view: 1,
        height: 1,
        parent: [0u8; 32],
        payload: payload.clone(),
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000,
        justify: None,
    };

    // Execute on both states
    let hash1 = state1.execute(&block);
    let hash2 = state2.execute(&block);

    assert_eq!(
        hash1, hash2,
        "Identical block execution should produce identical state hashes"
    );
}

/// Test empty block doesn't change state hash (other than timing)
#[test]
fn test_empty_block_minimal_hash_change() {
    let mut state = AppState::new();

    // Setup some base state
    execute_tx(
        &mut state,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );
    let hash_before = state.compute_state_hash();

    // Execute empty block (heartbeat)
    let context = ConsensusContext::new(0, [0u8; 32]);
    let empty_block = Block {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view: 1,
        height: 1,
        parent: [0u8; 32],
        payload: vec![], // Empty payload
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000,
        justify: None,
    };
    state.execute(&empty_block);

    let hash_after = state.compute_state_hash();

    // Hash may change due to timestamp/view updates, but balances are unchanged
    let account = state.account("alice").unwrap();
    assert_eq!(
        account.balance, 100,
        "Empty block shouldn't change balances"
    );
}

/// Test repeated hashing is stable
#[test]
fn test_hash_stability() {
    let mut state = AppState::new();

    execute_tx(
        &mut state,
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        },
    );
    execute_tx(
        &mut state,
        Transaction::Deposit {
            trader: "bob".into(),
            amount: 200,
        },
    );

    // Compute hash multiple times
    let hash1 = state.compute_state_hash();
    let hash2 = state.compute_state_hash();
    let hash3 = state.compute_state_hash();

    assert_eq!(hash1, hash2, "Hash should be stable (call 1 vs 2)");
    assert_eq!(hash2, hash3, "Hash should be stable (call 2 vs 3)");
}
