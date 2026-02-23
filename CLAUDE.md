# Hyperlicked

A Hyperliquid clone built in Rust. Can also be used as a standalone perpdex starter. AI-native codebase designed for AI-assisted development.

## Vision

Clone Hyperliquid's user-facing behavior, features, and performance. HotStuff-2 is the consensus foundation.

## Tech Stack

| Layer | Technology |
|-------|------------|
| Consensus | Rust (HotStuff-2, 2-chain commit, BLS12-381 signatures) |
| Matching Engine | BTreeMap-based orderbook O(log N) |
| API | axum 0.7 (REST + WebSocket) |
| Frontend | Next.js 15 + Tailwind + Zustand |
| Signing | EIP-712 (customers), BLS12-381 (validators) |
| Storage | RocksDB 0.22 (blocks, consensus state, snapshots) |
| Crypto | k256 (ECDSA), blst (BLS), alloy (EIP-712) |

## Core Principles

1. **Integer math only** - No floats for cross-validator determinism
   - Price: i64 in cents (1 USD = 100)
   - Size: i64 in satoshis (1 unit = 100_000_000)
   - Overflow protection: i128 intermediates, `overflow-checks = true` in release

2. **AI-native** - Designed for AI collaboration
   - Max 500 LOC per file
   - Clear interfaces between layers
   - Comprehensive CLAUDE.md
   - Claude Code skills, agents, and hooks

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
    /// Epoch transition: returns new validator set if changed
    fn take_validator_update(&mut self) -> Option<ValidatorSetUpdate> { None }
    /// Submit equivocation evidence for slashing
    fn submit_equivocation_evidence(&mut self, proof: EquivocationProof) -> bool { false }
}

/// Swappable network transport (TCP now, libp2p later)
#[async_trait]
pub trait Network: Send + Sync {
    async fn broadcast_propose(&self, propose: Propose) -> Result<()>;
    async fn send_vote(&self, to: NodeId, vote: Vote) -> Result<()>;
    async fn broadcast_prepare(&self, prepare: Prepare) -> Result<()>;
    async fn broadcast_view_change(&self, vc: ViewChange) -> Result<()>;
    async fn broadcast_new_view(&self, nv: NewView) -> Result<()>;
    async fn recv_msg(&self) -> Result<(NodeId, Message)>;
}

/// Swappable block storage (memory or RocksDB)
pub trait BlockStore: Send + Sync {
    fn save(&self, block: &Block);
    fn get(&self, hash: &Hash) -> Option<Block>;
    fn get_by_height(&self, height: u64) -> Option<Block>;
    fn set_committed(&self, hash: &Hash);
    fn get_committed_head(&self) -> Option<Block>;
}

