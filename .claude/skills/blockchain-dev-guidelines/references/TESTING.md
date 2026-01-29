# Testing Patterns

Reference for integration tests and determinism verification.

## Table of Contents

- [Test Organization](#test-organization)
- [Single-Node Tests](#single-node-tests)
- [AppState Tests](#appstate-tests)
- [Determinism Verification](#determinism-verification)
- [Test Utilities](#test-utilities)

---

## Test Organization

```
tests/
├── e2e.rs            # End-to-end consensus tests
├── orderbook.rs      # Matching engine tests
├── accounts.rs       # Position/margin tests
├── crypto.rs         # Signature tests
└── common/
    └── mod.rs        # Shared test utilities
```

---

## Single-Node Tests

### Basic Block Production

```rust
#[test]
fn test_single_node_produces_blocks() {
    let config = ConsensusConfig::single_node();
    let app = AppState::new();
    let store = MemoryBlockStore::new();
    let mut engine = Engine::new(config, app, store);

    let mut blocks = Vec::new();
    for _ in 0..100 {
        if let Some(block) = engine.tick() {
            blocks.push(block);
            if blocks.len() >= 5 {
                break;
            }
        }
    }

    assert!(blocks.len() >= 5, "Should produce at least 5 blocks");

    // Verify chain structure
    for i in 1..blocks.len() {
        assert_eq!(blocks[i].parent, blocks[i-1].hash());
        assert_eq!(blocks[i].height, blocks[i-1].height + 1);
    }
}
```

### Block Execution

```rust
#[test]
fn test_block_execution_updates_state() {
    let config = ConsensusConfig::single_node();
    let mut app = AppState::new();
    let store = MemoryBlockStore::new();
    let mut engine = Engine::new(config, app, store);

    // Submit a deposit
    engine.app.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 100_000_000,  // $1,000,000
    }).unwrap();

    // Tick to execute
    let block = loop {
        if let Some(b) = engine.tick() {
            break b;
        }
    };

    // Verify state updated
    let account = engine.app.get_account("alice").unwrap();
    assert_eq!(account.balance, 100_000_000);
}
```

---

## AppState Tests

### Order Matching

```rust
#[test]
fn test_order_matching() {
    let mut state = AppState::new();

    // Setup: Deposit funds
    state.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 100_000_000,
    }).unwrap();
    state.submit_tx(Transaction::Deposit {
        trader: "bob".into(),
        amount: 100_000_000,
    }).unwrap();

    // Execute deposits
    state.execute_pending();

    // Alice places bid
    state.submit_tx(Transaction::PlaceOrder {
        trader: "alice".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Bid,
        price: 5_000_000,       // $50,000
        size: 100_000_000,      // 1 BTC
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    // Bob places ask that crosses
    state.submit_tx(Transaction::PlaceOrder {
        trader: "bob".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Ask,
        price: 5_000_000,       // $50,000
        size: 100_000_000,      // 1 BTC
        order_type: OrderType::Gtc,
        reduce_only: false,
    }).unwrap();

    // Execute orders
    state.execute_pending();

    // Verify fill occurred
    assert_eq!(state.pending_fills.len(), 1);

    let fill = &state.pending_fills[0];
    assert_eq!(fill.price, 5_000_000);
    assert_eq!(fill.size, 100_000_000);
}
```

### Margin Requirements

```rust
#[test]
fn test_insufficient_margin_rejected() {
    let mut state = AppState::new();

    // Deposit small amount
    state.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 1_000_000,  // $10,000
    }).unwrap();
    state.execute_pending();

    // Try to place large order (requires more margin)
    let result = state.submit_tx(Transaction::PlaceOrder {
        trader: "alice".into(),
        symbol: "BTC-USDT".into(),
        side: Side::Bid,
        price: 5_000_000,       // $50,000
        size: 1_000_000_000,    // 10 BTC = $500,000 notional
        order_type: OrderType::Gtc,
        reduce_only: false,
    });

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AppError::InsufficientMargin));
}
```

---

## Determinism Verification

### Same Input = Same Output

```rust
#[test]
fn test_execution_determinism() {
    let transactions = vec![
        Transaction::Deposit { trader: "alice".into(), amount: 100_000_000 },
        Transaction::Deposit { trader: "bob".into(), amount: 100_000_000 },
        Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        },
        Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 5_000_000,
            size: 100_000_000,
            order_type: OrderType::Gtc,
            reduce_only: false,
        },
    ];

    // Execute twice
    let hash1 = execute_and_hash(&transactions);
    let hash2 = execute_and_hash(&transactions);

    // Must produce identical state hash
    assert_eq!(hash1, hash2, "Execution must be deterministic");
}

fn execute_and_hash(transactions: &[Transaction]) -> Hash {
    let mut state = AppState::new();

    for tx in transactions {
        state.submit_tx(tx.clone()).unwrap();
    }

    let block = Block {
        view: 1,
        height: 1,
        parent: [0u8; 32],
        payload: state.prepare_payload(&Block::genesis()),
        proposer: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 0,
    };

    state.execute(&block)
}
```

### Hash Stability

```rust
#[test]
fn test_block_hash_stability() {
    let block = Block {
        view: 1,
        height: 1,
        parent: [1u8; 32],
        payload: vec![1, 2, 3],
        proposer: [2u8; 32],
        app_hash: [3u8; 32],
        timestamp: 12345,
    };

    let hash1 = block.hash();
    let hash2 = block.hash();

    assert_eq!(hash1, hash2);

    // Hash should be reproducible with same fields
    let block_copy = block.clone();
    assert_eq!(block.hash(), block_copy.hash());
}
```

---

## Test Utilities

### Test AppState

```rust
// tests/common/mod.rs

pub fn setup_test_state() -> AppState {
    let mut state = AppState::new();

    // Add default market config
    state.add_market(MarketConfig::default());

    // Fund test accounts
    state.submit_tx(Transaction::Deposit {
        trader: "alice".into(),
        amount: 1_000_000_000,  // $10M
    }).unwrap();
    state.submit_tx(Transaction::Deposit {
        trader: "bob".into(),
        amount: 1_000_000_000,
    }).unwrap();

    state.execute_pending();
    state
}
```

### Assert Helpers

```rust
pub fn assert_position(state: &AppState, trader: &str, symbol: &str, expected_size: Size) {
    let account = state.get_account(trader).expect("account exists");
    let position = account.positions.get(symbol);

    match (position, expected_size) {
        (None, 0) => {}  // No position, expected no position
        (Some(pos), size) => assert_eq!(pos.size, size),
        (None, size) => panic!("expected position size {} but no position", size),
    }
}

pub fn assert_balance(state: &AppState, trader: &str, expected: i64) {
    let account = state.get_account(trader).expect("account exists");
    assert_eq!(account.balance, expected);
}
```

---

## Running Tests

```bash
# Run all tests
cargo test

# Run specific test file
cargo test --test e2e

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_order_matching
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [TYPES.md](TYPES.md) - Test data types
- [ORDERBOOK.md](ORDERBOOK.md) - Matching logic to test
