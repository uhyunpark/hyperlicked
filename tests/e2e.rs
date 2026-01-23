//! End-to-End Tests
//!
//! These tests define the expected behavior of the system.
//! They serve as the living specification.
//!
//! Run with: cargo test --test e2e
//!
//! ## Test Organization
//!
//! - `e2e/` - Domain-specific test modules
//!   - `helpers/` - TestContext, builders, assertions
//!   - `accounts_test.rs` - Deposit, withdraw, account lifecycle
//!   - `orders_test.rs` - Order placement, cancellation, types
//!   - `matching_test.rs` - Price-time priority, partial fills
//!   - `positions_test.rs` - Long/short, PnL, entry prices
//!   - `liquidation_test.rs` - Margin checks, auto-liquidation
//!   - `funding_test.rs` - Funding rate payments
//!   - `triggers_test.rs` - Stop-loss, take-profit
//!   - `staking_test.rs` - Validators, delegation
//!   - `integration_test.rs` - Full flow scenarios

#[path = "e2e_tests/mod.rs"]
mod e2e;

use hyperlicked::app::{AppState, OrderType, Side, Transaction};
use hyperlicked::consensus::{Engine, MemoryBlockStore};
use hyperlicked::types::ConsensusConfig;

// =============================================================================
// Phase 1: Consensus Tests
// =============================================================================

#[test]
fn test_single_node_produces_blocks() {
    // A single node should be able to produce blocks
    let config = ConsensusConfig::single_node();
    let app = AppState::new();
    let store = MemoryBlockStore::new();
    let mut engine = Engine::new(config, app, store);

    // Produce 5 blocks
    let mut blocks = Vec::new();
    for _ in 0..10 {
        if let Some(block) = engine.tick() {
            blocks.push(block);
            if blocks.len() >= 5 {
                break;
            }
        }
    }

    assert!(blocks.len() >= 5, "Should produce at least 5 blocks");
}

#[test]
fn test_blocks_chain_correctly() {
    // Each block should reference its parent
    let config = ConsensusConfig::single_node();
    let app = AppState::new();
    let store = MemoryBlockStore::new();
    let mut engine = Engine::new(config, app, store);

    // Produce blocks
    let mut blocks = Vec::new();
    for _ in 0..20 {
        if let Some(block) = engine.tick() {
            blocks.push(block);
            if blocks.len() >= 5 {
                break;
            }
        }
    }

    // Verify chain
    for i in 1..blocks.len() {
        assert_eq!(
            blocks[i].parent,
            blocks[i - 1].hash(),
            "Block {} should reference block {}",
            i, i - 1
        );
        assert_eq!(
            blocks[i].height,
            blocks[i - 1].height + 1,
            "Heights should increment"
        );
    }
}

#[test]
#[ignore = "requires network setup"]
fn test_block_contains_transactions() {
    // Blocks should contain transactions from mempool
    // This requires a more complex setup with transaction injection
}

// =============================================================================
// Phase 2: Multi-Node Tests
// =============================================================================

#[test]
#[ignore = "requires network setup"]
fn test_three_nodes_reach_consensus() {
    // 3 nodes should agree on block order
    // This test requires running 3 nodes with networking
}

#[test]
#[ignore = "requires timeout handling"]
fn test_view_change_on_timeout() {
    // If leader fails, view should advance
}

// =============================================================================
// Phase 3: App Layer Tests
// =============================================================================

