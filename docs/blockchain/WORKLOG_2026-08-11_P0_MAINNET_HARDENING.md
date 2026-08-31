# P0 메인넷 하드닝 작업 로그

> **현재 상태: NOT MAINNET READY.**
> 이번 작업은 로컬 canonical runtime의 안전 경계를 넓힌 것이다. MODE=dev만 허용되며,
> 실자금 보관·Ethereum 자산 브리지·공개 validator 운영을 승인하지 않는다.

## 1. 작업 목적과 범위

이 문서는 2026-08-11 P0 구현 결과를 기록한다. 목표는 `hl-server`와 `hl-node`를 합친
단일 런타임 위에서, 서로 다른 chain/genesis의 replay, 위조된 합의 메시지, 서명 없는
사용자 action, crash 이후 safety state 손실을 줄이는 것이었다.

이번 P0는 **epoch 0의 curated, stake-weighted committee**만 다룬다. 다음 epoch 전환과
permissionless PoS는 아직 구현하지 않았고, 이 문서 후반의 단계별 계획으로 남긴다.

## 2. 구현 결과

### 2.1 Chain/genesis domain binding

`ConsensusContext`는 이제 `(epoch, committee_hash, genesis_hash)`를 함께 가진다.
`genesis_hash`는 chain ID, epoch, view timeout, canonical committee를 versioned
preimage로 해시한 domain이다. Block, proposal, vote, QC, timeout/TC, ViewChange/VCC,
NewView, safety state, persistence/recovery, equivocation evidence, sync object가 같은
context를 검증한다.

따라서 같은 block/signature bytes를 다른 chain ID, genesis 또는 committee에 재사용하는
경로는 context mismatch로 거부된다. 이 binding은 static epoch 0 범위이며, historical
committee를 이용한 다음 epoch 검증을 대신하지 않는다.

### 2.2 EIP-712 canonical transaction envelope

사용자 action의 canonical wire object로 versioned `SignedEnvelope`를 도입했다.

- EIP-712 v1 domain: `HyperLicked`, version `1`, chain/genesis domain을 `bytes32 salt`로 사용
- typed transaction에 signer, nonce, validity window, `keccak` action hash를 포함
- action hash는 고정 `HYPERLICKED-ACTION-V1` tag와 canonical bincode action bytes에서 계산
- API ingress, mempool, block execution, 모든 validator가 같은 envelope의 domain, nonce,
  시간 범위, signer/action binding, secp256k1 signature를 재검증
- high-s/invalid recovery-id, malformed length, oversized envelope/action을 거부
- valid action이 실행 중 실패해도 nonce/replay 규칙과 상태 rollback을 deterministic하게 유지
- unsigned `System(Transaction)`은 protocol-owned/local fixture 용도이며 production user
  ingress가 사용할 수 없다. Dev envelope는 dev application에서만 허용된다.

이제 API가 검증한 내부 `Transaction`만으로 합의 payload를 구성하지 않는다. 단, frontend가
직접 EIP-712 action hash를 만들려면 canonical bincode encoder/signing-data API가 추가로
필요하므로 production wallet integration은 launch blocker로 남긴다.

### 2.3 BLS proof-of-possession, schema v2, next-epoch key rotation

genesis/node loader의 schema를 v2로 고정하고 각 validator에 96-byte BLS
proof-of-possession(PoP)을 요구한다. PoP는 chain/genesis domain, node ID, BLS public key에
분리된 domain으로 서명되며 listener를 열기 전에 검증한다. malformed key/PoP와 duplicate
BLS public key는 거부된다.

`RegisterValidator`는 PoP를 포함하고, `RotateValidatorKey`는 현재 validator identity를
유지한 채 새 key와 PoP를 다음 epoch 후보에 기록한다. active epoch-0 committee를 즉시
바꾸지 않으며, transition certificate와 historical committee 검증이 없으면 activation하지
않는다.

### 2.4 RocksDB durable commit과 restart safety

canonical `hl-node`는 RocksDB를 사용한다. finalized block, height index, committed
metadata, `ConsensusState`(high/locked QC, voted views, context 등)를 하나의 synced
write batch로 기록하고, durable commit 뒤에만 committed application/API 결과를 공개한다.

validator는 follower vote와 leader self-vote를 네트워크 전송 또는 로컬 quorum 집계에
넣기 전에 vote intent와 safety state를 synced storage에 먼저 기록한다. 이 기록이 실패하면
해당 vote를 내보내지 않고 fail-stop한다. 또한 committed/speculative replay 모두에서
canonical committee의 BLS 서명과 stake-weighted quorum을 application 실행보다 먼저
검증한다.