/// Extended BlockStore with persistence (RocksDB)
pub trait PersistentStore: BlockStore {
    fn save_consensus_state(&self, state: &ConsensusState) -> Result<()>;
    fn load_consensus_state(&self) -> Result<Option<ConsensusState>>;
    fn save_snapshot(&self, height: u64, snapshot: &AppSnapshot) -> Result<()>;
    fn load_latest_snapshot(&self, before_height: u64) -> Result<Option<(u64, AppSnapshot)>>;
    fn blocks_from_height(&self, from_height: u64) -> Result<Vec<Block>>;
    fn commit_block(&self, block: &Block, state: &ConsensusState) -> Result<()>;
}
```

## File Structure

```
hyperlicked/
├── CLAUDE.md              # This file (constitution)
├── Cargo.toml             # Rust deps + features
├── .env.example           # Example environment config
├── src/
│   ├── lib.rs             # Crate root, re-exports
│   ├── config.rs          # Runtime config (Mode, NodeRole, env parsing)
│   ├── types/             # Core types (was types.rs, now a module)
│   │   ├── mod.rs         # Type re-exports, Price/Size aliases
│   │   ├── block.rs       # Block struct
│   │   ├── certificate.rs # QC, Certificate, ViewChangeCertificate
│   │   ├── config.rs      # ConsensusConfig, NodeId
│   │   └── messages.rs    # Propose, Vote, Prepare, ViewChange, NewView
│   ├── consensus/         # HotStuff-2 engine
│   │   ├── mod.rs         # Traits (AppHook, BlockStore), constants
│   │   ├── engine.rs      # Core consensus loop (leader/follower)
│   │   ├── runner.rs      # ConsensusRunner (orchestrates engine + network)
│   │   ├── safety.rs      # Voting rules (when safe to vote)
│   │   ├── pacemaker.rs   # View advancement, timeouts
│   │   ├── aggregator.rs  # Vote aggregation, QC formation, rate limiting
│   │   ├── message_handler.rs # Incoming message dispatch
│   │   ├── view_change.rs # ViewChange/NewView protocol
│   │   ├── timeout.rs     # Timeout certificates
│   │   ├── equivocation.rs # Double-vote detection + proof generation
│   │   └── metrics.rs     # Consensus performance metrics
│   ├── network/           # TCP transport, sync, gossip
│   │   ├── mod.rs         # Network trait definition
│   │   ├── transport.rs   # TcpNetwork implementation
│   │   ├── active_sync.rs # RPC node sync with QC verification
│   │   ├── sync.rs        # SyncClient/SyncHandler
│   │   ├── gossip.rs      # Epidemic gossip protocol
│   │   ├── handshake.rs   # BLS-authenticated peer handshakes
│   │   └── mock.rs        # MockNetwork for testing
│   ├── app/               # Exchange business logic
│   │   ├── mod.rs         # Transaction enum, re-exports
│   │   ├── state/         # AppState (implements AppHook)
│   │   │   ├── mod.rs     # Struct + accessors
│   │   │   ├── execution.rs  # Transaction execution
│   │   │   ├── consensus.rs  # AppHook impl
│   │   │   ├── trigger.rs    # Trigger order evaluation
│   │   │   ├── trigger_tests.rs # Trigger order tests
│   │   │   ├── parallel.rs   # Parallel matching (rayon)
│   │   │   └── incremental_hash.rs # Incremental state hashing
│   │   ├── orderbook/     # BTreeMap-based matching engine
│   │   │   ├── mod.rs     # OrderBook struct, depth limits
│   │   │   └── matching.rs # Price-time priority matching
│   │   ├── oracle/        # External price feeds
│   │   │   ├── mod.rs     # OracleState, price management
│   │   │   ├── fetcher.rs # CEX price fetcher (Binance, etc.)
│   │   │   ├── aggregation.rs # Weighted median aggregation
│   │   │   └── types.rs   # OraclePrice, PriceSource, OracleConfig
│   │   ├── market_maker/  # Artificial market maker (dev mode)
│   │   │   ├── mod.rs     # MarketMakerState
│   │   │   ├── strategy.rs # Trading strategies (MM, trend, mean reversion)
│   │   │   ├── config.rs  # Intensity presets (low/medium/high)
│   │   │   ├── account.rs # Deterministic address generation
│   │   │   └── types.rs   # MM-specific types
│   │   ├── staking/       # Validator staking system
│   │   │   ├── mod.rs     # StakingState re-exports
│   │   │   ├── state.rs   # StakingState struct + logic
│   │   │   ├── types.rs   # ValidatorInfo, Delegation, EpochSnapshot
│   │   │   ├── transactions.rs # Stake/Unstake/Delegate handlers
│   │   │   ├── epoch.rs   # Epoch transitions, validator set selection
│   │   │   ├── rewards.rs # Block reward distribution
│   │   │   ├── jailing.rs # Liveness tracking, jail/unjail
│   │   │   └── slashing.rs # Equivocation slashing, tombstoning
│   │   ├── accounts.rs    # Account & AccountManager (margin, PnL)
│   │   ├── positions.rs   # Position struct
│   │   ├── mempool.rs     # 3-bucket ordering, anti-spam
│   │   ├── candles.rs     # OHLCV aggregation (1m-1d intervals)
│   │   ├── funding.rs     # Funding rate payments
│   │   ├── liquidation.rs # Liquidation engine
│   │   ├── liquidation_queue.rs # Queued liquidation processing
│   │   ├── adl.rs         # Auto-deleverage when insurance fund empty
│   │   └── trigger.rs     # Trigger orders (TP/SL)
│   ├── crypto/            # Cryptographic primitives
│   │   ├── mod.rs         # Re-exports
│   │   ├── eip712.rs      # EIP-712 typed data hashing/signing
│   │   ├── agent.rs       # Agent key delegation for gasless trading
│   │   ├── signer.rs      # ECDSA signing, address recovery
│   │   └── bls.rs         # BLS12-381 sign/aggregate/verify
│   ├── storage/           # Persistence layer
│   │   ├── mod.rs         # ConsensusState, PersistentStore trait
│   │   ├── rocks.rs       # RocksDB implementation
│   │   ├── snapshot.rs    # AppSnapshot serialization
│   │   ├── recovery.rs    # Crash recovery (snapshot + replay)
│   │   └── verify.rs      # Block chain & snapshot verification
│   ├── api/               # REST + WebSocket API
│   │   ├── mod.rs         # Router setup, create_router()
│   │   ├── state.rs       # SharedState (Arc<RwLock<AppState>>)
│   │   ├── handlers.rs    # Common handler utilities
│   │   ├── types.rs       # API request/response types
│   │   ├── verify.rs      # EIP-712 signature verification
│   │   ├── rate_limit.rs  # IP-based rate limiting
│   │   ├── websocket.rs   # WebSocket connection handling
│   │   └── routes/        # Route handlers by domain
│   │       ├── mod.rs     # Route registration
│   │       ├── order.rs   # Place/cancel orders
│   │       ├── account.rs # Balances, positions, nonce
│   │       ├── market.rs  # Orderbook, trades, candles, market ctx
│   │       ├── chain.rs   # Block height, health endpoint
│   │       ├── oracle.rs  # Oracle price submission/query
│   │       ├── staking.rs # Validator, delegation, unstake endpoints
│   │       ├── trigger.rs # Trigger order endpoints
│   │       ├── adl.rs     # ADL status endpoints
│   │       └── sync.rs    # Block sync for RPC nodes
│   └── visor/             # Process supervisor (hl-visor)
│       ├── mod.rs         # Visor re-exports
│       ├── config.rs      # Visor configuration
│       ├── process.rs     # Child process management
│       ├── health.rs      # Health monitoring
│       ├── upgrade.rs     # Binary upgrade logic
│       └── verify.rs      # Binary verification (ed25519)
├── src/bin/
│   ├── server.rs          # Main binary (API + consensus)
│   ├── node.rs            # Consensus-only node
│   ├── visor.rs           # Process supervisor (hl-visor)
│   └── multinode.rs       # Multi-validator testing
├── tests/                 # Integration tests
│   ├── e2e.rs             # E2E test harness entry
│   ├── e2e_tests/         # Domain-specific E2E tests
│   │   ├── helpers/       # Test fixtures, builders, assertions
│   │   ├── matching_test.rs
│   │   ├── orders_test.rs
│   │   ├── positions_test.rs
│   │   ├── accounts_test.rs
│   │   ├── liquidation_test.rs
│   │   ├── funding_test.rs
│   │   ├── oracle_test.rs
│   │   ├── staking_test.rs
│   │   ├── triggers_test.rs
│   │   └── integration_test.rs
│   ├── adl_test.rs        # ADL-specific tests
│   ├── bls_batch_test.rs  # BLS batch verification
│   ├── byzantine_test.rs  # Byzantine fault scenarios
│   ├── consensus_gaps_test.rs
│   ├── equivocation_test.rs
│   ├── gossip_integration_test.rs
│   ├── incremental_hash_test.rs
│   ├── parallel_matching_test.rs
│   ├── recovery_test.rs   # Crash recovery tests
│   ├── state_hash_test.rs
│   └── multinode.rs       # Multi-node integration
├── web/                   # Next.js frontend
│   ├── app/               # Next.js App Router
│   │   ├── layout.tsx     # Root layout
│   │   ├── page.tsx       # Trading page
│   │   └── globals.css    # Tailwind styles
│   ├── components/
│   │   ├── Providers.tsx   # ErrorBoundary + ToastContainer
│   │   ├── trading/        # 19 trading components
│   │   │   ├── Header.tsx, Chart.tsx, Orderbook.tsx
│   │   │   ├── TradePanel.tsx, OrderInputs.tsx, SubmitButton.tsx
│   │   │   ├── Positions.tsx, OpenOrders.tsx, OrderHistory.tsx
│   │   │   ├── AccountInfo.tsx, Balances.tsx, RecentTrades.tsx
│   │   │   ├── TpSlSection.tsx, OrderPreview.tsx, FundingHistory.tsx
│   │   │   └── SideToggle.tsx, OrderTypeSelector.tsx, BottomTabs.tsx, TradeHistory.tsx
│   │   └── ui/            # Shared UI components
│   │       ├── EnableTradingModal.tsx
│   │       ├── ErrorBoundary.tsx
│   │       └── Toast.tsx
│   └── lib/               # Frontend logic
│       ├── store.ts       # Zustand stores (trading, orderbook, account)
│       ├── api.ts         # REST API client
│       ├── types.ts       # TypeScript types
│       ├── config.ts      # API URL, chain config
│       ├── utils.ts       # Formatting, math helpers
│       ├── useWallet.ts   # Wallet connection hook
│       ├── useWebSocket.ts # WebSocket hook
│       ├── useAccountData.ts # Account data polling
│       ├── useOrderSubmit.ts # Order submission logic
│       ├── agentKey.ts    # Agent key management
│       ├── candlestick.ts # Candlestick chart helpers
│       ├── candlestickAggregator.ts # OHLCV aggregation
│       ├── mock-data.ts   # Dev mode mock data
│       ├── wallet/        # Wallet sub-modules
│       │   ├── useWalletConnection.ts
│       │   ├── useWalletSigning.ts
│       │   ├── useAgentKeyHook.ts
│       │   ├── types.ts
│       │   └── index.ts
│       └── websocket/     # WebSocket sub-modules
│           ├── useWebSocketConnection.ts
│           ├── handlers.ts
│           ├── types.ts
│           └── index.ts
└── docs/                  # Documentation
    ├── README.md
    ├── api/               # API documentation
    │   ├── REST.md        # REST endpoint reference
    │   └── WEBSOCKET.md   # WebSocket protocol reference
    ├── blockchain/        # Backend architecture
    │   ├── ROADMAP.md     # Current status + what's next
    │   ├── architecture-decisions.md
    │   └── multinode.md
    ├── operations/        # Operational guides
    │   ├── CONFIGURATION.md
    │   └── INCREMENTAL_HASH_MIGRATION.md
    ├── storage/           # Storage documentation
    │   └── PERSISTENCE.md
    ├── plans/             # Implementation plans
    ├── dev/               # Multi-session context files
    ├── reviews/           # Security review reports
    └── features/          # Feature implementation plans