#[test]
fn test_order_matching() {
    // Bid and ask should match at correct price
    let mut state = AppState::new();

    // Deposit for both traders
    state.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 100_000_000, // $1M
    }).unwrap();

    state.submit_tx(Transaction::Deposit {
        trader: "bob".into(),
        amount: 100_000_000,
    }).unwrap();

    // Alice places bid at $50,000
    state.submit_tx(Transaction::PlaceOrder {
        trader: "alice".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Bid,
        price: 5_000_000, // $50,000 in cents
        size: 100_000_000, // 1 BTC in satoshis
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    // Bob places ask at $49,000 (should match)
    state.submit_tx(Transaction::PlaceOrder {
        trader: "bob".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Ask,
        price: 4_900_000,
        size: 100_000_000,
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    // Execute block to process transactions
    let block = hyperlicked::types::Block {
        view: 0,
        height: 1,
        parent: [0u8; 32],
        payload: vec![],
        proposer: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000,
        justify: None,
    };

    use hyperlicked::consensus::AppHook;
    state.execute(&block);

    // Verify positions
    let alice = state.account("alice").expect("Alice account");
    let bob = state.account("bob").expect("Bob account");

    let alice_pos = alice.position("BTC-USDT");
    let bob_pos = bob.position("BTC-USDT");

    // Alice should be long 1 BTC
    assert_eq!(alice_pos.size, 100_000_000, "Alice should be long 1 BTC");

    // Bob should be short 1 BTC
    assert_eq!(bob_pos.size, -100_000_000, "Bob should be short 1 BTC");

    // Trade should have executed at the bid price ($50,000)
    assert_eq!(alice_pos.entry_price, 5_000_000, "Entry at bid price");
    assert_eq!(bob_pos.entry_price, 5_000_000, "Entry at bid price");
}

#[test]
fn test_position_updates() {
    // After fill, positions should update correctly
    let mut state = AppState::new();

    // Setup: Alice deposits and goes long
    state.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 100_000_000,
    }).unwrap();

    state.submit_tx(Transaction::Deposit {
        trader: "bob".into(),
        amount: 100_000_000,
    }).unwrap();

    // Alice bids, Bob hits
    state.submit_tx(Transaction::PlaceOrder {
        trader: "alice".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Bid,
        price: 5_000_000,
        size: 200_000_000, // 2 BTC
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    state.submit_tx(Transaction::PlaceOrder {
        trader: "bob".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Ask,
        price: 5_000_000,
        size: 100_000_000, // 1 BTC (partial)
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    // Execute
    let block = hyperlicked::types::Block {
        view: 0,
        height: 1,
        parent: [0u8; 32],
        payload: vec![],
        proposer: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000,
        justify: None,
    };

    use hyperlicked::consensus::AppHook;
    state.execute(&block);

    // Alice should have 1 BTC (partial fill), with remaining order on book
    let alice_pos = state.account("alice").unwrap().position("BTC-USDT");
    assert_eq!(alice_pos.size, 100_000_000, "Alice should have 1 BTC from partial fill");

    // Check orderbook still has Alice's remaining order
    let book = state.orderbook("BTC-USDT").unwrap();
    let bid_levels = book.bid_levels(10);
    assert_eq!(bid_levels.len(), 1, "Should have one bid level");
    assert_eq!(bid_levels[0].size, 100_000_000, "1 BTC remaining on bid");
}

#[test]
fn test_full_flow() {
    // Order → Block → Fill → Position Update
    // This is the complete flow through consensus

    let config = ConsensusConfig::single_node();
    let mut state = AppState::new();

    // 1. Submit transactions to mempool
    state.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 100_000_000,
    }).unwrap();

    state.submit_tx(Transaction::Deposit {
        trader: "bob".into(),
        amount: 100_000_000,
    }).unwrap();

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

    // 2. Create engine with our state
    let store = MemoryBlockStore::new();
    let mut engine = Engine::new(config, state, store);

    // 3. Produce a block (should execute transactions)
    let mut block = None;
    for _ in 0..10 {
        if let Some(b) = engine.tick() {
            block = Some(b);
            break;
        }
    }

    assert!(block.is_some(), "Should produce at least one block");

    // 4. Verify the app state (positions updated)
    // Note: We'd need access to engine's app state to verify positions
    // For now, this test demonstrates the flow works
}

// =============================================================================
// Mempool Tests
// =============================================================================

#[test]
fn test_mempool_3_bucket_ordering() {
    // Transactions should be ordered: deposits → cancels → orders
    use hyperlicked::app::Mempool;

    let mut mempool = hyperlicked::app::Mempool::default();

    // Add in wrong order
    mempool.add(Transaction::PlaceOrder {
        trader: "alice".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Bid,
        price: 5_000_000,
        size: 100_000_000,
        order_type: OrderType::Gtc,
        reduce_only: false,
    }, 1).unwrap();

    mempool.add(Transaction::CancelOrder {
        trader: "bob".into(),
        order_id: "order1".into(),
    }, 2).unwrap();

    mempool.add(Transaction::Deposit {
        trader: "charlie".into(),
        amount: 1000,
    }, 3).unwrap();

    // Extract
    let txs = mempool.prepare_block(10);

    // Verify order: deposit first, cancel second, order third
    assert!(matches!(txs[0], Transaction::Deposit { .. }));
    assert!(matches!(txs[1], Transaction::CancelOrder { .. }));
    assert!(matches!(txs[2], Transaction::PlaceOrder { .. }));
}
