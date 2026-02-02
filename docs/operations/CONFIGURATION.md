# Configuration Guide

Complete guide to environment variables and configuration options.

## Quick Start

```bash
# Development (default)
cargo run --bin hl-server

# Production
MODE=mainnet DATA_DIR=/data PORT=8080 cargo run --bin hl-server --release

# RPC Node (sync from validators)
MODE=testnet NODE_ROLE=rpc PEERS=http://validator1:8080 cargo run --bin hl-server
```

---

## Runtime Mode

| Variable | Default | Description |
|----------|---------|-------------|
| `MODE` | dev | Runtime mode: `dev`, `testnet`, `mainnet` |
| `NODE_ROLE` | validator | Node role: `validator`, `rpc` |

### Mode Differences

| Feature | dev | testnet | mainnet |
|---------|-----|---------|---------|
| Auto-faucet | Yes | No | No |
| SKIP_SIG_VERIFY | Allowed | Blocked | Blocked |
| SKIP_QC_VERIFY | Allowed | Blocked | Blocked |
| Authenticated peers | Optional | Required | Required |
| WebSocket auth | Optional | Required | Required |

---

## Server Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | 8080 | API server port |
| `BLOCK_TIME_MS` | 100 | Block interval in ms (0 = max speed) |
| `CONSENSUS_LOOP_DELAY_MS` | 10 | Delay between consensus rounds (prevents CPU spin) |
| `LOG_BLOCKS` | false | Log all blocks including empty heartbeats |
| `RUST_LOG` | info | Log level: error, warn, info, debug, trace |

---

## Persistence

| Variable | Default | Description |
|----------|---------|-------------|
| `DATA_DIR` | None | RocksDB data directory (None = in-memory) |
| `SNAPSHOT_INTERVAL` | 1000 | Snapshot app state every N blocks (0 = disabled) |

---

## Security Flags

**WARNING**: These flags bypass critical security checks. Never enable in production.

| Variable | Default | Description |
|----------|---------|-------------|
| `SKIP_SIG_VERIFY` | false | Skip EIP-712 signature verification (dev only!) |
| `SKIP_QC_VERIFY` | false | Skip QC verification for RPC sync (dev only!) |

These flags are **blocked** in testnet and mainnet mode. Attempting to use them will cause a startup error:

```
FATAL: SKIP_SIG_VERIFY=true is not allowed in production (MODE != dev).
```

---

## Mempool Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `MEMPOOL_MAX_PER_BUCKET` | 10000 | Max transactions per bucket |
| `MEMPOOL_MAX_AGE_MS` | 3600000 | Max transaction age before eviction (1 hour) |
| `MEMPOOL_MAX_PER_ADDRESS` | 100 | Max pending transactions per address |

---

## Liquidation

| Variable | Default | Description |
|----------|---------|-------------|
| `MAX_LIQUIDATIONS_PER_BLOCK` | 100 | Max liquidations per block (circuit breaker) |

---

## Network / Sync

| Variable | Default | Description |
|----------|---------|-------------|
| `PEERS` | (empty) | Comma-separated peer URLs for sync |
| `SYNC_POLL_INTERVAL_MS` | 1000 | Sync poll interval for RPC nodes |
| `PEER_BLACKLIST_THRESHOLD` | 5 | Consecutive failures before blacklisting |
| `PEER_BLACKLIST_DURATION_MS` | 60000 | Duration to blacklist a peer (60s) |

---

## Oracle

| Variable | Default | Description |
|----------|---------|-------------|
| `ORACLE_ENABLED` | false | Enable oracle system at startup |

---

## Market Maker (Dev Mode)

| Variable | Default | Description |
|----------|---------|-------------|
| `MM_ENABLED` | false | Enable artificial market maker |
| `MM_INTERVAL_MS` | 100 | Market maker tick interval |
| `MM_INTENSITY` | medium | Intensity preset: low, medium, high |
| `MM_SEED` | 12345 | RNG seed for deterministic addresses |

---

## Development Mode Auto-Faucet

| Variable | Default | Description |
|----------|---------|-------------|
| `DEV_FAUCET_AMOUNT` | 10000000 | Auto-fund amount for new accounts ($100k) |

---

## Production Checklist

### Security
- [ ] `MODE=mainnet` (or `testnet`)
- [ ] `SKIP_SIG_VERIFY` is NOT set
- [ ] `SKIP_QC_VERIFY` is NOT set
- [ ] API behind load balancer with TLS
- [ ] Rate limiting enabled (automatic)

