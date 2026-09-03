# Canonical Runtime 및 향후 계획

> **현재 상태: NOT MAINNET READY.**
> 현재 경로는 `MODE=dev` 로컬 검증용이다. smoke 결과는 메인넷 운영, 실자금 보관,
> Ethereum bridge, 독립 보안 감사 또는 launch approval을 의미하지 않는다.

상세 P0 결과는 [P0 메인넷 하드닝 작업 로그](WORKLOG_2026-08-11_P0_MAINNET_HARDENING.md),
현재 gate는 [메인넷 준비도 감사](MAINNET_READINESS.md), 로컬 명령은
[config/local README](../../config/local/README.md)를 기준으로 한다.

## 1. 실행 모델

| 경로 | 현재 의미 | 판단 |
| --- | --- | --- |
| `hl-node` | genesis/node loader, BLS seed, authenticated TCP, gossip admission, consensus, RocksDB recovery, canonical API/WS를 한 process가 소유한다. 같은 binary로 N=1 또는 N=4를 실행한다. | canonical local runtime. 여전히 `MODE=dev` only이며 mainnet runtime이 아니다. |
| `multinode` | 고정 local fixture로 TCP consensus/BLS 경로를 exercise하는 개발 harness다. `hl-node`와 동일한 `CanonicalAppHook` 경계에서 speculative execution, Commitment v2, schema-v3 full-state root preflight를 수행한다. | 실행 의미는 통일했지만 테스트 보조 경로이며 배포 profile이 아니다. |

Cosmos 계열과 같은 방향으로 **하나의 node runtime**을 N=1 single-validator network와
N-validator network에 공통 적용한다. N=1은 validator가 한 명인 별도 network로 뜨며,
local 기능·replay·API를 확인하는 데 사용할 수 있다. N=4는 `f=1` curated BFT fixture다.
`hl-server`의 별도 consensus/API ownership은 제거했고 API/WS는 `hl-node`가 관리하는
canonical state만 읽는다. `hl-mm`은 별도 dev/showcase client/service로서 state나
consensus를 소유하지 않고, 선택한 한 validator API에 canonical signed transaction을
제출한다.

## 2. P0에서 고정한 안전 경계

### 2.1 Chain/genesis context

`ConsensusContext = (epoch, committee_hash, genesis_hash)`다. genesis domain V3는 chain ID,
epoch, view timeout, canonical committee, block-hash protocol V5, state-root schema v3와
Commitment v2 schema/version을 versioned preimage에 넣어 계산한다. Block,
proposal, vote, QC, timeout/TC, ViewChange/VCC/NewView, safety state, persistence/recovery,
equivocation, sync가 이 context를 carry/verify한다.

현재 protocol phase는 static epoch 0이다. 다른 chain/genesis/committee의 replay는 거부하지만,
historical committee를 이용한 동적 epoch 검증은 아직 없다.

### 2.2 User transaction

`SignedEnvelope`가 canonical user payload다. EIP-712 v1 domain은 `HyperLicked`/`1`이며
chain/genesis domain을 `bytes32 salt`로 사용한다. typed data는 signer, nonce, validity
window, fixed-tagged canonical action hash를 포함한다.

API, mempool, block execution, validator replay가 동일 envelope의 signature, domain, nonce,
시간, action owner, size를 다시 검증한다. high-s/invalid recovery-id 및 malformed/oversized
payload는 거부한다. `System(Transaction)`은 protocol-owned/local fixture 전용이며 일반
사용자 unsigned action 경로가 아니다.

Production browser signer가 action hash를 만들려면 Rust와 같은 canonical bincode/signing-data
encoder/API가 필요하다. 이 integration은 메인넷 launch 전 별도 작업이다.

### 2.3 Validator key ownership

genesis schema v2는 각 validator의 BLS public key와 96-byte proof-of-possession(PoP)을
요구한다. PoP는 chain domain, node ID, public key에 분리된 domain으로 묶으며 listener
시작 전에 검증한다. duplicate key와 잘못된 PoP는 거부한다.

`RotateValidatorKey`는 현재 identity를 유지한 채 next-epoch 후보 key/PoP를 기록한다.
transition certificate가 없으면 active epoch-0 committee를 바꾸지 않는다.

### 2.4 Persistence/restart

RocksDB synced write batch에 finalized block, height index, committed metadata와
`ConsensusState`(context, high/locked QC, voted views 등)를 함께 기록한다. 재시작 시
genesis부터 finalized chain을 replay하고 context/parent/QC/app hash를 확인한다. persisted
high/locked QC가 가리키는 speculative branch는 별도로 복원하지만 committed result로
노출하지 않는다.

