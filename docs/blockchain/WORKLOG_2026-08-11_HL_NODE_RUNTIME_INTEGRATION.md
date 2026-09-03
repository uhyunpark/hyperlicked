# hl-node 단일 런타임 통합 작업 로그

> **Historical integration snapshot.** This document records the state immediately after the
> `hl-server`/`hl-node` runtime merge and before the later 2026-08-11 P0 hardening. Its remaining
> items (memory store, unbound chain domain, unsigned envelope, unsafe ActiveSync, and gossip
> admission) were addressed or re-scoped by the P0 tranche; do not use those lines as the
> current status. See [P0 메인넷 하드닝 작업 로그](WORKLOG_2026-08-11_P0_MAINNET_HARDENING.md).

> **현재 상태: NOT MAINNET READY.**
> 이 문서는 로컬 통합 경로와 검증 checkpoint를 기록한다. 메인넷 운영, 실자금 보관,
> 브리지 출시, 독립 보안 감사 또는 launch approval을 의미하지 않는다.

관련 문서: [canonical runtime 계획](CANONICAL_RUNTIME_AND_FUTURE_PLAN.md),
[메인넷 준비도 감사](MAINNET_READINESS.md),
[로컬 fixture 명령](../../config/local/README.md)

## 1. 통합 배경: 두 런타임은 같은 노드의 두 모드가 아니었다

이전에는 `hl-server`와 `hl-node`가 서로 다른 상태 전이 소유자였다.

- `hl-server`는 REST/WebSocket과 local `run_consensus_loop`를 함께 실행했다. 이 loop는
  네트워크 HotStuff-2의 proposal/QC/justify/commit 경로와 동일한 소유자가 아니었고,
  `AppState`를 직접 변경하는 개발용 빈 블록·heartbeat 경로를 가졌다.
- `hl-node`는 genesis/node 파일, 환경변수 BLS seed, authenticated TCP와
  `ConsensusRunner`를 소유했지만 API/WS 표면이 없었다.
- 따라서 API가 읽거나 변경하는 상태와 validator가 QC로 확정하는 상태가 분리될 수
  있었다. oracle fetcher와 market-maker task 같은 외부 loop도 합의 입력으로 정렬되지
  않은 직접 mutation 경로가 될 수 있었다. 두 프로세스를 함께 실행하는 것만으로는
  하나의 canonical chain이 되지 않는다.

이번 통합에서는 `Cargo.toml`의 standalone `hl-server` binary entry와 그 fake consensus
경로를 제거하고, API/WS를 `hl-node`의 consensus-owned process 안으로 옮겼다. `hl-node`가
유일한 validator/RPC 런타임이며 `multinode`는 같은 consensus 경로를 점검하는 개발용
harness로 남는다. API, oracle, market-maker가 합의 상태를 별도 loop에서 직접 확정하는
경로를 canonical contract로 취급하지 않는다.

## 2. 최종 로컬 구조

```text
client / web
      │ HTTP + WebSocket
      ▼
┌──────────────────────────────────────────────┐
│                    hl-node                    │
│  API/WS ingress → mempool → execution hook   │
│             ↘ authenticated TCP ↙             │
│       proposal / vote / QC / 2-chain commit   │
│              canonical committed state        │
└──────────────────────────────────────────────┘
```

하나의 `hl-node` process가 process-local `NodeFile`의 consensus `listen_addr`와 API
`api_listen_addr`를 읽고, shared genesis의 committee와 환경변수 BLS seed를 검증한다.
API/WS는 같은 `SharedState`/canonical application hook을 사용하며, 별도의 validator
loop나 fake server를 시작하지 않는다. 현재 이 구조는 curated local genesis와
`MODE=dev`에 한정된다.

### Candidate에서 API/WS까지의 finality 경계

현재 로컬 consensus 모델에서 API가 관찰해야 할 경계는 다음과 같다.

1. 클라이언트가 signed action을 API로 제출하면 `hl-node` ingress가 형식, 서명, domain,
   nonce와 admission 조건을 확인한 뒤 mempool 후보로 둔다.
2. leader는 parent 위에서 exact payload를 실행할 **speculative candidate** block을
   deterministic ordering으로 제안한다. 이 candidate와 그 실행 결과는 아직 canonical
   query/WS 결과가 아니다.
3. validator들은 parent, payload/app hash, epoch/committee context와 proposal을
   검증하고 vote한다. 유효한 vote 집합으로 QC가 만들어져 candidate가 certified 된다.
4. 다음 proposal의 QC가 HotStuff-2 two-chain 조건을 만족하면 해당 ancestor가 commit된다.
   commit 전 candidate·QC는 finalized API 상태가 아니다.
