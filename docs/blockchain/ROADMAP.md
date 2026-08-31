# Blockchain Roadmap

> **Audited status — not mainnet readiness:** The historical ✅ marks below record feature
> presence in this development repository. They are not production assurance, independent
> audit results, or launch approval. See [Mainnet readiness](MAINNET_READINESS.md) for the
> current architecture audit and required gates. This roadmap's history is intentionally
> preserved.

## 2026-08-11 P0 current-state override

The historical checkmarks below are feature-history markers, not claims about the current
canonical runtime or launch readiness. The current P0 implementation has added chain/genesis
domain binding, EIP-712 canonical envelopes, schema-v2 BLS PoP and next-epoch key-rotation
recording, RocksDB atomic durable commit/recovery, gossip pre-relay admission with proposer BLS,
and verified-download-only ActiveSync. The runner remains `MODE=dev` only and is **NOT MAINNET
READY**. Verified import/snapshot, indexer proof serving, epoch transition/historical
committee, bridge proof, operations, and independent audits remain launch gates; see
[P0 worklog](WORKLOG_2026-08-11_P0_MAINNET_HARDENING.md).

The local runtime now authenticates the deterministic Commitment v2 artifact containing ordered
receipts and transaction/system events. Its combined root is the dedicated
`Block.commitment_root`, which is covered by the block hash, proposer signature, votes, and QCs
without changing the state-root meaning of `app_hash`. See the
[activation worklog](WORKLOG_2026-08-21_COMMITMENT_V2_CONSENSUS_ACTIVATION.md).

The consensus-authenticated full-state root now uses a fixed nine-component schema v5 tree.
`Block.app_hash`, the V5 block hash, proposer signatures, votes, and QCs bind this root.
Orderbook/staking/
trigger derived indexes are rebuilt atomically at snapshot/import boundaries and validated after
block execution, so the root commits only their authoritative primary records. See the
[derived-index invariant worklog](WORKLOG_2026-08-13_DERIVED_INDEX_INVARIANTS.md) and
[component-tree worklog](WORKLOG_2026-08-13_COMPONENT_TREE_SHADOW.md).

Primary semantic validation now covers market/orderbook/account/staking/trigger/oracle/funding
records at snapshot import, speculative execution, and private replay boundaries. Invalid state
cannot become a candidate, and snapshot validator PoP is rechecked against the trusted node chain
domain. See the [primary-state invariant worklog](WORKLOG_2026-08-13_PRIMARY_STATE_INVARIANTS.md).

Snapshot storage/import and HTTP/P2P block sync now have explicit byte budgets. Sync responders
read only the requested committed height window, page before exceeding the 32 MiB wire/HTTP cap,
and TCP rejects oversized frames before allocation. Snapshot fast-sync remains disabled until a
verified chunk manifest/proof protocol replaces the current bounded single JSON object. See the
[resource-limit worklog](WORKLOG_2026-08-13_SNAPSHOT_SYNC_RESOURCE_LIMITS.md).

The divergent legacy `incremental_hash` Cargo feature and its incomplete dirty cache have been
removed. Every build now uses the schema-v3 component root as `Block.app_hash`; the block hash
domain is V5 and genesis domain V3 additionally binds Commitment v2 schema/version. See the
[legacy incremental-hash removal worklog](WORKLOG_2026-08-13_LEGACY_INCREMENTAL_HASH_REMOVAL.md).
The component layout and domains are recorded in the
[component-tree shadow worklog](WORKLOG_2026-08-13_COMPONENT_TREE_SHADOW.md).
The canonical candidate path derives only invalidated leaves from a sealed parent
tree; new, unknown, chain-domain-mismatched, and recovery states fall back to all nine leaves.
Preflight and direct commit independently recompute the complete tree and fail closed on any
candidate mismatch. Dirty derivation remains only an optimization; fresh preflight/commit
verification is the safety oracle before the authenticated root is voted or persisted.
Speculative restart replay also requires the exact trusted committed-head hash and stages the
whole branch before publishing candidates, preventing anchor substitution and partial recovery.

## Current Status

### Completed

- **Consensus** - HotStuff-2 with 2-chain commit, pacemaker, safety rules
- **Orderbook** - Heap-based O(log N) matching with price-time priority
- **Mempool** - 3-bucket ordering (Hyperlicked-style)
- **Accounts** - Position tracking, margin calculation
- **API** - REST endpoints + WebSocket streaming
- **Crypto** - EIP-712 signing, agent key delegation
- **Frontend** - Trading UI with wallet integration
- **View Change Protocol** - Timeout-based view change, ViewChange/NewView messages
- **BLS Signatures** - BLS12-381 signing, signature aggregation for QCs
- **Order Types Phase 1** - TIF selector (GTC/IOC/ALO), reduce_only, market orders
- **Dev Mode UI** - DEV badge indicator, test USDT faucet button

