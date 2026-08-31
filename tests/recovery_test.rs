//! Recovery Integration Test
//!
//! Tests that state can be recovered correctly after a simulated crash/restart
//! by replaying blocks from storage.

use hyperlicked::app::{AppState, ConsensusTransaction, OrderType, Side, Transaction};
use hyperlicked::consensus::AppHook;
#[cfg(feature = "legacy-engine")]
use hyperlicked::consensus::{Engine, MemoryBlockStore};
use hyperlicked::types::{Block, ConsensusConfig, ConsensusContext};

fn test_context() -> ConsensusContext {
    ConsensusConfig::single_node()
        .context()
        .expect("single-node config must have a canonical context")
}

/// Encode local fixture transactions in the explicit dev/system envelope.
/// Production user actions must use `ConsensusTransaction::Signed`; this
/// helper is only for the in-process `AppState::new()` recovery tests.
fn system_payload(transactions: Vec<Transaction>) -> Vec<u8> {
    let entries = transactions
        .into_iter()
        .map(ConsensusTransaction::System)
        .collect::<Vec<_>>();
    bincode::serialize(&entries).expect("system payload should serialize")
}

/// Test that transactions are properly included in block payloads
/// and can be recovered via replay
#[test]
fn test_block_payload_contains_transactions() {
    let mut state = AppState::new();

    // Submit a deposit transaction
    state
        .submit_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        })
        .unwrap();

    // Prepare a canonical payload (the mempool emits explicit System entries
    // for local fixtures).
    let genesis = Block::genesis(test_context());
    let payload = state.prepare_payload(&genesis);

    // Payload should not be empty since we have a pending tx
    assert!(
        !payload.is_empty(),
        "Payload should contain serialized transactions"
    );

    // Deserialize the canonical consensus payload to verify it contains the
    // explicit local System transaction.
    let txs: Vec<ConsensusTransaction> =
        bincode::deserialize(&payload).expect("Should deserialize canonical consensus payload");
    assert_eq!(txs.len(), 1, "Should have one transaction in payload");

    // Verify it's a deposit
    match &txs[0] {
        ConsensusTransaction::System(Transaction::Deposit { trader, amount }) => {
            assert_eq!(trader, "alice");
            assert_eq!(*amount, 100_000_000);
        }
        _ => panic!("Expected Deposit transaction"),
    }
}

/// Test that executing a block with payload applies the transactions
#[test]
fn test_block_execution_applies_transactions() {
    let mut state = AppState::new();

    // Create a block with a deposit payload
    let deposit_tx = Transaction::Deposit {
        trader: "bob".into(),
        amount: 50_000_000,
    };
    let payload = system_payload(vec![deposit_tx]);

    let block = Block {
        epoch: test_context().epoch,
        committee_hash: test_context().committee_hash,
        genesis_hash: test_context().genesis_hash,
        view: 1,
        height: 1,
        parent: [0u8; 32],
        payload,
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000,
        justify: None,
    };

    // Execute the block
    let _hash = state.execute(&block);

    // Verify the deposit was applied
    let account = state.account("bob").expect("Bob should have an account");
    assert_eq!(
        account.balance, 50_000_000,
        "Bob should have deposited funds"
    );
}