### Reliability
- [ ] `DATA_DIR` set for persistence
- [ ] `SNAPSHOT_INTERVAL` appropriate for data volume
- [ ] Backup strategy in place
- [ ] Monitoring configured

### Performance
- [ ] `BLOCK_TIME_MS` tuned for network latency
- [ ] `CONSENSUS_LOOP_DELAY_MS=0` for max throughput (careful: CPU usage)
- [ ] Sufficient disk IOPS for RocksDB
- [ ] Adequate RAM for mempool/orderbook

---

## Example Configurations

### Single Node Development
```bash
MODE=dev \
MM_ENABLED=true \
ORACLE_ENABLED=true \
RUST_LOG=debug \
cargo run --bin hl-server
```

### Multi-Node Testnet Validator
```bash
MODE=testnet \
NODE_ROLE=validator \
PORT=8080 \
DATA_DIR=/data/hyperlicked \
SNAPSHOT_INTERVAL=1000 \
BLOCK_TIME_MS=50 \
RUST_LOG=info \
cargo run --bin hl-server --release
```

### RPC Node (Sync from Validators)
```bash
MODE=testnet \
NODE_ROLE=rpc \
PORT=8081 \
DATA_DIR=/data/hyperlicked-rpc \
PEERS=http://validator1:8080,http://validator2:8080 \
SYNC_POLL_INTERVAL_MS=500 \
RUST_LOG=info \
cargo run --bin hl-server --release
```

### Production Mainnet
```bash
MODE=mainnet \
NODE_ROLE=validator \
PORT=8080 \
DATA_DIR=/data/hyperlicked \
SNAPSHOT_INTERVAL=1000 \
BLOCK_TIME_MS=100 \
MAX_LIQUIDATIONS_PER_BLOCK=50 \
MEMPOOL_MAX_PER_BUCKET=50000 \
RUST_LOG=warn \
cargo run --bin hl-server --release
```

---

## Supervisor (hl-visor)

For production deployments, use the process supervisor:

```bash
# Validator
cargo run --bin hl-visor run-validator

# Non-validator (RPC)
cargo run --bin hl-visor run-non-validator
```

The visor provides:
- Automatic restart on crash
- Health monitoring
- Version management
- Graceful upgrades

---

## MarketConfig (Compile-time)

Market parameters are configured in `src/app/mod.rs`:

| Field | Default | Description |
|-------|---------|-------------|
| `symbol` | "BTC-USDT" | Market symbol |
| `tick_size` | 1 | Minimum price increment (cents) |
| `lot_size` | 1 | Minimum size increment (satoshis) |
| `min_notional` | 1000 | Minimum order value ($10) |
| `maker_fee` | 2 | Maker fee (0.02%) |
| `taker_fee` | 5 | Taker fee (0.05%) |
| `funding_interval_ms` | 3600000 | Funding interval (1 hour) |
| `interest_rate_bps` | 1 | Interest rate (0.01%) |
| `max_funding_rate_bps` | 400 | Max funding rate (4%) |
| `max_order_size` | 1e12 | Max order (10,000 BTC) |
| `max_position_size` | 1e13 | Max position (100,000 BTC) |
| `max_open_orders` | 100 | Max open orders per account |

---

## Hardcoded Constants (Future Work)

These constants should be made configurable in future versions:

### Protocol Safety
- `MAINTENANCE_MARGIN_BPS` (500) - 5% maintenance margin
- `EQUIVOCATION_SLASH_BPS` (5000) - 50% slash for double-voting
- `MAX_VOTES_PER_VALIDATOR_PER_SECOND` (10) - Vote rate limit

### Liveness
- `MAX_NONCE_GAP` (10) - Max out-of-order nonces
- `UNSTAKE_DELAY_MS` (7 days) - Unbonding period
- `JAIL_DURATION_MS` (1 hour) - Validator jail time

### Memory
- `MAX_CANDLES` (500) - Candles per interval
- `MAX_TRADES_PER_SYMBOL` (1000) - Trade history length
- `INSURANCE_FUND_WARNING_THRESHOLD` ($1M) - Low fund warning

### API Rate Limits
- Trading: 100 req/min
- Read: 1000 req/min
- Heavy: 20 req/min
