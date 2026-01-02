# Hyperliquid Clone - Rust Implementation

## ROLE
You are building a Hyperliquid clone in Rust. This is an AI-native codebase: designed for AI-assisted development from the ground up.

## PROJECT STATUS
- **Phase**: 5 (Crypto & Frontend Integration) ✅
- **Next**: Signature verification, liquidation automation, persistence

### Completed
- ✅ Phase 1: Consensus Core (HotStuff-2)
- ✅ Phase 2: Networking (TCP transport)
- ✅ Phase 3: App Layer (orderbook, accounts, mempool)
- ✅ Phase 4: API (REST, WebSocket)
- ✅ Phase 5: EIP-712 signing, agent keys

### Remaining
- ⏳ On-chain signature verification
- ⏳ Liquidation engine automation
- ⏳ BLS signature aggregation
- ⏳ State persistence (Pebble/RocksDB)
- ⏳ View-change protocol

## AI-NATIVE PRINCIPLES

### File Size Limits
- **Max 500 LOC per file** - Must fit in AI context
- **Max 3 levels deep** - `src/consensus/pacemaker.rs` not `src/consensus/core/timing/pacemaker.rs`
- **One concept per file** - If file does two things, split it

### Code Style
```rust
// GOOD: Explicit, readable
pub fn place_order(order: Order, book: &mut OrderBook) -> Result<Vec<Fill>, OrderError>

// BAD: Magic, implicit
pub fn place(o: impl Into<Order>) -> impl Iterator<Item = Fill>
```

### When You're Stuck
1. Check `docs/specs/` for the relevant specification
2. Check `docs/adr/` for why decisions were made
3. Check `tests/e2e.rs` for expected behavior
4. Ask user if still unclear

## ARCHITECTURE OVERVIEW

```
┌─────────────────────────────────────────────────┐
│                   API Layer                     │
│              (axum REST + WebSocket)            │
└─────────────────────────────────────────────────┘
                       │
┌─────────────────────────────────────────────────┐
│                 Crypto Layer                    │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐           │
│  │ Signer  │ │ EIP-712 │ │  Agent  │           │
│  └─────────┘ └─────────┘ └─────────┘           │
└─────────────────────────────────────────────────┘
                       │
┌─────────────────────────────────────────────────┐
│                 App Layer                       │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐           │
│  │Orderbook│ │Accounts │ │ Mempool │           │
│  └─────────┘ └─────────┘ └─────────┘           │
└─────────────────────────────────────────────────┘
                       │
┌─────────────────────────────────────────────────┐
│               Consensus Layer                   │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐           │
│  │ Engine  │ │Pacemaker│ │ Safety  │           │
│  └─────────┘ └─────────┘ └─────────┘           │
└─────────────────────────────────────────────────┘
                       │
┌─────────────────────────────────────────────────┐
│               Network Layer                     │
│              (TCP transport)                    │
└─────────────────────────────────────────────────┘
```

## FILE STRUCTURE

```
rust/
├── CLAUDE.md              # You are here
├── Cargo.toml
├── .env                   # Configuration (PORT, BLOCK_TIME_MS, etc.)
├── src/
│   ├── lib.rs             # Crate root, re-exports
│   ├── types.rs           # Core types (Block, Vote, Order, etc.)
│   ├── consensus/
│   │   ├── mod.rs         # AppHook, BlockStore traits
│   │   ├── engine.rs      # Main consensus loop (single-node)
│   │   ├── pacemaker.rs   # View advancement, exponential backoff
│   │   ├── runner.rs      # Async multi-node runner
│   │   └── safety.rs      # Voting rules, high_qc, locked_qc
│   ├── network/
│   │   ├── mod.rs         # Network trait
│   │   └── transport.rs   # TCP transport implementation
│   ├── app/
│   │   ├── mod.rs         # Transaction types, MarketConfig
│   │   ├── orderbook.rs   # Heap-based matching engine
│   │   ├── accounts.rs    # Position, margin, PnL tracking
│   │   ├── mempool.rs     # 3-bucket tx ordering
│   │   └── state.rs       # AppState (implements AppHook)
│   ├── crypto/
│   │   ├── mod.rs         # Re-exports
│   │   ├── signer.rs      # ECDSA signing, address recovery
│   │   ├── eip712.rs      # EIP-712 typed data (MetaMask compatible)
│   │   └── agent.rs       # Agent key delegation (gasless trading)
│   └── api/
│       ├── mod.rs         # create_router, re-exports
│       ├── routes.rs      # REST endpoints (/api/v1/*)
│       ├── state.rs       # SharedState, Event types
│       └── websocket.rs   # Real-time updates
├── src/bin/
│   ├── node.rs            # Single-node demo
│   ├── multinode.rs       # Multi-node validator
│   └── server.rs          # API server with consensus (main binary)
└── tests/
    └── e2e.rs             # End-to-end tests
```