/// Test state recovery by replaying blocks
#[test]
fn test_state_recovery_via_block_replay() {
    // Step 1: Build initial state with transactions
    let mut original_state = AppState::new();

    // Execute several transactions through blocks
    let txs_block1 = vec![
        Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        },
        Transaction::Deposit {
            trader: "bob".into(),
            amount: 50_000_000,
        },
    ];

    let block1 = Block {
        epoch: test_context().epoch,
        committee_hash: test_context().committee_hash,
        genesis_hash: test_context().genesis_hash,
        view: 1,
        height: 1,
        parent: [0u8; 32],
        payload: system_payload(txs_block1),
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000,
        justify: None,
    };
    original_state.execute(&block1);

    // Block 2: Place orders
    let txs_block2 = vec![Transaction::PlaceOrder {
        trader: "alice".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Bid,
        price: 5_000_000,
        size: 10_000_000,
        order_type: OrderType::Gtc,
        reduce_only: false,
    }];

    let block2 = Block {
        epoch: test_context().epoch,
        committee_hash: test_context().committee_hash,
        genesis_hash: test_context().genesis_hash,
        view: 2,
        height: 2,
        parent: block1.hash(),
        payload: system_payload(txs_block2),
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 2000,
        justify: None,
    };
    original_state.execute(&block2);

    let original_hash = original_state.compute_state_hash();
    let original_alice_balance = original_state.account("alice").unwrap().balance;
    let original_bob_balance = original_state.account("bob").unwrap().balance;

    // Step 2: Simulate crash - create fresh state
    let mut recovered_state = AppState::new();

    // Step 3: Replay blocks
    recovered_state.execute(&block1);
    recovered_state.execute(&block2);

    // Step 4: Verify recovered state matches original
    let recovered_hash = recovered_state.compute_state_hash();
    assert_eq!(
        original_hash, recovered_hash,
        "State hashes should match after recovery"
    );

    let recovered_alice = recovered_state.account("alice").unwrap();
    let recovered_bob = recovered_state.account("bob").unwrap();

    assert_eq!(recovered_alice.balance, original_alice_balance);
    assert_eq!(recovered_bob.balance, original_bob_balance);
}

/// Test that two-phase mempool commit prevents transaction loss
#[test]
fn test_mempool_two_phase_commit() {
    let mut state = AppState::new();

    // Submit transactions to mempool
    state
        .submit_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        })
        .unwrap();

    state
        .submit_tx(Transaction::Deposit {
            trader: "bob".into(),
            amount: 50_000_000,
        })
        .unwrap();

    // Mempool should have 2 transactions
    let (b0, b1, b2) = state.mempool_stats();
    assert_eq!(b0, 2, "Should have 2 deposit transactions in bucket 0");

    // Create a block with these transactions
    let genesis = Block::genesis(test_context());
    let payload = state.prepare_payload(&genesis);

    let block = Block {
        epoch: test_context().epoch,
        committee_hash: test_context().committee_hash,
        genesis_hash: test_context().genesis_hash,
        view: 1,
        height: 1,
        parent: genesis.hash(),
        payload,
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000,
        justify: None,
    };

    // Execute block (this commits the transactions)
    state.execute(&block);

    // Mempool should be empty after commit
    let (b0, b1, b2) = state.mempool_stats();
    assert_eq!(
        b0 + b1 + b2,
        0,
        "Mempool should be empty after block execution"
    );

    // Verify transactions were applied
    assert!(state.account("alice").is_some());
    assert!(state.account("bob").is_some());
}

/// Test consensus engine produces blocks with transactions
#[test]
#[cfg(feature = "legacy-engine")]
fn test_consensus_engine_includes_transactions() {
    let config = ConsensusConfig::single_node();
    let mut app = AppState::new();

    // Submit a transaction before starting consensus
    app.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 100_000_000,
    })
    .unwrap();

    let store = MemoryBlockStore::new();
    let mut engine = Engine::new(config, app, store);

    // Run a few ticks to produce a block
    let mut committed = None;
    for _ in 0..10 {
        if let Some(block) = engine.tick() {
            committed = Some(block);
            break;
        }
    }

    // Should have committed a block
    let block = committed.expect("Should commit a block");

    // Verify the block has payload (contains our transaction)
    if block.height > 0 {
        // First non-genesis block should have our transaction
        // (or it's in the next block if genesis was just committed)
    }

    // Verify alice got her deposit
    let alice = engine.app().account("alice");
    assert!(
        alice.is_some(),
        "Alice should have an account after block commit"
    );
    assert_eq!(alice.unwrap().balance, 100_000_000);
}