```

## Cargo Features

```toml
[features]
default = ["bls_batch_verify"]
parallel_matching = ["rayon"]   # Parallel orderbook matching
incremental_hash = []           # Incremental state hashing
bls_batch_verify = []           # BLS batch verify (prevents rogue key attacks)
```

## Binaries

| Binary | Source | Description |
|--------|--------|-------------|
| `hl-server` | `src/bin/server.rs` | Main binary: API server + consensus |
| `hl-node` | `src/bin/node.rs` | Consensus-only node (no API) |
| `hl-visor` | `src/bin/visor.rs` | Process supervisor for production |
| `multinode` | `src/bin/multinode.rs` | Multi-validator test harness |

## Commands

```bash
# Run API server (default port 8080)
cargo run --bin hl-server

# Run with custom config
PORT=3000 BLOCK_TIME_MS=50 cargo run --bin hl-server

# Run tests
cargo test

# Run tests with optional features
cargo test --features parallel_matching
cargo test --features incremental_hash

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
| **Server** | | |
| `MODE` | dev | Runtime mode (dev/testnet/mainnet) |
| `NODE_ROLE` | validator | Node role (validator/rpc) |
| `PORT` | 8080 | API server port |
| `RUST_LOG` | info | Log level (error/warn/info/debug/trace) |
| **Consensus** | | |
| `BLOCK_TIME_MS` | 100 | Block interval for server (0 = max speed) |
| `CONSENSUS_LOOP_DELAY_MS` | 10 | Delay between consensus rounds (0 = yield only) |
| `LOG_BLOCKS` | false | Log empty heartbeat blocks |
| **Storage** | | |
| `DATA_DIR` | None | RocksDB persistence path (None = in-memory only) |
| `SNAPSHOT_INTERVAL` | 1000 | Snapshot app state every N blocks (0 = disabled) |
| **Security** | | |
| `SKIP_SIG_VERIFY` | false | Skip signature verification (dev mode only, unsafe!) |
| `SKIP_QC_VERIFY` | false | Skip QC verification for RPC sync (dev mode only, unsafe!) |
| **Development** | | |
| `DEV_FAUCET_AMOUNT` | 10000000 | Auto-fund amount for new accounts (dev mode only) |
| `ORACLE_ENABLED` | false | Enable oracle system at startup (dev mode) |
| `MM_ENABLED` | false | Enable artificial market maker (dev mode) |
| `MM_INTERVAL_MS` | 100 | Market maker tick interval |
| `MM_INTENSITY` | medium | Intensity preset: low/medium/high |
| `MM_SEED` | 12345 | RNG seed for deterministic MM addresses |
| **Network** | | |
| `PEERS` | (empty) | Comma-separated peer URLs for sync |
| `SYNC_POLL_INTERVAL_MS` | 1000 | Sync poll interval for RPC nodes |
| `PEER_BLACKLIST_THRESHOLD` | 5 | Consecutive failures before blacklisting a peer |
| `PEER_BLACKLIST_DURATION_MS` | 60000 | Duration to blacklist a peer (ms) |
| **Gossip** | | |
| `GOSSIP_ENABLED` | true | Whether gossip protocol is enabled |
| `GOSSIP_FANOUT` | 3 | Number of peers to forward each gossip message to |
| `GOSSIP_TTL` | 5 | Initial TTL for gossip messages (hops) |
| `GOSSIP_CACHE_SIZE` | 10000 | Maximum message IDs to track in gossip seen cache |
| **Mempool** | | |
| `MEMPOOL_MAX_PER_BUCKET` | 10000 | Maximum transactions per mempool bucket |
| `MEMPOOL_MAX_AGE_MS` | 3600000 | Maximum transaction age before eviction (1 hour) |
| `MEMPOOL_MAX_PER_ADDRESS` | 100 | Maximum pending transactions per address |
| **Safety** | | |
| `MAX_LIQUIDATIONS_PER_BLOCK` | 100 | Maximum liquidations per block (circuit breaker) |

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
- Use integer math for all prices/quantities (i128 for intermediates)
- Keep files under 500 LOC
- Write tests for new features
- Update docs after significant changes
- Use `thiserror` for domain errors, `anyhow` for infrastructure

