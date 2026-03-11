# Hyperlicked

High performance blockchain built for perpdex inspired by Hyperliquid. Can also be used as a standalone perpdex starter.

## Quick Start

### Backend
```bash
# Run API server (default port 8080)
cargo run --bin hl-server

# Run with custom config
PORT=3000 BLOCK_TIME_MS=50 cargo run --bin hl-server

# Run consensus-only node
cargo run --bin hl-node

# Build release
cargo build --release
```

### Frontend
```bash
cd web && bun run dev
```

### Process Supervisor (Production)
```bash
# Run as validator
cargo run --bin hl-visor run-validator

# Run as RPC node
cargo run --bin hl-visor run-non-validator
```

### Tests
```bash
cargo test
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `MODE` | dev | Runtime mode (dev/testnet/mainnet) |
| `NODE_ROLE` | validator | Node role (validator/rpc) |
| `PORT` | 8080 | API server port |
| `BLOCK_TIME_MS` | 100 | Block interval (0 = max speed) |
| `CONSENSUS_LOOP_DELAY_MS` | 10 | Delay between consensus rounds |
| `LOG_BLOCKS` | false | Log empty heartbeat blocks |
| `RUST_LOG` | info | Log level (error/warn/info/debug/trace) |
| `DEV_FAUCET_AMOUNT` | 10000000 | Auto-fund new accounts (dev mode, cents) |
| `DATA_DIR` | None | RocksDB persistence path |
| `SNAPSHOT_INTERVAL` | 1000 | Snapshot every N blocks (0 = disabled) |
| `SKIP_SIG_VERIFY` | false | Skip signature verification (dev only!) |
| `ORACLE_ENABLED` | false | Enable oracle system (dev mode) |
| `MM_ENABLED` | false | Enable market maker (dev mode) |
| `MM_INTERVAL_MS` | 100 | Market maker tick interval |
| `MM_INTENSITY` | medium | MM intensity: low/medium/high |
| `MM_SEED` | 12345 | RNG seed for deterministic MM addresses |
| `PEERS` | (empty) | Comma-separated peer URLs for sync |
| `SYNC_POLL_INTERVAL_MS` | 1000 | Sync poll interval for RPC nodes |

## Documentation

- **CLAUDE.md** - Development guidelines, architecture, AI instructions
- **docs/blockchain/ROADMAP.md** - Current status and next steps

## Tech Stack

| Layer | Technology |
|-------|------------|
| Consensus | Rust (HotStuff-2, 2-chain commit, BLS signatures) |
| Matching Engine | Heap-based orderbook O(log N) |
| API | axum (REST + WebSocket) |
| Frontend | Next.js 15 + Tailwind + Zustand |
| Signing | EIP-712 (customers), BLS12-381 (validators) |

## References

- [HotStuff-2 paper](https://eprint.iacr.org/2023/397) (2-chain commit, pacemaker)
- [Hyperliquid docs](https://hyperliquid.gitbook.io/)
