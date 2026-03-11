---
name: blockchain-dev-guidelines
description: Rust blockchain development patterns for hyperlicked. Covers HotStuff-2 consensus, orderbook matching engine, BLS signatures, integer math, AppHook trait, BlockStore trait, mempool ordering, Position/Account types, error handling with thiserror, and module organization. Activates when working on consensus, blocks, votes, certificates, matching, liquidation, funding, pacemaker, safety, perpetual futures, perpdex, or any src/*.rs files.
---

# Blockchain Development Guidelines

## Purpose

Comprehensive patterns and conventions for the hyperlicked blockchain codebase. Inspired by Hyperliquid; implements HotStuff-2 BFT consensus with a heap-based orderbook matching engine.

## Project Structure

```
src/
├── lib.rs              # Crate root, module declarations
├── types.rs            # Core types (Block, Vote, Order, ~571 LOC)
├── config.rs           # Runtime configuration from env vars
├── consensus/          # HotStuff-2 BFT implementation
│   ├── mod.rs          # Traits: AppHook, BlockStore
│   ├── engine.rs       # Main consensus loop (leader/follower)
│   ├── pacemaker.rs    # View advancement & timeouts
│   ├── safety.rs       # Voting safety rules
│   └── aggregator.rs   # BLS signature aggregation
├── app/                # Business logic layer
│   ├── mod.rs          # Transaction types, MarketConfig
│   ├── state/          # AppState (implements AppHook)
│   │   ├── mod.rs      # Struct definition + accessors
│   │   ├── execution.rs # Transaction handlers
│   │   └── consensus.rs # AppHook impl + state hash
│   ├── orderbook/      # Heap-based matching engine
│   │   ├── mod.rs      # OrderBook struct + basic ops
│   │   └── matching.rs # place() + matching logic
│   ├── accounts.rs     # Account, AccountManager
│   ├── positions.rs    # Position struct + PnL/funding
│   ├── mempool.rs      # 3-bucket transaction ordering
│   ├── candles.rs      # OHLCV aggregation
│   ├── funding.rs      # Funding rate payments
│   ├── liquidation.rs  # Liquidation engine
│   └── staking/        # Validator staking (NEW)
│       ├── mod.rs      # Re-exports
│       ├── types.rs    # Validator, Delegation, etc.
│       ├── state.rs    # StakingState
│       ├── epoch.rs    # Epoch transitions
│       ├── rewards.rs  # Reward distribution
│       ├── slashing.rs # Slashing logic
│       ├── jailing.rs  # Jailing/unjailing
│       └── transactions.rs # Staking tx types
├── crypto/             # Cryptographic operations
│   ├── bls.rs          # BLS12-381 signatures
│   ├── eip712.rs       # EIP-712 typed data signing
│   └── agent.rs        # Agent key delegation
├── api/                # REST + WebSocket
│   ├── routes.rs       # Axum handlers
│   └── websocket.rs    # Real-time subscriptions
├── network/            # TCP transport
└── storage/            # RocksDB persistence
```

---

## Core Principles

### 1. Integer Math Only

**CRITICAL**: No floats for cross-validator determinism.

```rust
pub type Price = i64;  // Cents (1 USD = 100)
pub type Size = i64;   // Satoshis (1 unit = 100_000_000)

// PnL calculation
let pnl = (size * price_diff) / 100_000_000;
```

### 2. Key Traits

```rust
/// Consensus calls this to execute blocks
pub trait AppHook: Send + Sync {
    fn prepare_payload(&self, parent: &Block) -> Vec<u8>;
    fn execute(&mut self, block: &Block) -> Hash;
}

/// Swappable storage
pub trait BlockStore: Send + Sync {
    fn save(&self, block: &Block);
    fn get(&self, hash: &Hash) -> Option<Block>;
    fn set_committed(&self, hash: &Hash);
}
```

### 3. Generic Engine

```rust
pub struct Engine<A, S>
where
    A: AppHook,
    S: BlockStore,
{ ... }
```

---

## Key Patterns

### Block & Vote Structure

```rust
pub struct Block {
    pub view: View,           // Consensus round
    pub height: Height,       // Block number
    pub parent: Hash,         // Parent hash
    pub payload: Vec<u8>,     // Serialized transactions
    pub proposer: NodeId,     // Leader
    pub app_hash: Hash,       // State root AFTER execution
    pub timestamp: u64,
}

pub struct Vote {
    pub view: View,
    pub block_hash: Hash,
    pub app_hash: Hash,       // For Byzantine detection
    pub voter: NodeId,
    pub signature: Signature,
}
```

**Critical**: BlockHash does NOT include AppHash (execution happens after proposal).

### Consensus Tick Pattern

```rust
impl<A, S> Engine<A, S> {
    pub fn tick(&mut self) -> Option<Block> {
        let view = self.pacemaker.current_view();
        if self.config.is_leader(view) {
            self.run_leader(view)
        } else {
            self.run_follower(view)
        }
    }
}
```

### 3-Bucket Mempool

```rust
impl Transaction {
    pub fn bucket(&self) -> u8 {
        match self {
            Transaction::Deposit { .. } | Transaction::Withdraw { .. } => 0,
            Transaction::CancelOrder { .. } => 1,
            Transaction::PlaceOrder { .. } => 2,
        }
    }
}
```

Priority: Deposits/Withdraws → Cancels → Orders

### Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum OrderBookError {
    #[error("invalid price")]
    InvalidPrice,
    #[error("price not aligned to tick size")]
    PriceNotAligned,
    #[error("ALO order would match immediately")]
    AloWouldMatch,
}
```

Each module defines its own error enum. Use `#[from]` for automatic conversion.

---

## Quick Reference

### Type Aliases
```rust
pub type View = u64;
pub type Height = u64;
pub type Hash = [u8; 32];
pub type NodeId = [u8; 32];
pub type Price = i64;   // cents
pub type Size = i64;    // satoshis
```

### File Size
- Max 500 LOC per file
- Split complex modules into multiple files

### Documentation
- Module-level docs explain high-level concepts
- Type docs explain invariants
- Cross-reference to `docs/blockchain/` for deep dives

---

## Reference Files

For detailed information on specific topics, see:

### [CONSENSUS.md](references/CONSENSUS.md)
HotStuff-2 implementation details:
- Engine tick loop
- Leader/follower logic
- Block/Vote/Certificate structures
- 2-chain commit rule

### [ORDERBOOK.md](references/ORDERBOOK.md)
Matching engine patterns:
- Heap-based order storage
- FIFO within price levels
- Fill/Order types
- ALO, IOC, GTC handling

### [TYPES.md](references/TYPES.md)
Core type definitions:
- Integer math patterns
- Position/Account structures
- MarketConfig
- Type aliases

### [CRYPTO.md](references/CRYPTO.md)
Cryptographic operations:
- BLS12-381 signatures
- Signature aggregation
- EIP-712 signing
- Agent key delegation

### [TESTING.md](references/TESTING.md)
Testing patterns:
- Integration tests with AppState
- Single-node engine tests
- Determinism verification

### [PATTERNS.md](references/PATTERNS.md)
Code organization:
- Module structure
- Error enum patterns
- Serialization conventions
- 500 LOC guideline

---

## Common Pitfalls

1. **Using floats** - Causes non-determinism across validators
2. **Including AppHash in BlockHash** - Creates circular dependency
3. **Blocking on I/O in tick()** - Must return quickly
4. **Not validating orders** - Check price/size alignment
5. **Missing reduce_only checks** - Can't open position with reduce_only=true
6. **Forgetting 3-bucket priority** - Deposits must execute before orders

---

## Related Documentation

- `docs/blockchain/consensus.md` - HotStuff-2 protocol spec
- `docs/blockchain/orderbook.md` - Matching engine design
- `docs/blockchain/ROADMAP.md` - Current priorities
- `CLAUDE.md` - Project overview

---

**Line Count**: < 500 (following 500-line rule)
**Progressive Disclosure**: Reference files for detailed information
