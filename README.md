# Hyperlicked

High performance blockchain built for perpdex inspired by Hyperliquid. Can also be used as a standalone perpdex starter.

> **Local prototype warning:** The commands below run development binaries. `hl-node`,
> `multinode`, and `hl-visor` are not production validator, RPC, or supervisor
> deployments and must not be used to custody real funds or bridge real assets.

## Quick Start

### Backend (local prototype)
```bash
# Start the canonical local single-node validator and API (API: 127.0.0.1:8080)
./scripts/local-node

# In another terminal, start the web client
cd web
bun install  # first run, or after dependency changes
bun run dev

# The launcher uses the public development-only validator0 BLS fixture seed.
# Never use HL_LOCAL_BLS_SEED_1 or any value from config/local in production.

# Optional: persist and restart the same local node with RocksDB
hl_restart_dir="$(mktemp -d)"
./scripts/local-node --blocks 3 --data-dir "$hl_restart_dir"
./scripts/local-node --blocks 5 --data-dir "$hl_restart_dir"

# Verified bootstrap: first start all four host-4 validators with their matching
# HL_LOCAL_BLS_SEED_N values; wait until validator0's source API is ready, then stop
# validator3 only. Keep validators0-2 running while restarting validator3 fresh:
HL_LOCAL_BLS_SEED_4=04000000000000000000000000000000000000000000000000000000000000be \
MODE=dev cargo run --locked --bin hl-node -- \
  --genesis config/local/genesis.json --config config/local/host-4/node3.json \
  --sync-peer http://127.0.0.1:8180 --data-dir "$(mktemp -d)"

# Run the local three-node consensus demo in three terminals
# (loopback, deterministic development keys)
cargo run --locked --bin multinode -- --node 0
cargo run --locked --bin multinode -- --node 1
cargo run --locked --bin multinode -- --node 2

# Build binaries for local testing; this is not a production release process
cargo build --release
```

`./scripts/local-node`가 `MODE=dev`, `hl-node`, `--locked`, single-node genesis/config을
자동으로 설정한다. 일반적인 로컬 실행에서는 사용자가 `--bin hl-node`, `--locked`,
`--genesis`, `--config`를 직접 입력할 필요가 없다. `ready ... committed_height=0`은
리스너와 consensus가 시작될 준비가 됐고, 현재 확정 높이가 canonical genesis인 상태라는
뜻이다. 오류나 정지 상태를 뜻하지 않는다. `ready` 출력 뒤 single validator가 consensus를
자동 실행해 블록을 제안하고 확정한다.

launcher의 기본 `RUST_LOG=warn`은 정상적인 proposal/commit info 로그를 숨긴다. 블록 진행을
터미널에서 보려면 다음처럼 실행한다.

```bash
RUST_LOG=info ./scripts/local-node
```

다른 터미널에서 높이를 확인할 수 있다.

```bash
curl -s http://127.0.0.1:8080/api/v1/chain/status
watch -n 1 'curl -s http://127.0.0.1:8080/api/v1/chain/status'
```

`watch`가 없는 macOS에서는 다음처럼 같은 URL을 반복 조회한다.

```bash
while true; do
  curl -s http://127.0.0.1:8080/api/v1/chain/status
  printf '\n'
  sleep 1
done
```

`--blocks N`을 추가하면 committed height가
`N`에 도달한 뒤 프로세스가 종료된다. 생략하면 Ctrl-C까지 계속 실행한다.

`--data-dir`를 생략하면 chain domain과 node ID에 묶인
`.hyperlicked/data/<genesis-domain>/<node-id>` 경로가 사용되므로 재시작 시 같은 RocksDB
상태를 복원한다. 매번 새 체인으로 확인하려면 임시 디렉터리를 명시한다.

```bash
hl_fresh_dir="$(mktemp -d)"
RUST_LOG=info ./scripts/local-node --blocks 3 --data-dir "$hl_fresh_dir"
```

같은 디렉터리를 다시 지정하면 이전 committed height에서 이어서 실행한다.

4-validator Docker fixture와 PoP/schema v2/BLS seed 설명은
[config/local README](config/local/README.md)에 있다. `--sync-peer`는 peer의 finalized block
batch를 HTTP startup 단계에서 검증·replay하며, local genesis와 trusted committee를 trust
root로 사용한다. 잘못된 app hash, Commitment, QC 또는 incomplete snapshot은 fail closed한다.

4-validator Docker smoke test는 다음처럼 실행한다. 각 container가 `--blocks 3`을 받아
committed height 3에 도달하면 종료하므로, 장기 실행 네트워크가 아니다.

```bash
docker compose -f docker-compose.validator4.yml up --build
```