finalized commit도 application payload를 read-only preflight한 뒤 RocksDB synced atomic
commit을 먼저 수행한다. 그 다음에만 canonical application state와 WebSocket event를
publish하고 runner/sync head를 갱신한다. storage failure는 application/API/event/head를
변경하지 않은 채 fail-stop하며, durable write 이후 application failure는 재시작 replay가
복구 경계가 된다.

재시작 시 genesis부터 finalized chain을 replay하며 block/context/parent/QC/app hash를
검증한다. 별도로 durable된 high/locked QC가 가리키는 speculative branch도 복원하되,
committed state를 오염시키거나 query에 finalized로 노출하지 않는다. 이 경계가 완전한
state snapshot/receipt root 검증을 구현했다는 뜻은 아니다.

live QC와 proposal ancestry, canonical application 실행/commit, RocksDB atomic commit은
모두 현재 durable committed head의 정확한 hash까지 이어지는 parent chain을 요구한다.
높이만 같은 유효 QC fork는 high-QC 선택, application 실행, commit, storage mutation 전에
거부된다.

### 2.5 Gossip pre-relay admission과 Propose BLS

gossip envelope는 payload에서 message ID를 재계산하고 TTL, nested gossip, sync/snapshot
message 전파를 먼저 검사한다. semantic admission을 통과하기 전에는 seen-cache 기록,
deliver, relay가 일어나지 않는다.

trusted epoch-0 context/committee를 `NetworkConfig`에 주입해 Vote, Timeout, ViewChange,
Propose, Prepare, NewView의 origin, leader, context, BLS signature, QC/VCC, stake-weighted
quorum을 검증한다. leader proposal은 최종 block hash(및 app hash가 반영된 block hash)에
대한 proposer BLS signature를 포함한다. 검증된 logical origin만 consensus runner로
전달하고 relay한다.

### 2.6 Trusted ActiveSync

ActiveSync는 state machine이나 import authority가 아니라 **verified-download-only**
client가 되었다. caller가 제공한 trusted anchor와 context/committee를 기준으로 peer가
주장한 block hash를 재구성하고, 순차 height, parent, proposer/leader, genesis domain,
역할별 QC와 strict weighted quorum을 검증한다.

peer 응답은 application state, snapshot, RocksDB, committed head를 직접 바꿀 수 없다.
검증된 `Vec<Block>`만 반환하며 실제 import는 일반 consensus/store 경로가 맡는다. 검증된
block import, snapshot manifest, full state root 및 receipt/event root는 별도 launch gate다.
HTTP sync body는 chunked 응답을 포함해 32 MiB로 제한하며, block/range API는 durable
committed height를 넘는 stale 또는 speculative height-index entry를 공개하지 않는다.

### 2.7 Equivocation evidence와 산술 overflow

equivocation detector/proof는 epoch, committee hash, genesis domain과 양쪽 vote의
`block_hash`, `app_hash`, signature를 보존한다. 현재 protocol phase는 static epoch 0이며,
genesis staking operator mapping과 historical committee registry가 없으므로 이 자료가
permissionless slashing/epoch transition을 완성한 것은 아니다.

API financial calculation의 intermediate arithmetic overflow 경로도 수정했다. 입력/자금
정책과 consensus execution의 전역 안전성을 이 수정 하나로 보증하는 것은 아니다.

## 3. 로컬 실행 checkpoint

일반적인 single-node smoke는 다음 한 줄이면 된다.

```bash
./scripts/local-node
```

위 launcher가 `MODE=dev`, single genesis/config, validator-0 공개 fixture seed를
자동으로 선택한다. 사용자가 `--bin hl-node`, `--locked`, `--genesis`, `--config`를 직접
입력할 필요는 없다. 웹 클라이언트는 별도 터미널에서 실행한다.

```bash
cd web && bun run dev
```

RocksDB 재시작 smoke 예:

```bash
hl_restart_dir="$(mktemp -d)"
./scripts/local-node --blocks 3 --data-dir "$hl_restart_dir"
./scripts/local-node --blocks 5 --data-dir "$hl_restart_dir"
```

`--data-dir`를 생략하면 `.hyperlicked/data/<genesis-domain>/<node-id>`가 사용된다.
multi-node에서는 validator마다 별도 directory를 사용해야 한다. `data-dir`는 chain domain과
node identity를 섞어 쓰지 않도록 하는 로컬 persistence 경계다.

Docker 4-validator fixture는 authenticated TCP, `GOSSIP_ENABLED=true`, validator별 named
RocksDB volume을 사용한다.

```bash
docker compose -f docker-compose.validator4.yml build --pull
docker compose -f docker-compose.validator4.yml up --build
docker compose -f docker-compose.validator4.yml down
```