### In Progress

- **Multi-node testing** - TCP transport between validators, integration tests

### Security Hardening (2026-01-29) ✅

Critical security fixes applied:
- **BLS verification** - Proper aggregate signature verification with `verify_aggregate_same_message`
- **Observer halt-on-corruption** - Nodes halt on app_hash mismatch for operator intervention
- **QC verification in sync** - Full cryptographic verification of certificates during sync
- **Oracle signature verification** - BLS signature required for price updates (skippable in dev)
- **Overflow prevention** - i128 intermediate calculations in all financial math
- **Liquidation accounting** - Separated position PnL from insurance_fund_delta
- **Mempool view safety** - View-checked commit_proposal prevents race conditions

### High Priority Fixes (2026-01-29) ✅

Additional fixes from comprehensive review:
- **HIGH-3: Pacemaker crash-safe** - Timeout state (consecutive_timeouts, vc_sent_for_view) persisted in ConsensusState
- **HIGH-4: ViewChange future limit** - Reject ViewChanges too far ahead (MAX_FUTURE_VIEWS = 10) to prevent memory exhaustion
- **HIGH-8: Liquidation circuit breaker** - MAX_LIQUIDATIONS_PER_BLOCK (default 100) prevents cascade/long blocks
- **HIGH-9: Nonce gap handling** - Allow out-of-order nonces within MAX_NONCE_GAP (10) for dropped tx recovery
- **HIGH-11: Peer reputation** - Track peer failures, blacklist after consecutive failures (PEER_BLACKLIST_THRESHOLD)

### Critical Issues Fixes (2026-01-29 PM) ✅

Remaining critical issues from comprehensive review:
- **CRITICAL-1: voted_views persisted** - Already fixed in runner.rs:651-667 (panic on failure)
- **CRITICAL-2: ConsensusState persistence** - Already fixed (panic on failure for safety)
- **CRITICAL-3: Max order/position size limits** - Added max_order_size, max_position_size, max_open_orders to MarketConfig
- **CRITICAL-5: Insurance fund floor** - Floor at zero after ADL, warning log when below $1M threshold
- **CRITICAL-6: SKIP_QC_VERIFY in prod** - Already fixed in config.rs:246-268 (blocked in mainnet mode)
- **CRITICAL-7: Vote rate limiting** - MAX_VOTES_PER_VALIDATOR_PER_SECOND (10), vote pruning after commit

**Deferred to P2:**
- **CRITICAL-4: Isolated margin** - Complex feature requiring MarginMode enum, margin allocation txs, modified liquidation

### Security Hardening for Multi-Node (2026-01-31) ✅

Production security hardening before testnet deployment:

**Consensus Safety:**
- **Vote rate limiting enforced** - `VoteRateLimiter` in aggregator.rs with sliding window (10 votes/sec/validator)
- **Safety persistence verified** - voted_views already persisted on every vote with panic on failure

**Network Security:**
- **TCP authentication** - BLS-authenticated handshakes via `handshake.rs`, rejects unauthenticated peers in non-dev mode
- **NetworkConfig.require_authenticated_peers** - Default false in dev, true in testnet/mainnet

**API Security:**
- **REST rate limiting** - IP-based limits: 100 req/min (trading), 1000 req/min (read), 20 req/min (sync)
- **WebSocket authentication** - User subscriptions require EIP-191 signature in non-dev mode
- **Agent key support** - WebSocket accepts agent key signatures (no extra MetaMask popup needed)

**Orderbook Fix:**
- **Self-trade continuation** - When all orders at a price level are self-trades, matching continues to next level

All features disabled in dev mode for seamless local testing.

### BLS Security Fix (2026-02-02) ✅

- **bls_batch_verify default** - Enabled by default in Cargo.toml features. Without this, rogue key attacks are possible where Byzantine validators could submit invalid signatures that corrupt aggregate signatures.
- **Fixed signing data mismatch** - `aggregate_bls` now uses `signing_data_common()` to match how `Vote::new_bls` signs votes (excludes voter ID for efficient aggregate verification)

### Mempool Anti-Spam (2026-02-02) ✅

Hardened mempool for gasless trading model:
- **Per-address limits** - Max 100 pending transactions per address (`MEMPOOL_MAX_PER_ADDRESS`)
- **Age-based eviction** - Transactions older than 1 hour are pruned (`MEMPOOL_MAX_AGE_MS`)
- **Configurable bucket size** - Bucket limits are now configurable (`MEMPOOL_MAX_PER_BUCKET`, default 10,000)
- **Address count tracking** - Per-address counts properly maintained across commit/remove/prune operations

### Documentation & API (2026-02-02) ✅

