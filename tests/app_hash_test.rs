//! Tests for the canonical application hash
//!
//! Verifies that:
//! - State hashes are consistent (same inputs = same outputs)
//! - Hashes change when state changes
//! - Performance with many accounts
//!
//! The application hash must use one canonical algorithm in every build. The
//! schema-v5 component-tree root is the authenticated block commitment.

use hyperlicked::api::{CanonicalAppHook, SharedState};
use hyperlicked::app::state::AppState;
use hyperlicked::app::{
    orderbook::{OrderType, Side},
    ConsensusTransaction, Transaction,
};
use hyperlicked::consensus::AppHook;
use hyperlicked::types::{Block, ConsensusContext};

fn create_test_block(state: &AppState, height: u64) -> Block {
    let context = ConsensusContext::new(0, [0u8; 32]);
    let mut block = Block {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view: height,
        height,
        parent: [0u8; 32],
        payload: vec![],
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000 + height * 100,
        justify: None,
    };
    block.payload = state.prepare_payload(&block);
    block
}

fn canonical_test_block(
    context: ConsensusContext,
    parent: &Block,
    height: u64,
    timestamp: u64,
    payload: Vec<u8>,
) -> Block {
    Block {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view: height,
        height,
        parent: parent.hash(),
        payload,
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp,
        justify: None,
    }
}

fn canonical_test_payload(transaction: Transaction) -> Vec<u8> {
    bincode::serialize(&vec![ConsensusTransaction::System(transaction)])
        .expect("test transaction payload is serializable")
}

#[test]
fn test_hash_consistency() {
    let mut state = AppState::new();

    // Setup: multiple traders with deposits and orders
    for i in 0..10 {
        state
            .submit_tx(Transaction::Deposit {
                trader: format!("trader_{}", i),
                amount: 10_000_000_000,
            })
            .unwrap();
    }

    // Execute deposits
    let block = create_test_block(&state, 1);
    let hash1 = state.execute(&block);

    // Verify the hash is not zero (was actually computed)
    assert_ne!(hash1, [0u8; 32], "Hash should be computed");

    // Add some orders
    for i in 0..5 {
        state
            .submit_tx(Transaction::PlaceOrder {
                trader: format!("trader_{}", i),
                symbol: "BTC-USDT".into(),
                side: if i % 2 == 0 { Side::Bid } else { Side::Ask },
                price: 5_000_000 + (i as i64 * 1000),
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();
    }

    // Execute orders
    let block = create_test_block(&state, 2);
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
        state
            .submit_tx(Transaction::Deposit {
                trader: format!("trader_{}", i),
                amount: 10_000_000 + i as i64,
            })
            .unwrap();
    }

    // Execute block
    let block = create_test_block(&state, 1);
    let hash1 = state.execute(&block);

    // Verify hash is computed
    assert_ne!(
        hash1, [0u8; 32],
        "Hash should be computed with {} accounts",
        num_accounts
    );

    // Modify only a few accounts
    for i in 0..5 {
        state
            .submit_tx(Transaction::Deposit {
                trader: format!("trader_{}", i),
                amount: 1000,
            })
            .unwrap();
    }

    // Execute modification block
    let block = create_test_block(&state, 2);
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
            state
                .submit_tx(Transaction::Deposit {
                    trader: format!("trader_{}", i),
                    amount: 1_000_000 * (i as i64 + 1),
                })
                .unwrap();
        }

        let block = create_test_block(&state, 1);
        state.execute(&block)
    }

    let hash1 = run_scenario();
    let hash2 = run_scenario();

    assert_eq!(hash1, hash2, "Same operations should produce same hash");
}

#[test]
fn app_hash_uses_authenticated_schema_v5_root() {
    let mut state = AppState::new();
    state
        .submit_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 1_000_000,
        })
        .unwrap();

    let block = create_test_block(&state, 1);
    let app_hash = state.execute(&block);

    assert_eq!(app_hash, state.compute_state_hash());
    assert_eq!(app_hash, state.compute_full_state_root());
    assert_ne!(app_hash, state.compute_state_hash_full());
}

#[test]
fn canonical_dirty_candidate_seal_matches_fresh_root_and_replay_hits_candidate() {
    let context = ConsensusContext::new(0, [0u8; 32]);
    let genesis = Block::genesis(context);
    let shared = SharedState::new(AppState::new_with_chain_domain_and_dev(
        context.genesis_hash,
        true,
    ));
    let mut hook = CanonicalAppHook::new(shared);

    let mut base = canonical_test_block(
        context,
        &genesis,
        1,
        1_001,
        canonical_test_payload(Transaction::Deposit {
            trader: "base-trader".to_string(),
            amount: 1_000_000,
        }),
    );
    base.app_hash = hook.execute(&base);

    let mut child = canonical_test_block(
        context,
        &base,
        2,
        1_002,
        canonical_test_payload(Transaction::Deposit {
            trader: "child-trader".to_string(),
            amount: 2_000_000,
        }),
    );
    child.app_hash = hook.execute(&child);

    let mut fresh = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
    fresh.execute(&base);
    fresh.execute(&child);
    let expected_root = fresh.compute_full_state_root();

    // preflight independently recomputes the fresh tree and rejects a
    // candidate if the parent-tree dirty derivation was incorrect.
    assert_eq!(
        hook.preflight_state_root(&child),
        Ok(Some(expected_root)),
        "dirty candidate tree must equal an independent fresh root"
    );
    assert_eq!(hook.candidate_count(), 2);

    // A finalized duplicate takes the exact-candidate hit branch and must not
    // create another speculative candidate.
    assert_eq!(hook.execute(&child), child.app_hash);
    assert_eq!(hook.candidate_count(), 2);
}

#[test]
fn test_trade_changes_hash() {
    let mut state = AppState::new();

    // Setup initial state
    state
        .submit_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        })
        .unwrap();

    state
        .submit_tx(Transaction::Deposit {
            trader: "bob".into(),
            amount: 100_000_000,
        })
        .unwrap();

    let block = create_test_block(&state, 1);
    let hash1 = state.execute(&block);

    // Create a trade (modifies mark price / orderbook state)
    state
        .submit_tx(Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    state
        .submit_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    let block = create_test_block(&state, 2);
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
    state
        .submit_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 500_000_000,
        })
        .unwrap();

    state
        .submit_tx(Transaction::Deposit {
            trader: "bob".into(),
            amount: 500_000_000,
        })
        .unwrap();

    let block = create_test_block(&state, 1);
    let hash_initial = state.execute(&block);

    // Place orders that match (creates a fill, changes account state)
    state
        .submit_tx(Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    state
        .submit_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    let block = create_test_block(&state, 2);
    let hash_btc = state.execute(&block);

    // Verify hash changed from initial
    assert_ne!(hash_initial, hash_btc, "Hash should change after BTC trade");

    // Trade on ETH market
    state
        .submit_tx(Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "ETH-USDT".into(),
            side: Side::Bid,
            price: 300_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    state
        .submit_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "ETH-USDT".into(),
            side: Side::Ask,
            price: 300_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    let block = create_test_block(&state, 3);
    let hash_eth = state.execute(&block);

    // Verify hashes are different (positions changed)
    assert_ne!(hash_btc, hash_eth, "Hashes should differ after ETH trade");
}
