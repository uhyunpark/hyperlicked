# Hyperlicked

A Hyperliquid clone built in Rust. AI-native codebase designed for AI-assisted development.

## Vision

Clone Hyperliquid's user-facing behavior, features, and performance. HotStuff-2 is the consensus foundation.

## Tech Stack

| Layer | Technology |
|-------|------------|
| Consensus | Rust (HotStuff-2, 2-chain commit, BLS signatures) |
| Matching Engine | Heap-based orderbook O(log N) |
| API | axum (REST + WebSocket) |
| Frontend | Next.js 15 + Tailwind + Zustand |
| Signing | EIP-712 (customers), BLS12-381 (validators) |

## Core Principles

1. **Integer math only** - No floats for cross-validator determinism
   - Price: i64 in cents (1 USD = 100)
   - Size: i64 in satoshis (1 unit = 100_000_000)

2. **AI-native** - Designed for AI collaboration
   - Max 500 LOC per file
   - Clear interfaces between layers
   - Comprehensive CLAUDE.md

3. **Hyperliquid parity** - Match their behavior, not just theory
   - 3-bucket mempool ordering
   - Sub-second block times
   - Gasless trading via agent keys

## Key Interfaces

```rust
/// Consensus calls this to execute blocks
pub trait AppHook: Send + Sync {
    fn prepare_payload(&self, parent: &Block) -> Vec<u8>;
    fn execute(&mut self, block: &Block) -> Hash;
}

/// Swappable network transport (TCP now, libp2p later)
#[async_trait]
pub trait Network: Send + Sync {
    async fn broadcast_propose(&self, propose: Propose) -> Result<()>;
    async fn send_vote(&self, to: NodeId, vote: Vote) -> Result<()>;
    async fn recv_msg(&self) -> Result<(NodeId, Message)>;
}

/// Swappable storage (memory now, RocksDB later)
pub trait BlockStore: Send + Sync {
    fn save(&self, block: &Block);
    fn get(&self, hash: &Hash) -> Option<Block>;
    fn set_committed(&self, hash: &Hash);
}
```

## File Structure

```
hyperlicked/
├── CLAUDE.md              # This file (constitution)
├── Cargo.toml             # Rust dependencies
├── src/
│   ├── lib.rs             # Crate root
│   ├── types.rs           # Core types (Block, Vote, Order)
│   ├── config.rs          # Runtime config (mode, faucet, etc.)
│   ├── consensus/         # HotStuff-2 engine + BLS vote aggregation
│   ├── network/           # TCP transport
│   ├── app/               # Orderbook, accounts, mempool
│   ├── crypto/            # EIP-712, agent keys, BLS12-381
│   ├── storage/           # RocksDB persistence, snapshots, recovery
│   └── api/               # REST + WebSocket
├── src/bin/
│   ├── server.rs          # Main binary (API + consensus)
│   ├── node.rs            # Consensus-only node
│   └── multinode.rs       # Multi-validator testing
├── tests/                 # Integration tests
├── web/                   # Next.js frontend
└── docs/                  # Documentation
    ├── blockchain/        # Backend architecture
    └── frontend/          # Frontend architecture
```

## Commands

```bash
# Run API server (default port 8080)
cargo run --bin hl-server

# Run with custom config
PORT=3000 BLOCK_TIME_MS=50 cargo run --bin hl-server

# Run tests
cargo test

# Run frontend
cd web && bun run dev

# Build release
cargo build --release
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `MODE` | dev | Runtime mode (dev/testnet/mainnet) |
| `PORT` | 8080 | API server port |
| `BLOCK_TIME_MS` | 100 | Block interval for server (0 = max speed) |
| `CONSENSUS_LOOP_DELAY_MS` | 10 | Delay between consensus rounds (0 = yield only) |
| `LOG_BLOCKS` | false | Log empty heartbeat blocks |
| `RUST_LOG` | info | Log level (error/warn/info/debug/trace) |
| `DEV_FAUCET_AMOUNT` | 10000000 | Auto-fund amount for new accounts (dev mode only) |
| `DATA_DIR` | None | RocksDB persistence path (None = in-memory only) |
| `SNAPSHOT_INTERVAL` | 1000 | Snapshot app state every N blocks (0 = disabled) |
| `SKIP_SIG_VERIFY` | false | Skip signature verification (dev mode only, unsafe!) |

## Golden Rules

### Must Do
- Read files before modifying them
- Use integer math for all prices/quantities
- Keep files under 500 LOC
- Write tests for new features
- Update docs after significant changes

### Never Do
- Use floating point for deterministic state
- Commit secrets or private keys
- Skip signature verification in production
- Break existing interfaces without migration

## Documentation

**Start here when context resets:**
- `docs/blockchain/ROADMAP.md` - Current status, what's done, what's next
- `docs/plans/` - Active implementation plans
- `docs/dev/` - Context files for multi-session tasks

**Technical patterns (for AI):**
- `.claude/skills/blockchain-dev-guidelines/` - Rust/consensus patterns
- `.claude/skills/frontend-dev-guidelines/` - Next.js/Tailwind patterns

## References

- HotStuff-2 paper (2-chain commit, pacemaker)
- Hyperliquid docs: https://hyperliquid.gitbook.io/
