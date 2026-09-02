# Hyperlicked

A standalone perpdex with HotStuff-2 BFT consensus, BTreeMap orderbook matching, and a Next.js trading frontend. Inspired by hyperliquid.

## Tech Stack

| Layer | Technology |
|-------|------------|
| Consensus | Rust, HotStuff-2 (2-chain commit, BLS12-381) |
| Matching | BTreeMap orderbook, price-time priority |
| API | axum 0.7 (REST + WebSocket) |
| Frontend | Next.js 15 + Tailwind + Zustand |
| Signing | EIP-712 (traders), BLS12-381 (validators) |
| Storage | RocksDB (blocks, consensus, snapshots) |

## Core Principles

1. **Integer math only** — Price: i64 cents (1 USD = 100). Size: i64 satoshis (1 unit = 1e8). i128 intermediates for overflow safety.
2. **500 LOC max per file** — Split into modules when approaching limit.
3. **Hyperliquid parity** — 3-bucket mempool, sub-second blocks, gasless trading via agent keys.

## Project Layout

```
src/
├── consensus/    # HotStuff-2 engine, pacemaker, safety, aggregator
├── app/          # Exchange logic: orderbook, accounts, staking, oracle, funding, liquidation
├── crypto/       # BLS, EIP-712, ECDSA, agent keys
├── api/          # axum REST + WebSocket routes
├── network/      # TCP transport, gossip, sync
├── storage/      # RocksDB persistence, snapshots, recovery
├── types/        # Block, Vote, Certificate, config types
├── visor/        # Process supervisor: binary upgrades, health checks (hl-visor)
└── bin/          # hl-node, hl-visor, multinode
web/              # Next.js trading frontend
tests/            # Integration + E2E tests
docs/             # Architecture decisions, roadmap
```

## Commands

```bash
./scripts/local-node                    # Canonical local N=1 node: consensus + REST + WebSocket
RUST_LOG=info ./scripts/local-node      # Same node with consensus progress logs
curl -s http://127.0.0.1:8080/api/v1/chain/status  # Check committed height
cargo test                             # All tests
cargo test --test e2e                  # E2E integration tests
cd web && bun run dev                  # Frontend dev server
cd web && bun run lint                 # Lint frontend (biome)
cargo clippy --all-targets             # Lint Rust
```

## Golden Rules

**Must do:** Read before modifying. Integer math for prices/sizes. Tests for new features. `thiserror` for domain errors.

**Never do:** Floats in deterministic state. Commit secrets. Skip sig verification in production. Break interfaces without migration.

## Discovery Guide

| Need to know about... | Look here |
|---|---|
| Rust patterns, consensus, orderbook | `blockchain-dev-guidelines` skill (auto-activates on `src/**/*.rs`) |
| Frontend patterns, components, state | `frontend-dev-guidelines` skill (auto-activates on `web/**/*.tsx`) |
| API endpoints, WebSocket channels | `src/api/CLAUDE.md` (auto-loaded) |
| Storage schema, snapshots, recovery | `src/storage/CLAUDE.md` (auto-loaded) |
| Exchange logic, MarketConfig | `src/app/CLAUDE.md` (auto-loaded) |
| Environment variables | Read `src/config.rs` directly |
| Roadmap and priorities | `docs/blockchain/ROADMAP.md` |
| EIP-712 integration pattern | `blockchain-dev-guidelines` skill → `references/CRYPTO.md` (backend), `frontend-dev-guidelines` skill → `references/WALLET.md` (frontend) |
| Code review | `backend-architecture-reviewer` and `frontend-architecture-reviewer` agents run automatically |