각 finalized block에는 Commitment v2와 schema-v3 full-state root도 같은 synced batch로
기록한다. state-root row는 raw 32-byte hash가 아니라 `(u16 schema_version, 32-byte root)`의
고정 canonical record다. 재시작 replay는 저장된 root를 fresh full-state recomputation과
비교하며, raw legacy row, 미지원 version, 잘못된 길이와 root 불일치를 fail closed한다.

block protocol V5에서 `Block::app_hash`는 schema-v3 root, `Block::commitment_root`는
Commitment v2 receipt/event combined root다. block hash, proposer signature, Vote와 QC가 둘을
함께 인증한다. leader/follower/observer는 fresh preflight가 header와 일치하기 전에는
proposal/vote/commit을 진행하지 않고 storage도 non-genesis artifact/root mismatch를 거부한다.
Verified snapshot/import와 개별 receipt/event inclusion proof까지 구현됐다는 뜻은 아니다.
또한 canonical restart가 genesis부터 replay하므로 verified snapshot anchor recovery 전에는
block pruning을 validator 운영에서 활성화하면 안 된다.

### 2.5 Network admission

gossip message ID/TTL/nested envelope/sync message를 먼저 검사하고, semantic admission
전에는 seen-cache/deliver/relay하지 않는다. trusted epoch-0 context/committee로 Vote,
Timeout, ViewChange, Propose, Prepare, NewView의 origin, leader, BLS signature, QC/VCC와
stake-weighted quorum을 검증한다. Propose에는 최종 block hash에 대한 proposer BLS signature가
필요하다.

ActiveSync는 verified-download-only다. trusted anchor/context/committee로 block hash,
height, parent, proposer/leader, QC와 quorum을 검증하고 `Vec<Block>`만 반환한다. peer 응답이
AppState, snapshot, RocksDB, committed head를 직접 변경할 수 없다.

### 2.6 Equivocation/overflow

equivocation evidence는 epoch, committee hash, genesis domain, 양쪽 block hash/app hash와
signature를 보존한다. 이는 static epoch-0 evidence boundary다. genesis staking mapping과
historical committee registry 없이는 permissionless slashing/transition이 완성되지 않는다.
API financial intermediate arithmetic overflow도 방어했지만, 경제/회계 독립 review를 대체하지
않는다.

## 3. 로컬 실행 계약

기능 쇼케이스는 다음 세 터미널에서 실행한다.

```bash
# Terminal 1 — canonical local validator/API
./scripts/local-node

# Terminal 2 — optional separate dev/showcase market maker
./scripts/local-mm

# Terminal 3 — web client
cd web
bun install  # first run, or after dependency changes
bun run dev
```

launcher가 `MODE=dev`, `hl-node`, `--locked`, single genesis/config, 공개 validator-0
fixture seed를 선택한다. `--bin hl-node`, `--locked`, `--genesis`, `--config`를 직접 입력할
필요가 없다. 시작 시 보이는 `ready ... committed_height=0`은 genesis 복구와 listener 준비가
끝났다는 뜻이다. 오류나 정지 상태가 아니며, 그 직후 N=1 consensus가 빈 블록을 포함해
블록을 자동 생성한다. launcher의 기본 로그 수준이 `warn`이라 round별 진행이 보이지 않을
뿐이다.

```bash
# 다른 터미널에서 committed height 증가 확인 (Ctrl-C로 종료)
while true; do curl -s http://127.0.0.1:8080/api/v1/chain/status; echo; sleep 1; done

# round별 로그가 필요하면 이렇게 시작
RUST_LOG=info ./scripts/local-node
```

`hl-mm`은 deterministic public dev secp256k1 signer fixture와 simulated balance만 사용하는
별도 서비스다. 기본 target은 `http://127.0.0.1:8080`이며, 한 MM 인스턴스는 한 validator
API만 대상으로 한다. Docker N=4에서는 validator0 host API에 하나만 연결한다.

```bash
./scripts/local-mm --node-url http://127.0.0.1:18080
```

fixture key, real funds, production 사용은 금지한다. `hl-mm`은 `hl-node` 내부 loop가 아니며
`MM_ENABLED` 환경변수로 시작되지 않는다.

web은 별도 process다. 위 Terminal 3에서 실행한다.

```bash
cd web
bun install  # first run, or after dependency changes
bun run dev
```

RocksDB restart는 같은 data directory를 넘긴다.

```bash
hl_restart_dir="$(mktemp -d)"
./scripts/local-node --blocks 3 --data-dir "$hl_restart_dir"
./scripts/local-node --blocks 5 --data-dir "$hl_restart_dir"
```

`--data-dir`를 생략하면 `.hyperlicked/data/<genesis-domain>/<node-id>`다. BLS seed는
validator secret key를 재현하는 32-byte secret input이며, `config/local`의 값은 공개
dev fixture다. JSON/config에 seed를 저장하지 않으며 production에서는 HSM/key custody를
사용해야 한다.