5. commit된 block/state만 canonical query와 WebSocket block/event stream의 source가 된다.
   따라서 API/WS가 speculative candidate를 finalized balance, market, account 또는
   block으로 노출해서는 안 된다.

이 경계는 API 응답의 "제출됨"과 consensus의 "확정됨"을 구분한다. 현재 smoke는 health,
status, markets, deposit, account와 WebSocket block 경로가 실제 single node에서 연결되는지
확인하지만, 아래의 durable persistence와 authenticated transaction envelope가 완성됐다는
뜻은 아니다.

## 3. Local 실행 구성과 정확한 명령

### Single node

저장소 루트에서 다음 launcher를 실행한다.

```bash
./scripts/local-node
```

launcher는 repo root를 찾아 `MODE=dev cargo run --locked --bin hl-node`를 실행하고,
`config/local/single-genesis.json`과 `config/local/host-single/node.json`을 사용한다.
single consensus listen은 `127.0.0.1:9100`, API는 `127.0.0.1:8080`이다. launcher는
뒤의 public validator0 fixture seed를 환경변수로만 주입한다.

web package의 실제 `package.json` script는 `"dev": "next dev --turbopack"`이다. 별도
터미널에서 다음을 실행한다.

```bash
cd web && bun run dev
```

### Host four-node fixture

공통 `config/local/genesis.json`과 다음 process-local files를 사용한다.

| node | consensus listen | API listen | BLS env |
| --- | --- | --- | --- |
| 0 | `127.0.0.1:9101` | `127.0.0.1:8180` | `HL_LOCAL_BLS_SEED_1` |
| 1 | `127.0.0.1:9102` | `127.0.0.1:8181` | `HL_LOCAL_BLS_SEED_2` |
| 2 | `127.0.0.1:9103` | `127.0.0.1:8182` | `HL_LOCAL_BLS_SEED_3` |
| 3 | `127.0.0.1:9104` | `127.0.0.1:8183` | `HL_LOCAL_BLS_SEED_4` |

네 터미널에서 각각 실행한다.

```bash
MODE=dev HL_LOCAL_BLS_SEED_1=01000000000000000000000000000000000000000000000000000000000000be \
  cargo run --locked --bin hl-node -- --genesis config/local/genesis.json --config config/local/host-4/node0.json --blocks 3 --peer-wait-ms 5000

MODE=dev HL_LOCAL_BLS_SEED_2=02000000000000000000000000000000000000000000000000000000000000be \
  cargo run --locked --bin hl-node -- --genesis config/local/genesis.json --config config/local/host-4/node1.json --blocks 3 --peer-wait-ms 5000

MODE=dev HL_LOCAL_BLS_SEED_3=03000000000000000000000000000000000000000000000000000000000000be \
  cargo run --locked --bin hl-node -- --genesis config/local/genesis.json --config config/local/host-4/node2.json --blocks 3 --peer-wait-ms 5000

MODE=dev HL_LOCAL_BLS_SEED_4=04000000000000000000000000000000000000000000000000000000000000be \
  cargo run --locked --bin hl-node -- --genesis config/local/genesis.json --config config/local/host-4/node3.json --blocks 3 --peer-wait-ms 5000
```

Docker fixture의 consensus는 컨테이너 내부 `0.0.0.0:9000`, API는 `0.0.0.0:8080`이며
API host mapping은 `18080..18083`이다. 실행 명령은 다음과 같다.

```bash
docker compose -f docker-compose.validator4.yml up --build
```

## 4. BLS seed의 의미와 fixture 경고

각 node file에는 seed 값이 아니라 seed를 담은 환경변수 이름만 기록한다. local fixture
seed는 32-byte 값의 hex 표현이며, validator index를 첫 byte(`01`..`04`)로 표시하고
마지막 byte는 `0xbe`, 나머지는 0이다. `BlsSecretKey::from_seed(seed).public_key()`로
genesis의 validator public key를 재현한다. 이 값은 공개된 deterministic development
fixture일 뿐 secret이 아니며, production/testnet key custody에 재사용하면 안 된다.

전체 seed/public-key 표는 [config/local README](../../config/local/README.md)에 있다.
저장소에 `.env`나 seed file을 만들지 않고, launcher/Compose environment에서만 주입한다.

## 5. 통합으로 제거된 standalone 경로

- `hl-server`의 local fake consensus loop와 별도 REST/WS process ownership을 제거했다.
- server 경로의 empty-payload heartbeat 및 direct `AppState` mutation을 canonical commit으로
  사용하지 않는다.
- 외부 oracle fetcher와 market-maker loop가 consensus 밖에서 price/order/balance 상태를
  확정하는 경로를 제거·비활성화했다. oracle update와 market action은 향후 signed,
  ordered consensus transaction이어야 한다.