### BLS seed란 무엇인가

BLS seed는 validator BLS secret key를 결정적으로 만들기 위한 32-byte secret input이다.
public key와 PoP는 seed에서 유도하지만 seed 자체는 genesis JSON에 저장하지 않는다.
`HL_LOCAL_BLS_SEED_1..4`와 config/local 값은 재현 가능한 공개 dev fixture일 뿐이며,
production key나 custody 방법이 아니다. 운영에서는 HSM/외부 key custody와 rotation/backup
절차가 별도로 필요하다.

## 4. 검증 checkpoint

최종 full regression과 실제 runtime smoke에서 확인한 결과는 다음과 같다.

- `hl-node` single-node, host 4-validator, Docker 4-validator 경로가 같은 canonical
  runtime/loader를 사용한다.
- single-node와 multi-node가 동일 chain/genesis domain 및 committee context를 사용한다.
- Docker fixture는 gossip을 켜고 validator별 named RocksDB volume을 마운트한다.
- malformed context/signature/QC/PoP/envelope 및 untrusted ActiveSync response를 거부하는
  회귀 테스트가 P0 범위에 포함된다.
- 재시작 경로는 finalized replay와 speculative high/locked QC 복원을 별도로 검증한다.

| 항목 | 최종 검증 결과 |
| --- | --- |
| `cargo test --all-targets --all-features --locked --no-fail-fast --quiet` | 626 passed, 0 failed, 0 ignored |
| `cargo test --doc --all-features --locked --quiet` | 6 passed, 0 failed, 0 ignored |
| `cargo check --all-targets --all-features --locked` | 성공 |
| single-node restart | 같은 RocksDB directory에서 height 3 종료 → height 3 복구 → height 5 종료 성공 |
| `cd web && bun run build` | 성공 |
| Docker 4-validator/gossip/persistence | 네 노드 모두 인증+gossip, 동일 hash로 height 3 종료 → 같은 named volume에서 height 3 복구 → 동일 hash로 height 5 종료, 모두 exit 0 |
| ignored Rust tests | 0개 |

## 5. 아직 구현하지 않은 계획

### Curated PoS → permissionless PoS

1. static epoch-0 committee를 signed genesis와 on-chain validator/operator mapping으로
   재현한다.
2. finalized state에서 next committee 후보, self-bond/delegation, unbonding, rewards,
   commission, jailing/slashing을 deterministic하게 계산한다.
3. old committee가 검증하는 epoch transition certificate, historical committee registry,
   key rotation/entry/exit 및 restart/sync proof를 추가한다.
4. public testnet에서 Byzantine/fault/entry-exit/economic attack을 통과한 뒤에만
   permissionless selection을 활성화한다.

Acceptance criteria는 모든 validator가 같은 historical committee와 voting power를 독립
재구성하고, transition certificate 없는 epoch activation 및 stale committee proof를
거부하며, restart/ActiveSync에서도 동일한 history를 유지하는 것이다.

### Ethereum bridge

현재 simulated balance만 사용한다. 향후 bridge는 Ethereum deposit proof/finality/reorg,
replay ID, accounting conservation, pause/recovery, relayer key custody를 검증하는
별도 execution path로 구현한다. relayer나 환경변수가 balance를 직접 쓰게 하지 않고,
검증된 bridge transaction만 canonical state를 변경하게 한다. testnet proof와 독립 보안
review 전에는 실자산을 연결하지 않는다.

### Separate EVM domain

EVM은 현재 구현하지 않는다. 필요할 때 같은 consensus 아래 별도 execution domain/VM
version, gas/resource limit, state root, receipt/event root, deterministic host interface를
정의한다. perp/orderbook state와 EVM state를 암묵적으로 공유하지 않고, JSON-RPC 표면만
추가하는 것으로 안전성 경계를 주장하지 않는다.

## 6. Mainnet launch blockers

현재 남은 blocker는 다음과 같다.

- verified epoch transition certificate, historical committee registry, genesis staking
  operator mapping 및 permissionless 전환 검증
- verified block import, snapshot manifest/proof, full state root, receipt/event root
- legacy in-memory `Engine` 제거 또는 test-only 비공개화와 sync API 총 응답 byte budget
- Ethereum bridge proof/finality/reorg/replay/accounting 및 pause/recovery
- validator key custody, genesis ceremony, upgrades/reproducible builds, telemetry,
  alerting, incident response, chaos/long-run testing, independent consensus/crypto/
  accounting/security audit

위 항목과 독립 review가 끝날 때까지 공식 상태는 **NOT MAINNET READY**다.
