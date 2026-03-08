# Hyperlicked

Hyperliquid clone in Rust — a standalone perpdex with HotStuff-2 BFT consensus, BTreeMap orderbook matching, and a Next.js trading frontend.

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
└── bin/          # hl-server, hl-node, hl-visor, multinode
web/              # Next.js trading frontend
tests/            # Integration + E2E tests
docs/             # Architecture, API, operations guides
```

## Commands

```bash
cargo run --bin hl-server              # API server + consensus (port 8080)
cargo test                             # All tests
cargo test --test e2e                  # E2E integration tests
cd web && bun run dev                  # Frontend dev server
```

## Golden Rules

**Must do:** Read before modifying. Integer math for prices/sizes. Tests for new features. `thiserror` for domain errors.

**Never do:** Floats in deterministic state. Commit secrets. Skip sig verification in production. Break interfaces without migration.

## Discovery Guide

| Need to know about... | Look here |
|---|---|
| Rust patterns, consensus, orderbook | Activates via `blockchain-dev-guidelines` skill when touching `src/**/*.rs` |
| Frontend patterns, components, state | Activates via `frontend-dev-guidelines` skill when touching `web/**/*.tsx` |
| Environment variables, deployment | `docs/operations/CONFIGURATION.md` |
| API endpoints (REST/WebSocket) | `docs/api/REST.md`, `docs/api/WEBSOCKET.md` |
| Storage, persistence, recovery | `docs/storage/PERSISTENCE.md` |
| Current roadmap and priorities | `docs/blockchain/ROADMAP.md` |
| Architecture decisions | `docs/blockchain/architecture-decisions.md` |
| Active plans and dev context | `docs/plans/`, `docs/dev/` |
| EIP-712 integration pattern | Check MEMORY.md for the checklist |
| Code review | `backend-architecture-reviewer` and `frontend-architecture-reviewer` agents run automatically |