- **API documentation** - Complete REST API and WebSocket protocol documentation
- **Operations docs** - Configuration guide and storage persistence docs
- **Health endpoint** - `GET /api/v1/chain/health` for node monitoring

### Architecture Improvements (2026-02-02) ✅

Code clarity and operational improvements from blockchain expert review:
- **Operator alerting** - `state_corrupted` exposed in `/chain/health` for Byzantine detection
- **Incremental hash** - Migration for incremental app hash computation
- **ADL code extraction** - `process_liquidations_with_adl()` helper for cleaner execute()
- **Follower documentation** - Clarified intentional no-op in Engine::run_follower
- **Matching optimization** - `take_while` iterator for early termination in orderbook matching

### Architecture Improvements Phase 2 (2026-02-03) ✅

Additional improvements from comprehensive blockchain expert review:
- **Orderbook depth limits** - `max_price_levels` in MarketConfig (default: 1000) prevents memory exhaustion from unbounded orderbook growth
- **Staking API exposure** - REST endpoints for pending unstakes visibility (`/api/v1/staking/unstakes/:address`, `/api/v1/staking/summary/:address`)
- **Gossip protocol** - Epidemic message propagation module for multi-node resilience (`src/network/gossip.rs`)
  - Configurable fanout (default: 3 peers), TTL (default: 5 hops), seen cache (default: 10k messages)
  - Environment variables: `GOSSIP_FANOUT`, `GOSSIP_TTL`, `GOSSIP_CACHE_SIZE`, `GOSSIP_ENABLED`

### Recently Completed

- **RPC Node Sync** ✅ - Observer mode with QC verification, sync API endpoints
- **Staking Foundation** ✅ - Epoch transitions, validator sets, jailing, slashing
- **Real-time Streaming** ✅ - Trigger order events, enhanced position updates, order history streaming
- **Market Maker** ✅ - Dev mode artificial liquidity with multiple strategies
- **Oracle Fetcher** ✅ - Background CEX price fetching (Binance, etc.)
- **Trigger Orders (TP/SL)** ✅ - Stop-loss and take-profit with real-time events
- **ADL System** ✅ - Auto-deleverage when insurance fund insufficient
- **Market Stats (activeAssetCtx)** ✅ - Real-time market statistics WebSocket channel
- **Candles API** ✅ - Backend OHLCV aggregation (1m, 5m, 15m, 1h, 4h, 1d intervals)
- **Self-trade Prevention** ✅ - Orderbook skips matching against same trader
- **Trade Deduplication** ✅ - Deterministic trade IDs for WebSocket/REST consistency
- **Liquidation Engine** ✅ - Auto-liquidate underwater positions, insurance fund
- **Signature verification** ✅ - EIP-712 signature verification at API layer, per-account nonce tracking
- **State Persistence** ✅ - RocksDB storage, snapshots, crash recovery

## Upcoming

### P0: Production Essentials

#### On-Chain Signature Verification ✅
- ✅ EIP-712 signature verification at API layer (before mempool admission)
- ✅ Per-account sequential nonces for replay protection
- ✅ Agent delegation support for trading keys
- ✅ `SKIP_SIG_VERIFY` flag for dev mode
- ✅ Nonce query endpoint (`GET /api/v1/accounts/:address/nonce`)

#### Liquidation Engine ✅
- ✅ Check positions after each block execution
- ✅ Liquidate underwater accounts (equity < maintenance margin)
- ✅ Insurance fund tracking and API endpoint
- ✅ 5% maintenance margin (500 basis points)
- ✅ Proper liquidation price calculation in API

#### State Persistence ✅
- ✅ RocksDB integration (column families for blocks, consensus, snapshots)
- ✅ Persist blocks and consensus state on every commit
- ✅ Periodic app state snapshots (configurable interval)
- ✅ Recovery from snapshot + block replay on startup
- `hl-node` uses RocksDB by default; use `--data-dir /path` to select an isolated directory

### P1: Consensus Hardening ✅

#### View Change Protocol ✅
Handle leader failures:
- ✅ Timeout-based view change (ViewChange/NewView messages)
- ✅ New leader election (round-robin, ViewChangeCertificate)
- Basic state sync (SyncRequest/SyncResponse types)

#### BLS Signature Aggregation ✅
Reduce vote size:
- ✅ BLS12-381 signatures (blst crate)
- ✅ Aggregate 2f+1 votes into single 96-byte signature
- Threshold signatures (future)

#### RPC Node Sync ✅
Observer mode for non-validator nodes:
- ✅ Active sync client (poll validators for new blocks)
- ✅ QC certificate verification (Byzantine protection)
- ✅ App hash mismatch rejection (state integrity)
- ✅ Snapshot-based catch-up for nodes far behind
- ✅ `SKIP_QC_VERIFY` flag for dev mode testing
- ✅ Sync API endpoints (`/api/v1/sync/status`, `/blocks`, `/snapshot`)