Docker 4-validator는 `GOSSIP_ENABLED=true`와 validator별 named RocksDB volume을 사용한다.
현재 Compose fixture는 각 노드를 `--blocks 3`으로 실행하므로 height 3을 commit한 뒤 정상
종료하는 유한 smoke test다. MM showcase를 붙일 때는 Docker validator마다 실행하지 말고
`http://127.0.0.1:18080`에 연결하는 MM 하나만 사용한다.

```bash
docker compose -f docker-compose.validator4.yml build --pull
docker compose -f docker-compose.validator4.yml up --build
docker compose -f docker-compose.validator4.yml down
```

## 4. 계획 A — curated PoS에서 permissionless PoS로

현재는 signed genesis로 고정한 curated, stake-weighted epoch-0 committee다. 다음 순서로
확장한다.

1. genesis staking/operator mapping, validator identity, voting power, BLS key/PoP와 key
   custody/rotation policy를 replay 가능한 chain data로 고정한다.
2. finalized state에서 self-bond, delegation, unbonding, rewards, commission,
   jailing/slashing과 deterministic next-committee candidate를 계산한다.
3. old committee가 서명하는 epoch transition certificate와 historical committee registry를
   추가한다. entry/exit/rotation, evidence retention, restart/sync 검증도 같은 history를
   사용한다.
4. public testnet에서 Byzantine/fault/entry-exit/economic attack을 통과한 뒤에만
   permissionless selection을 활성화한다.

Acceptance criteria:

- 모든 validator가 동일 historical committee/voting power를 독립 재구성한다.
- transition certificate 없는 activation, stale committee proof, context mismatch를 거부한다.
- restart, verified sync, rollback/fault injection 후에도 old/new committee 경계가 동일하다.
- slashing evidence가 올바른 operator/stake에만 적용되고 double-count되지 않는다.

## 5. 계획 B — verified sync/snapshot과 indexer proof serving

schema-v3 state root와 Commitment v2 receipt/event root의 genesis-wide V5 consensus
activation은 완료됐다. 다음 단계에서는 verified block import를 정상 execution/store 경로에
연결한다. snapshot은 manifest, chunk hash, state root, commitment root와 trusted finalized
anchor를 가져야 하며, 모든 validator가 동일한 root를 재계산한 뒤에만 commit한다. ActiveSync가
peer가 제공한 state를 검증 없이 직접 import하지 않도록 유지한다. Indexer bounded-range 및
Merkle inclusion-proof API는 별도 tranche로 둔다.

Acceptance criteria는 forged block/QC, skipped height, wrong parent, rogue key, insufficient
weighted quorum, snapshot chunk corruption, state/receipt root mismatch를 모두 거부하고,
중단 후 재개가 동일한 finalized state를 복원하는 것이다.

## 6. 계획 C — Ethereum bridge

현재 local balance는 simulated balance다. production bridge는 구현하지 않았다. 향후
relayer는 deposit proof/finality/reorg 증거를 제출하고, 각 validator가 독립 검증한
bridge transaction만 balance를 credit한다. withdrawal은 finalized outbound intent로
생성하며, completion은 검증 가능한 inbound proof/attestation으로 기록한다.

replay/message ID, accounting conservation, confirmation/finality, rate limit,
pause/recovery, relayer key custody, independent proof review가 acceptance criteria다.
환경변수는 RPC URL/secret 같은 operational input만 제공하고, chain rule/contract address/
decimals/limits는 signed genesis/on-chain state에 둔다. 별도 Ethereum testnet 검증 전에는
실자산을 연결하지 않는다.

## 7. 계획 D — 별도 EVM execution domain

EVM은 지금 구현하지 않는다. 향후 필요하면 같은 consensus 아래 별도 domain/VM version으로
추가한다. EVM transaction/mempool, gas/resource schedule, deterministic host interface,
state root, receipt/event root, JSON-RPC compatibility를 명시하고 block이 versioned result를
commit한다. EVM state와 perp/orderbook state를 암묵적으로 공유하지 않는다. JSON-RPC surface만
추가하는 것으로 HyperEVM과 동등한 보안/호환성을 주장하지 않는다.

## 8. launch 전 운영/검증 gate

- verified epoch transition, historical committee, genesis staking mapping, permissionless
  economics
- verified block import/snapshot과 indexer artifact/range/inclusion-proof API
- bridge proof/finality/reorg/replay/accounting/pause/recovery
- validator key custody, genesis ceremony, upgrades/reproducible builds, telemetry,
  alerting, incident response, backups
- long-run/WAN/chaos/Byzantine/fault-injection test와 독립 consensus/crypto/accounting/
  security audit

위 gate가 수용되기 전까지 공식 상태는 계속 **NOT MAINNET READY**다.
