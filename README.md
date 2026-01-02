# Hyperlicked

Hyperliquid clone built in Rust.

## How to run

### Backend (Rust)
```bash
# Build
cargo build --release

# Run node with API server
cargo run --bin hl-server

# Or run consensus-only node
cargo run --bin hl-node
```

### Frontend
```bash
cd web && bun run dev
```

### Environment Variables
Configure via `.env`:
```
PORT=8080
RUST_LOG=info
BLOCK_TIME_MS=100
LOG_BLOCKS=false
```