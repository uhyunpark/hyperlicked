# Blockchain Roadmap

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

See `docs/reviews/2026-01-29-comprehensive-blockchain-review.md` for full details.

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
- Set `DATA_DIR=/path` to enable persistence

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