`./scripts/local-node`의 single-node 설정(`:8080`)은 source와 destination으로 동시에 띄울 수
없다. 또한 현재 startup은 모든 configured peer가 ready여야 하므로, 4-validator fixture를
먼저 모두 기동한 뒤 node3만 중지하고 재시작해야 한다. 위 예시는 같은
`config/local/genesis.json`/committee를 사용하되 `host-4/node0.json`(`:8180`)과
`host-4/node3.json`(`:8183`)으로 node와 API/consensus 포트를 분리한다.

### Process Supervisor (local prototype)
```bash
# Development process wrapper only; not production orchestration
cargo run --locked --bin hl-visor run-validator

# Development process wrapper only; not production orchestration
cargo run --locked --bin hl-visor run-non-validator
```

### Tests
```bash
cargo test
```

## Environment Variables

The canonical `hl-node` startup path uses only the small runtime surface below.
Consensus/API addresses and peers belong in the node JSON, not environment variables.
`.env.example` is a reference list; `hl-node` does not automatically load `.env`, so overrides
must be exported by the shell or prefixed to the launch command.

| Variable | Local default | Description |
|----------|---------------|-------------|
| `MODE` | `dev` in the launcher | `hl-node` currently refuses non-dev modes |
| `HL_LOCAL_BLS_SEED_N` | public fixture in the launcher/Compose | 32-byte validator secret seed selected by the node JSON; never reuse the local values in production |
| `RUST_LOG` | `warn` in `scripts/local-node` | Log filter (`info` shows every consensus round) |
| `CONSENSUS_LOOP_DELAY_MS` | `10` | Delay between consensus rounds |

Legacy settings such as `PORT`, `ORACLE_ENABLED`, and `MM_ENABLED` do not start separate
services or mutation loops in the canonical node. Persistence is always enabled for
`hl-node`; prefer the explicit `--data-dir` CLI option for local isolation. Oracle ingress
and market-maker actions must use deterministic consensus transactions before production use.

The four-validator Docker fixture passes `--blocks 3` to every container. It is a finite
consensus smoke test: after all validators reach committed height 3, the containers exit.
It is not a long-running local network.

## Documentation

- **CLAUDE.md** - Development guidelines, architecture, AI instructions
- **docs/blockchain/ROADMAP.md** - Current status and next steps
- **docs/blockchain/MAINNET_READINESS.md** - Current architecture audit and launch gates
- **docs/blockchain/WORKLOG_2026-08-11_HL_NODE_RUNTIME_INTEGRATION.md** - Canonical node/API integration record and local commands
- **docs/blockchain/WORKLOG_2026-08-11_P0_MAINNET_HARDENING.md** - P0 domain, envelope, PoP, persistence, gossip, ActiveSync, and launch blockers
- **docs/blockchain/WORKLOG_2026-08-12_COMMITMENT_V2_ARTIFACTS.md** - Historical deterministic receipt/event shadow artifact and indexer contract
- **docs/blockchain/WORKLOG_2026-08-21_COMMITMENT_V2_CONSENSUS_ACTIVATION.md** - V5 block/QC activation, storage/recovery boundaries, performance, and remaining proof-serving work
- **docs/blockchain/WORKLOG_2026-08-12_FULL_STATE_ROOT_SHADOW.md** - Versioned full-state shadow root, atomic restart verification, coverage audit, benchmark, and activation gates
- **docs/blockchain/WORKLOG_2026-08-13_DERIVED_INDEX_INVARIANTS.md** - Atomic derived-index rebuild, execution-time invariant guards, state-root schema v2, regressions, benchmark, and remaining activation gates
- **docs/blockchain/WORKLOG_2026-08-13_PRIMARY_STATE_INVARIANTS.md** - Primary state semantic validation, snapshot fail-closed boundaries, runtime mutation guards, performance tradeoffs, and remaining activation gates
- **docs/blockchain/WORKLOG_2026-08-23_SPECULATIVE_STATE_COW.md** - Bounded speculative application snapshots, COW/sharding benchmark, and remaining memory work
- **docs/blockchain/WORKLOG_2026-08-24_EQUIVOCATION_EVIDENCE_PIPELINE.md** - Deterministic double-vote evidence, durable delivery, curated committee binding, and remaining PoS work
- **docs/blockchain/WORKLOG_2026-08-25_STATIC_CURATED_STAKING_HARDENING.md** - HYCK bonded power, top-21/static epoch safety, recovery checks, and unstake isolation
- **docs/blockchain/WORKLOG_2026-08-25_HYCK_FIXED_SUPPLY_AND_TRANSITION_STAGING.md** - Fixed 1B HYCK supply, reserve-backed staking rewards, native accounting, and authenticated staged committee transitions
- **docs/blockchain/WORKLOG_2026-08-26_VERIFIED_STATE_SYNC.md** - Replay-first HTTP bootstrap, terminal finality proof, authenticated roots, crash-resumable prefix import, and current limits
- **docs/blockchain/HISTORICAL_COMMITTEE_AND_EPOCH_TRANSITION_PLAN.md** - Cross-epoch transition proof, historical evidence, and atomic activation plan

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