- Cargo의 standalone `hl-server` bin entry는 삭제했다. API/WS는 `hl-node`에 속하며,
  `multinode`는 production daemon이 아닌 test harness다.

## 6. 현재 검증 checkpoint

아래는 root 통합 검증 결과다.

| 검증 | 결과 |
| --- | --- |
| `cargo check --locked --bin hl-node` | pass |
| canonical runtime tests | 5 passed |
| single live HTTP: health/status/markets/deposit/account | pass |
| single live WebSocket block stream | pass |
| `cargo test --locked --all-targets` | 574 passed, 0 failed, 0 ignored |
| `cargo test --locked --doc` | 6 passed, 0 failed, 0 ignored |
| `cargo check --locked --all-features --all-targets` | pass |
| Docker Compose release build/live 4-node run | 4 nodes committed the same height 3/hash and exited 0 |
| `cd web && bun install --frozen-lockfile && bun run build` | pass |

이 checkpoint는 local integration smoke다. 특히 deposit은 dev simulation이며 Ethereum
deposit proof가 아니고, WS 연결 성공은 durable finalized-event log가 구현됐다는 뜻이 아니다.

## 7. 남은 한계

현재 runtime은 다음 제약을 명시적으로 가진다.

- `MODE=dev`만 허용한다.
- consensus/application store가 in-memory이고 durable atomic commit, restart/replay,
  crash recovery와 automatic restart contract가 없다.
- `chain_id`/genesis domain이 consensus signing context에 cryptographically bound되지
  않았다.
- transaction이 canonical, consensus-authenticated envelope로 peer gossip되고 모든
  validator에서 같은 bytes/domain/signature/nonce를 재검증하는 경로가 미완료다.
- account nonce, expiry/validity window, fee/resource bounds와 replay-resistant admission이
  완성되지 않았다.
- signed order API의 nonce 예약과 일부 dev-only admin mutation은 아직 consensus
  transaction envelope 안으로 완전히 이동하지 않았다. 또한 transaction gossip이 없으므로
  host/Docker 4-node fixture에서는 API write를 production transaction 경로로 간주하면 안 된다.
- expected committee/context와 certificate를 검증하는 canonical ActiveSync가 미완료다.
- BLS key ownership을 확인하는 proof-of-possession(PoP) 및 registration/rotation이 없다.
- gossip semantic admission이 mark-seen/deliver/relay보다 먼저 끝나는지, malformed
  payload가 vote 전에 거절되는지에 대한 production-grade 경계가 남아 있다.
- validator-set transition, historical committee registry, epoch transition과 관련
  certificate가 없으며 현재 epoch 0 curated committee만 사용한다.
- WAN fault, byzantine/fault injection, rolling restart, long-run Docker/host evidence와
  independent consensus/crypto/accounting review가 없다.

그러므로 이 통합은 local canonical runtime checkpoint이지 mainnet readiness가 아니다.

## 8. 다음 계획: 우선순위

1. **P0 — signed transaction envelope와 admission:** protocol/domain, chain/genesis
   digest, signer/signature, exact payload bytes, nonce, expiry, fee/resource bounds를
   versioned envelope로 고정한다. API에서 검증한 동일 bytes를 mempool과 consensus gossip에
   전달하고, 모든 validator가 proposal vote 전에 재검증한다.
2. **P0 — consensus domain과 key ownership:** chain/genesis domain을
   `ConsensusContext`와 signing bytes에 결합하고, BLS PoP-backed registration/key
   rotation과 완전한 context/app-hash equivocation evidence를 구현한다.
3. **P0 — durable state와 restart:** finalized block/payload/state/receipt roots, QC,
   safety/voted-view/pacemaker state를 atomic durable commit으로 저장하고 crash,
   restart, snapshot/replay에서 마지막 finalized state와 double-vote 방지를 검증한다.
4. **P0 — canonical ActiveSync와 gossip gate:** expected context/committee-bound
   certificate를 검증하는 active sync를 구현하고 semantic admission을
   mark-seen/deliver/relay보다 앞세운다. 완성 전에는 gossip/HTTP sync를 dev-only로 유지한다.
5. **P0 — committee transition:** historical committee, transition certificate, epoch
   activation, validator entry/exit와 stake/PoP rules를 검증하기 전까지 epoch transition을
   활성화하지 않는다.
6. **P1 — curated testnet gate:** 장기 multi-host 실행, fault injection, rolling restart,
   deterministic replay, external review, load/recovery evidence와 운영 runbook을 만든다.
7. **P2 — launch candidate:** bridge proof/accounting, key custody, reproducible builds,
   public bug bounty와 독립 검토가 끝난 뒤에만 별도 launch decision을 한다.

각 단계가 완료되기 전까지 공식 상태는 **not mainnet ready**다.
