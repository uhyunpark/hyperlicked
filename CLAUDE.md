# Hyperlicked

A Hyperliquid clone built in Rust. Can also be used as a standalone perpdex starter. AI-native codebase designed for AI-assisted development.

## Vision

Clone Hyperliquid's user-facing behavior, features, and performance. HotStuff-2 is the consensus foundation.

## Tech Stack

| Layer | Technology |
|-------|------------|
| Consensus | Rust (HotStuff-2, 2-chain commit, BLS signatures) |
| Matching Engine | BTreeMap-based orderbook O(log N) |
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
│   ├── network/           # TCP transport, RPC sync client, gossip
│   │   ├── active_sync.rs # RPC node sync with QC verification
│   │   └── gossip.rs      # Epidemic gossip protocol for message propagation
│   ├── app/               # Orderbook, accounts, mempool, staking
│   │   ├── mod.rs         # Transaction types, MarketConfig
│   │   ├── state/         # AppState (implements AppHook)
│   │   │   ├── mod.rs     # Struct + accessors
│   │   │   ├── execution.rs # Transaction execution
│   │   │   └── consensus.rs # AppHook impl
│   │   ├── orderbook/     # BTreeMap-based matching engine
│   │   │   ├── mod.rs     # OrderBook struct
│   │   │   └── matching.rs # Matching logic
│   │   ├── oracle/        # External price feeds for funding
│   │   │   ├── mod.rs     # OracleState, aggregation
│   │   │   └── fetcher.rs # CEX price fetcher (Binance, etc.)
│   │   ├── market_maker/  # Artificial market maker (dev mode)
│   │   │   ├── mod.rs     # MarketMakerState
│   │   │   ├── strategy.rs # Trading strategies
│   │   │   ├── config.rs  # Intensity presets
│   │   │   └── account.rs # Deterministic address generation
│   │   ├── accounts.rs    # Account & AccountManager
│   │   ├── positions.rs   # Position struct
│   │   ├── mempool.rs     # 3-bucket ordering
│   │   ├── candles.rs     # OHLCV aggregation
│   │   ├── funding.rs     # Funding rate payments
│   │   ├── liquidation.rs # Liquidation engine
│   │   ├── adl.rs         # Auto-deleverage when insurance fund empty
│   │   ├── trigger.rs     # Trigger orders (TP/SL)
│   │   └── staking/       # Validator staking system
│   ├── crypto/            # EIP-712, agent keys, BLS12-381
│   ├── storage/           # RocksDB persistence, snapshots, recovery
│   └── api/               # REST + WebSocket
├── src/bin/
│   ├── server.rs          # Main binary (API + consensus)
│   ├── node.rs            # Consensus-only node
│   ├── visor.rs           # Process supervisor (hl-visor)
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

# Run process supervisor (production)
cargo run --bin hl-visor run-validator
cargo run --bin hl-visor run-non-validator

# Build release
cargo build --release
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `MODE` | dev | Runtime mode (dev/testnet/mainnet) |
| `NODE_ROLE` | validator | Node role (validator/rpc) |
| `PORT` | 8080 | API server port |
| `BLOCK_TIME_MS` | 100 | Block interval for server (0 = max speed) |
| `CONSENSUS_LOOP_DELAY_MS` | 10 | Delay between consensus rounds (0 = yield only) |
| `LOG_BLOCKS` | false | Log empty heartbeat blocks |
| `RUST_LOG` | info | Log level (error/warn/info/debug/trace) |
| `DEV_FAUCET_AMOUNT` | 10000000 | Auto-fund amount for new accounts (dev mode only) |
| `DATA_DIR` | None | RocksDB persistence path (None = in-memory only) |
| `SNAPSHOT_INTERVAL` | 1000 | Snapshot app state every N blocks (0 = disabled) |
| `SKIP_SIG_VERIFY` | false | Skip signature verification (dev mode only, unsafe!) |
| `SKIP_QC_VERIFY` | false | Skip QC verification for RPC sync (dev mode only, unsafe!) |
| `ORACLE_ENABLED` | false | Enable oracle system at startup (dev mode) |
| `MM_ENABLED` | false | Enable artificial market maker (dev mode) |
| `MM_INTERVAL_MS` | 100 | Market maker tick interval |
| `MM_INTENSITY` | medium | Intensity preset: low/medium/high |
| `MM_SEED` | 12345 | RNG seed for deterministic MM addresses |
| `PEERS` | (empty) | Comma-separated peer URLs for sync |
| `SYNC_POLL_INTERVAL_MS` | 1000 | Sync poll interval for RPC nodes |
| `PEER_BLACKLIST_THRESHOLD` | 5 | Consecutive failures before blacklisting a peer |
| `PEER_BLACKLIST_DURATION_MS` | 60000 | Duration to blacklist a peer (ms) |
| `MAX_LIQUIDATIONS_PER_BLOCK` | 100 | Maximum liquidations per block (circuit breaker) |
| `MEMPOOL_MAX_PER_BUCKET` | 10000 | Maximum transactions per mempool bucket |
| `MEMPOOL_MAX_AGE_MS` | 3600000 | Maximum transaction age before eviction (1 hour) |
| `MEMPOOL_MAX_PER_ADDRESS` | 100 | Maximum pending transactions per address |
| `GOSSIP_FANOUT` | 3 | Number of peers to forward each gossip message to |
| `GOSSIP_TTL` | 5 | Initial TTL for gossip messages (hops) |
| `GOSSIP_CACHE_SIZE` | 10000 | Maximum message IDs to track in gossip seen cache |
| `GOSSIP_ENABLED` | true | Whether gossip protocol is enabled |

