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

- **Multi-node testing** - TCP transport between validators

### Recently Completed

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

### P2: Hyperlicked Features

#### Funding Rates ✅
Perpetual funding mechanism:
- ✅ Premium sampling every block (~100ms)
- ✅ Hourly funding payments
- ✅ Interest rate component (0.01%)
- ✅ Funding rate clamping (±4% max)
- ✅ Position cumulative funding tracking
- Bootstrap mode: uses mark price as index (no external oracle)

#### Oracle System
Multi-source price feeds:
- Aggregate from 8+ venues
- Weighted median
- 3-second update cadence

#### Insurance Fund
Socialized loss mechanism:
- ✅ Collect liquidation fees (from liquidation engine)
- Cover underwater positions
- ADL (auto-deleverage) as last resort

### P3: Advanced Features

#### Bridge
Cross-chain deposits/withdrawals:
- 2/3 multisig for withdrawals
- Deposit verification
- Withdrawal queue

#### Staking
Validator economics:
- Top-21 validators by stake
- Epoch-based rotation (~90 min)
- Slashing for misbehavior
- Jailing for downtime

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
