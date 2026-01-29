# Code Organization Patterns

Reference for module structure, error handling, and coding conventions.

## Table of Contents

- [Module Organization](#module-organization)
- [Error Handling](#error-handling)
- [Trait Patterns](#trait-patterns)
- [500 LOC Guideline](#500-loc-guideline)
- [Documentation](#documentation)

---

## Module Organization

### mod.rs Pattern

```rust
// src/consensus/mod.rs

//! HotStuff-2 Consensus Implementation
//!
//! This module implements the HotStuff-2 BFT consensus protocol.

// Traits first
pub trait AppHook: Send + Sync {
    fn prepare_payload(&self, parent: &Block) -> Vec<u8>;
    fn execute(&mut self, block: &Block) -> Hash;
}

pub trait BlockStore: Send + Sync {
    fn save(&self, block: &Block);
    fn get(&self, hash: &Hash) -> Option<Block>;
    fn set_committed(&self, hash: &Hash);
}

// Default implementations
pub struct MemoryBlockStore { ... }
impl BlockStore for MemoryBlockStore { ... }

pub struct NoOpApp;
impl AppHook for NoOpApp { ... }

// Module declarations
mod engine;
mod pacemaker;
mod safety;
mod aggregator;
mod runner;

// Re-exports
pub use engine::Engine;
pub use pacemaker::Pacemaker;
pub use safety::Safety;
pub use aggregator::VoteAggregator;
pub use runner::ConsensusRunner;
```

### File Structure

```
src/
├── lib.rs              # Crate root, module declarations
├── types.rs            # Shared types (Block, Vote, etc.)
├── config.rs           # Runtime configuration
├── consensus/
│   ├── mod.rs          # Traits + re-exports
│   ├── engine.rs       # Engine implementation
│   ├── pacemaker.rs    # Pacemaker implementation
│   ├── safety.rs       # Safety rules
│   └── aggregator.rs   # BLS aggregation
├── app/
│   ├── mod.rs          # Transaction types
│   ├── state.rs        # AppState
│   ├── orderbook.rs    # Matching engine
│   └── accounts.rs     # Account management
└── ...
```

---

## Error Handling

### thiserror Pattern

```rust
// Each module defines its own error enum
#[derive(Debug, thiserror::Error)]
pub enum OrderBookError {
    #[error("invalid price")]
    InvalidPrice,

    #[error("price not aligned to tick size")]
    PriceNotAligned,

    #[error("invalid size")]
    InvalidSize,

    #[error("size not aligned to lot size")]
    SizeNotAligned,

    #[error("ALO order would match immediately")]
    AloWouldMatch,

    #[error("order not found")]
    OrderNotFound,
}
```

### Error Conversion

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("mempool error: {0}")]
    Mempool(String),

    #[error("account error: {0}")]
    Account(#[from] AccountError),  // Auto-convert

    #[error("orderbook error: {0}")]
    OrderBook(#[from] OrderBookError),  // Auto-convert

    #[error("market not found")]
    MarketNotFound,

    #[error("insufficient margin")]
    InsufficientMargin,
}
```

### Usage

```rust
fn place_order(&mut self, order: Order) -> Result<Vec<Fill>, AppError> {
    // OrderBookError auto-converts to AppError via #[from]
    let fills = self.orderbook.place(order)?;

    // Explicit conversion
    self.accounts.apply_fills(&fills)
        .map_err(AppError::Account)?;

    Ok(fills)
}
```

### Network/API Errors

```rust
// At network/API boundary, use anyhow for flexibility
use anyhow::Result;

pub async fn handle_order(req: OrderRequest) -> Result<Response> {
    let order = parse_order(&req)?;
    let fills = state.place_order(order)?;
    Ok(Response::new(fills))
}
```

---

## Trait Patterns

### Generic Bounds

```rust
pub struct Engine<A, S>
where
    A: AppHook,
    S: BlockStore,
{
    app: A,
    store: S,
    // ...
}
```

### Trait Object Alternatives

```rust
// Static dispatch (preferred when type is known at compile time)
fn process<S: BlockStore>(store: &S) { ... }

// Dynamic dispatch (when runtime polymorphism needed)
fn process(store: &dyn BlockStore) { ... }

// Boxed (when ownership needed)
fn process(store: Box<dyn BlockStore>) { ... }
```

### Async Traits

```rust
use async_trait::async_trait;

#[async_trait]
pub trait Network: Send + Sync {
    async fn broadcast_propose(&self, propose: Propose) -> Result<()>;
    async fn send_vote(&self, to: NodeId, vote: Vote) -> Result<()>;
    async fn recv_msg(&self) -> Result<(NodeId, Message)>;
}
```

---

## 500 LOC Guideline

### When to Split

| LOC Range | Action |
|-----------|--------|
| < 200 | Single file is fine |
| 200-400 | Consider splitting if distinct concerns |
| 400-500 | Look for natural split points |
| > 500 | Must split |

### Split Strategies

**By functionality:**
```
orderbook.rs (500 LOC)
  → orderbook/mod.rs       (types, public API)
  → orderbook/matching.rs  (matching algorithm)
  → orderbook/validation.rs (order validation)
```

**By type:**
```
types.rs (600 LOC)
  → types/consensus.rs  (Block, Vote, Certificate)
  → types/trading.rs    (Order, Fill, Position)
  → types/mod.rs        (re-exports)
```

### Exceptions

Some files may exceed 500 LOC when:
- Multiple responsibilities must stay together
- Splitting would harm readability
- Complex state that can't be factored out

Document why in a comment:
```rust
// NOTE: This file exceeds 500 LOC because AppState must maintain
// coherent state across orderbooks, accounts, and mempool.
// Splitting would require excessive cross-module references.
```

---

## Documentation

### Module Docs

```rust
//! HotStuff-2 Consensus Engine
//!
//! This module implements the main consensus loop.
//!
//! ## Overview
//!
//! The engine runs a tick-based loop that:
//! 1. Determines if we're leader or follower
//! 2. Proposes or votes accordingly
//! 3. Processes QCs and commits blocks
//!
//! ## Usage
//!
//! ```rust
//! let engine = Engine::new(config, app, store);
//! loop {
//!     if let Some(committed) = engine.tick() {
//!         // Handle committed block
//!     }
//! }
//! ```
```

### Type Docs

```rust
/// A block in the chain.
///
/// Blocks form a chain via `parent` hash. Each block has:
/// - `view`: The consensus round it was proposed in
/// - `height`: Position in committed chain (0 = genesis)
/// - `payload`: Serialized transactions
/// - `app_hash`: State root after executing this block
///
/// # Invariants
///
/// - `height` must equal parent's height + 1
/// - `parent` must be a valid block hash
/// - `app_hash` is computed AFTER execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block { ... }
```

### Function Docs

```rust
/// Place an order in the orderbook.
///
/// # Arguments
///
/// * `order` - The order to place
/// * `config` - Market configuration for validation
///
/// # Returns
///
/// Vector of fills if order matched, empty if rested on book.
///
/// # Errors
///
/// - `InvalidPrice` if price <= 0
/// - `PriceNotAligned` if price % tick_size != 0
/// - `AloWouldMatch` if ALO order would cross
pub fn place(&mut self, order: Order, config: &MarketConfig)
    -> Result<Vec<Fill>, OrderBookError>
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [CONSENSUS.md](CONSENSUS.md) - Consensus patterns
- [TYPES.md](TYPES.md) - Type definitions