### Never Do
- Use floating point for deterministic state
- Commit secrets or private keys
- Skip signature verification in production (SKIP_SIG_VERIFY blocked in mainnet)
- Break existing interfaces without migration
- Force push without explicit permission

## Documentation

**Start here when context resets:**
- `docs/blockchain/ROADMAP.md` - Current status, what's done, what's next
- `docs/plans/` - Active implementation plans
- `docs/dev/` - Context files for multi-session tasks

**API reference:**
- `docs/api/REST.md` - REST endpoint documentation
- `docs/api/WEBSOCKET.md` - WebSocket protocol documentation

**Operations:**
- `docs/operations/CONFIGURATION.md` - Full configuration guide
- `docs/storage/PERSISTENCE.md` - RocksDB storage and recovery

**Technical patterns (for AI):**
- `.claude/skills/blockchain-dev-guidelines/` - Rust/consensus patterns
- `.claude/skills/frontend-dev-guidelines/` - Next.js/Tailwind patterns

**Security reviews:**
- `docs/reviews/` - Comprehensive security review reports

## Claude Code Extensions

This project uses Claude Code's extension system for AI-assisted development.

### Skills (`.claude/skills/`)

Skills are markdown files that teach Claude domain-specific knowledge. They activate automatically based on keywords or file patterns.