## KEY TRAITS (Interfaces)

```rust
/// App hook - consensus calls this to execute blocks
pub trait AppHook: Send + Sync {
    fn prepare_payload(&self, parent: &Block) -> Vec<u8>;
    fn execute(&mut self, block: &Block) -> Hash;  // Returns state hash
}

/// Network abstraction - swap TCP/libp2p without changing consensus
#[async_trait]
pub trait Network: Send + Sync {
    async fn broadcast_propose(&self, propose: Propose) -> Result<()>;
    async fn broadcast_prepare(&self, prepare: Prepare) -> Result<()>;
    async fn send_vote(&self, to: NodeId, vote: Vote) -> Result<()>;
    async fn recv_msg(&self) -> Result<(NodeId, Message)>;
}

/// Storage abstraction - swap in-memory/RocksDB
pub trait BlockStore: Send + Sync {
    fn save(&self, block: &Block);
    fn get(&self, hash: &Hash) -> Option<Block>;
    fn get_by_height(&self, height: u64) -> Option<Block>;
    fn set_committed(&self, hash: &Hash);
}
```

## CONVENTIONS

### Naming
- Files: `snake_case.rs`
- Types: `PascalCase`
- Functions: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`

### Error Handling
```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("mempool error: {0}")]
    Mempool(String),
    #[error("orderbook error: {0}")]
    OrderBook(#[from] OrderBookError),
}
```

### Integer Math
- **Price**: i64 in cents (1 USD = 100)
- **Size**: i64 in satoshis (1 unit = 100_000_000)
- No floats for determinism across validators

## IMPLEMENTATION ORDER

### Phase 1: Consensus Core ✅
1. ✅ `types.rs` - Block, Vote, Certificate, View
2. ✅ `consensus/safety.rs` - Voting rules (4 rules)
3. ✅ `consensus/pacemaker.rs` - View advancement
4. ✅ `consensus/engine.rs` - Main loop (2-chain commit)

### Phase 2: Networking ✅
1. ✅ `network/transport.rs` - TCP transport
2. ✅ `consensus/runner.rs` - Async runner with network

### Phase 3: App Layer ✅
1. ✅ `app/orderbook.rs` - Heap-based matching (O(log N))
2. ✅ `app/accounts.rs` - Position tracking, PnL
3. ✅ `app/mempool.rs` - 3-bucket ordering
4. ✅ `app/state.rs` - AppHook implementation

### Phase 4: API ✅
1. ✅ `api/routes.rs` - REST endpoints
2. ✅ `api/websocket.rs` - Real-time updates
3. ✅ `api/state.rs` - Shared state, events

### Phase 5: Crypto ✅
1. ✅ `crypto/signer.rs` - ECDSA (secp256k1)
2. ✅ `crypto/eip712.rs` - Typed data signing
3. ✅ `crypto/agent.rs` - Agent key delegation

### Phase 6: Hardening ⏳
1. ⏳ On-chain signature verification
2. ⏳ Liquidation automation
3. ⏳ BLS signatures for votes
4. ⏳ State persistence

## COMMANDS

```bash
# Run API server (default port 8080)
cargo run --bin hl-server

# Run with custom config
PORT=3000 BLOCK_TIME_MS=50 cargo run --bin hl-server

# Run tests
cargo test

# Build release
cargo build --release
```

## ENVIRONMENT VARIABLES

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | 8080 | API server port |
| `BLOCK_TIME_MS` | 100 | Block interval (0 = max speed) |
| `LOG_BLOCKS` | false | Log empty heartbeat blocks |
| `RUST_LOG` | info | Log level (error/warn/info/debug/trace) |

## REFERENCE

- **Frontend**: `../web/` (Next.js + MetaMask)
- **HotStuff-2 paper**: See `docs/specs/consensus.md`
- **Hyperliquid docs**: https://hyperliquid.gitbook.io/

## GOLDEN RULES

1. **Read specs first** - Before implementing, check `docs/specs/`
2. **Integer math only** - No floats for determinism
3. **E2E test first** - Write the test, then make it pass
4. **Ask if unclear** - Don't guess on design decisions
