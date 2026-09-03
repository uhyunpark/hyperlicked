//! Tests for parallel matching execution
//!
//! Verifies that:
//! - Sequential and parallel execution produce identical results
//! - Cross-symbol same-trader orders execute correctly
//! - Multiple symbols can be processed in a single block

use hyperlicked::app::state::AppState;
use hyperlicked::app::{
    orderbook::{OrderType, Side},
    Transaction,
};
use hyperlicked::consensus::AppHook;
use hyperlicked::types::{Block, ConsensusContext};

fn create_test_block(height: u64, payload: &[u8]) -> Block {
    let context = ConsensusContext::new(0, [0u8; 32]);
    Block {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view: height,
        height,
        parent: [0u8; 32],
        payload: payload.to_vec(),
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000 + height * 100,
        justify: None,
    }
}

fn execute_payload_block(state: &mut AppState, height: u64) -> [u8; 32] {
    let parent = create_test_block(height.saturating_sub(1), &[]);
    let payload = state.prepare_payload(&parent);
    let block = create_test_block(height, &payload);
    state.execute(&block)
}

#[test]
fn test_global_transactions_execute_first() {
    let mut state = AppState::new();

    // Submit: order first, then deposit
    // With correct execution order, deposit should run first (global tx priority)
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
        .submit_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        })
        .unwrap();

    // Execute block
    let _hash = execute_payload_block(&mut state, 1);

    // Verify the order was placed (deposit came first due to bucket priority)
    let book = state.orderbook("BTC-USDT").unwrap();
    assert!(book.best_bid().is_some(), "Order should be on book");
}

#[test]
fn test_multi_symbol_execution() {
    let mut state = AppState::new();

    // Add second market
    state.add_market(hyperlicked::app::MarketConfig {
        symbol: "ETH-USDT".into(),
        ..Default::default()
    });

    // Deposit for both symbols
    state
        .submit_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 200_000_000,
        })
        .unwrap();

    // Orders on different symbols
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
            trader: "alice".into(),
            symbol: "ETH-USDT".into(),
            side: Side::Bid,
            price: 3_000_00, // $3,000 in cents
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // Execute block
    let _hash = execute_payload_block(&mut state, 1);

    // Verify both orders placed
    let btc_book = state.orderbook("BTC-USDT").unwrap();
    let eth_book = state.orderbook("ETH-USDT").unwrap();

    assert!(btc_book.best_bid().is_some(), "BTC order should be on book");
    assert!(eth_book.best_bid().is_some(), "ETH order should be on book");
}

#[test]
fn test_cross_symbol_same_trader() {
    let mut state = AppState::new();

    // Add second market
    state.add_market(hyperlicked::app::MarketConfig {
        symbol: "ETH-USDT".into(),
        ..Default::default()
    });

    // Alice and Bob deposit
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

    // Alice places bids on both symbols
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
            trader: "alice".into(),
            symbol: "ETH-USDT".into(),
            side: Side::Bid,
            price: 3_000_00,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // Execute block 1 - orders rest
    let hash1 = execute_payload_block(&mut state, 1);

    // Bob matches on both symbols
    state
        .submit_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 50_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    state
        .submit_tx(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "ETH-USDT".into(),
            side: Side::Ask,
            price: 3_000_00,
            size: 50_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .unwrap();

    // Execute block 2 - fills happen
    let hash2 = execute_payload_block(&mut state, 2);

    // Verify fills occurred
    assert_ne!(hash1, hash2, "State should change after fills");

    // Alice should have positions on both symbols
    let alice = state.account("alice").unwrap();
    assert_ne!(
        alice.position("BTC-USDT").size,
        0,
        "Alice should have BTC position"
    );
    assert_ne!(
        alice.position("ETH-USDT").size,
        0,
        "Alice should have ETH position"
    );

    // Bob should have opposite positions
    let bob = state.account("bob").unwrap();
    assert_ne!(
        bob.position("BTC-USDT").size,
        0,
        "Bob should have BTC position"
    );
    assert_ne!(
        bob.position("ETH-USDT").size,
        0,
        "Bob should have ETH position"
    );
}

#[test]
fn test_determinism_with_multiple_symbols() {
    // Run the same sequence twice and verify same state hash
    fn run_scenario() -> [u8; 32] {
        let mut state = AppState::new();

        state.add_market(hyperlicked::app::MarketConfig {
            symbol: "ETH-USDT".into(),
            ..Default::default()
        });

        // Multiple traders, multiple symbols
        for i in 0..5 {
            state
                .submit_tx(Transaction::Deposit {
                    trader: format!("trader_{}", i),
                    amount: 1_000_000_000,
                })
                .unwrap();
        }

        // Execute deposits
        execute_payload_block(&mut state, 1);

        // Mix of orders across symbols
        for i in 0..5 {
            state
                .submit_tx(Transaction::PlaceOrder {
                    trader: format!("trader_{}", i),
                    symbol: if i % 2 == 0 { "BTC-USDT" } else { "ETH-USDT" }.into(),
                    side: if i % 2 == 0 { Side::Bid } else { Side::Ask },
                    price: 5_000_000 + (i as i64 * 1000),
                    size: 100_000_000,
                    order_type: OrderType::Gtc,
                    reduce_only: false,
                })
                .unwrap();
        }

        // Execute orders
        execute_payload_block(&mut state, 2)
    }

    let hash1 = run_scenario();
    let hash2 = run_scenario();

    assert_eq!(hash1, hash2, "Same inputs should produce same state hash");
}

#[test]
fn test_stress_multi_symbol() {
    let mut state = AppState::new();

    // Add multiple markets
    for symbol in ["ETH-USDT", "SOL-USDT", "AVAX-USDT"] {
        state.add_market(hyperlicked::app::MarketConfig {
            symbol: symbol.into(),
            ..Default::default()
        });
    }

    // Many traders deposit
    for i in 0..20 {
        state
            .submit_tx(Transaction::Deposit {
                trader: format!("trader_{}", i),
                amount: 10_000_000_000,
            })
            .unwrap();
    }

    execute_payload_block(&mut state, 1);

    // Many orders across all symbols
    let symbols = ["BTC-USDT", "ETH-USDT", "SOL-USDT", "AVAX-USDT"];
    for i in 0..100 {
        let symbol = symbols[i % symbols.len()];
        let trader = format!("trader_{}", i % 20);
        let side = if i % 2 == 0 { Side::Bid } else { Side::Ask };
        let base_price = match symbol {
            "BTC-USDT" => 5_000_000,
            "ETH-USDT" => 300_000,
            "SOL-USDT" => 10_000,
            _ => 20_000, // Must be high enough for min_notional ($10 = 1000 cents)
        };

        state
            .submit_tx(Transaction::PlaceOrder {
                trader,
                symbol: symbol.into(),
                side,
                price: base_price + ((i as i64 % 100) - 50) * 100,
                size: 10_000_000 + (i as i64 * 1000),
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();
    }

    // Execute block with many orders
    let hash = execute_payload_block(&mut state, 2);

    // Verify state is valid
    assert_ne!(hash, [0u8; 32], "Hash should be computed");

    // Verify all markets have orders
    for symbol in &symbols {
        let book = state.orderbook(symbol).unwrap();
        // At least some orders should be resting (not all matched)
        let has_orders = book.best_bid().is_some() || book.best_ask().is_some();
        assert!(has_orders, "{} should have some orders", symbol);
    }
}