**Structure:**
```
.claude/skills/{skill-name}/
├── SKILL.md           # Main skill file (max 500 lines)
└── references/        # Reference files for detailed info
```

**Our skills:**
- `blockchain-dev-guidelines` - Rust/HotStuff-2 patterns (triggers on `src/*.rs`)
  - References: CONSENSUS.md, CRYPTO.md, ORDERBOOK.md, PATTERNS.md, TESTING.md, TYPES.md
- `frontend-dev-guidelines` - Next.js/Tailwind patterns (triggers on `web/*.tsx`)
  - References: API.md, COMPONENTS.md, HOOKS.md, STATE.md, STYLING.md, WALLET.md
- `skill-developer` - Meta-skill for creating new skills
  - Resources: ADVANCED.md, HOOK_MECHANISMS.md, PATTERNS_LIBRARY.md, SKILL_RULES_REFERENCE.md, TRIGGER_TYPES.md, TROUBLESHOOTING.md

### Agents (`.claude/agents/`)

Agents are specialized subagents that run in their own context with specific tools and prompts.

**Our agents:**
- `backend-architecture-reviewer` - Reviews Rust code for HotStuff-2/orderbook patterns
- `frontend-architecture-reviewer` - Reviews React code for Next.js/Tailwind patterns

### Hooks (`.claude/hooks/`)

Hooks run automatically on Claude Code events:

- `skill-activation-prompt.sh/.ts` - Activates relevant skills on user prompt (UserPromptSubmit)
- `post-work-verify.sh` - Runs verification after Edit/Write operations (PostToolUse)
- `stop-build-check.sh` - Build verification on stop

### Commands (`.claude/commands/`)

- `commit.md` - Structured commit workflow

**Key differences:**
| Feature | Skills | Agents |
|---------|--------|--------|
| Purpose | Domain knowledge | Task delegation |
| Context | Shared with main | Separate context |
| Activation | Auto (keywords/files) | Claude delegates |
| Tools | Inherits all | Specified per agent |

## Codebase Stats (as of 2026-02-23)

- **Rust source files:** 95 across 17 directories
- **Total Rust LOC:** ~34,300
- **Integration tests:** 27 files (11 standalone + 10 E2E + 6 helpers)
- **Frontend components:** 22 (19 trading + 3 UI)

## References

- HotStuff-2 paper (2-chain commit, pacemaker)
- Hyperliquid docs: https://hyperliquid.gitbook.io/