## MarketConfig

Per-market configuration set in `src/app/mod.rs`:

| Field | Default | Description |
|-------|---------|-------------|
| `symbol` | "BTC-USDT" | Market symbol |
| `tick_size` | 1 | Minimum price increment (cents) |
| `lot_size` | 1 | Minimum size increment (satoshis) |
| `min_notional` | 1000 | Minimum order value (cents, $10) |
| `maker_fee` | 2 | Maker fee in basis points (0.02%) |
| `taker_fee` | 5 | Taker fee in basis points (0.05%) |
| `funding_interval_ms` | 3600000 | Funding interval (1 hour) |
| `interest_rate_bps` | 1 | Interest rate component (0.01%) |
| `max_funding_rate_bps` | 400 | Max funding rate cap (4%) |
| `max_order_size` | 1e12 | Max single order (10,000 BTC in satoshis) |
| `max_position_size` | 1e13 | Max position per account (100,000 BTC in satoshis) |
| `max_open_orders` | 100 | Max open orders per account |
| `max_price_levels` | 1000 | Max price levels per side (OOM prevention) |

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

## Claude Code Extensions

This project uses Claude Code's extension system for AI-assisted development.

### Skills (`.claude/skills/`)

Skills are markdown files that teach Claude domain-specific knowledge. They activate automatically based on keywords or file patterns. See [Claude Code Skills docs](https://code.claude.com/docs/en/skills).

**Structure:**
```
.claude/skills/{skill-name}/
├── SKILL.md           # Main skill file (max 500 lines)
└── resources/         # Reference files for detailed info
```

**Our skills:**
- `blockchain-dev-guidelines` - Rust/HotStuff-2 patterns (triggers on `src/*.rs`)
- `frontend-dev-guidelines` - Next.js/Tailwind patterns (triggers on `web/*.tsx`)
- `skill-developer` - Meta-skill for creating new skills

**SKILL.md format:**
```yaml
---
name: skill-name
description: What it does and trigger keywords (max 1024 chars)
---
# Skill content (markdown)
```

### Agents (`.claude/agents/`)

Agents are specialized subagents that run in their own context with specific tools and prompts. They're delegated to for complex tasks. See [Claude Code Subagents docs](https://code.claude.com/docs/en/sub-agents).

**Structure:**
```
.claude/agents/{agent-name}.md
```

**Our agents:**
- `backend-architecture-reviewer` - Reviews Rust code for HotStuff-2/orderbook patterns
- `frontend-architecture-reviewer` - Reviews React code for Next.js/Tailwind patterns

**Agent format:**
```yaml
---
name: agent-name
description: When Claude should delegate to this agent
model: sonnet|opus|haiku
tools: Read, Grep, Glob, Bash  # Comma-separated tools
---
# Agent system prompt (markdown)
```

**Key differences:**
| Feature | Skills | Agents |
|---------|--------|--------|
| Purpose | Domain knowledge | Task delegation |
| Context | Shared with main | Separate context |
| Activation | Auto (keywords/files) | Claude delegates |
| Tools | Inherits all | Specified per agent |

## References

- HotStuff-2 paper (2-chain commit, pacemaker)
- Hyperliquid docs: https://hyperliquid.gitbook.io/