#### Staking Foundation ✅
Validator economics foundation:
- ✅ ValidatorInfo, Delegation, UnstakeRequest types
- ✅ Epoch-based transitions (~90 min epochs)
- ✅ Active validator set selection (top-N by stake)
- ✅ Epoch transition hook to consensus layer
- ✅ Liveness tracking and jailing
- ✅ Slashing for equivocation (double-voting)
- ✅ 7-day unbonding period

### P2: Hyperlicked Features

#### Market Stats (activeAssetCtx) ✅
Real-time market statistics (like Hyperliquid's activeAssetCtx):
- ✅ WebSocket `assetCtx` event streamed every block
- ✅ REST endpoint `GET /api/v1/markets/:symbol/ctx`
- ✅ 24h volume tracking (base + notional)
- ✅ 24h price change (prev day close tracking)
- ✅ Open interest calculation
- ✅ Mark/oracle/mid price + premium
- ✅ Funding rate + countdown
- ✅ Frontend Header integration

#### Funding Rates ✅
Perpetual funding mechanism:
- ✅ Premium sampling every block (~100ms)
- ✅ Hourly funding payments
- ✅ Interest rate component (0.01%)
- ✅ Funding rate clamping (±4% max)
- ✅ Position cumulative funding tracking
- ✅ Oracle index price integration (with mark price fallback)

#### Oracle System ✅
Multi-source price feeds:
- ✅ Weighted median aggregation
- ✅ Circuit breaker (10% max deviation from mark)
- ✅ Staleness detection (3s default)
- ✅ Validator authorization for submissions
- ✅ Bootstrap mode (falls back to mark price when disabled)
- ✅ External fetcher service (background task, 5s polling)

#### Insurance Fund & ADL ✅
Socialized loss mechanism:
- ✅ Collect liquidation fees (from liquidation engine)
- ✅ Cover underwater positions
- ✅ ADL (auto-deleverage) when insurance fund insufficient
- ✅ Counter-party selection by profit ranking
- ✅ WebSocket ADL event notifications

#### Trigger Orders (TP/SL) ✅
Stop-loss and take-profit orders:
- ✅ Trigger order placement and storage
- ✅ Mark price trigger checking
- ✅ Conversion to market orders on trigger
- ✅ Cancel trigger order support
- ✅ WebSocket real-time trigger events

#### Oracle Price Fetcher ✅
External price fetching service:
- ✅ Background task fetching from CEXs (Binance, etc.)
- ✅ Weighted median aggregation
- ✅ Automatic price updates every 5 seconds
- ✅ Integration with funding rate calculation

#### Market Maker (Dev Mode) ✅
Artificial liquidity for development:
- ✅ Configurable intensity presets (low/medium/high)
- ✅ Multiple trading strategies (market making, trend following, mean reversion)
- ✅ Deterministic account generation from seed
- ✅ Oracle price reference for realistic spreads

#### Real-time WebSocket Streaming ✅
Enhanced event streaming:
- ✅ Trigger order events (placed/triggered/cancelled)
- ✅ Enhanced position updates (with liquidation price, margin, leverage)
- ✅ Order closed events for history streaming
- ✅ cloid support in user fills for order correlation
- ✅ Frontend uses WebSocket-first with REST backup (30s)

### P3: Advanced Features

#### Bridge
Cross-chain deposits/withdrawals:
- 2/3 multisig for withdrawals
- Deposit verification
- Withdrawal queue

#### Staking (Advanced)
Validator economics enhancements:
- Rewards distribution proportional to stake
- Commission rates for validators
- Delegation UI in frontend
- Staking transaction signing

#### EVM Surface (Optional)
Smart contract support:
- Dual-block scheduler
- Limited mempool for EVM
- JSON-RPC subset

## Performance Targets

| Metric | Current | Target |
|--------|---------|--------|
| Block time | 100ms (configurable) | ~20-30ms (network-bound) |
| Execution | ~10ms | <10ms |
| Throughput | ~10k orders/sec | 30k+ orders/sec |
| Finality | 200ms (2-chain) | 40-60ms |

### Key Optimizations Needed

1. **Cached AppHash** - Incremental hashing, only rehash modified symbols
2. **Parallel matching** - Process different symbols concurrently
3. **Object pooling** - Reduce GC pressure on hot path

## Non-Goals

Things we explicitly won't do:

- **Full EVM compatibility** - Focus on perp DEX, not general compute
- **Proof of Work** - BFT provides fast finality
- **Sharding** - Single shard is sufficient for target throughput
- **ZK proofs** - Not needed for L1 consensus
