# 2026-08-10 메인넷 하드닝 작업 로그

> **Historical snapshot.** This log predates the 2026-08-11 P0 tranche. Statements below that
> describe memory storage, an unbound chain domain, gossip-off, unsigned payloads, or unsafe
> ActiveSync are preserved as the earlier audit baseline and are not the current implementation
> status. See [P0 메인넷 하드닝 작업 로그](WORKLOG_2026-08-11_P0_MAINNET_HARDENING.md) and
> [메인넷 준비도 감사](MAINNET_READINESS.md) for the current status.

> **현재 상태: NOT MAINNET READY.**
> 이 문서는 개발 트리에서 수행한 하드닝 트랜치의 기록이며, 메인넷 출시 승인이나
> 독립 보안 감사 결과가 아니다. 실자금 보관, 실제 자산 브리지, 공개 검증자 운영에
> 사용해서는 안 된다.

관련 기준 문서: [메인넷 준비도 감사](MAINNET_READINESS.md), [블록체인 로드맵](ROADMAP.md),
[프로젝트 README](../../README.md), [로컬 runtime fixture 명령](../../config/local/README.md)

## 1. 상태 변화

### 트랜치 전

- 기능은 있었지만 하나의 합의 런타임이 아니었다. 서버의 로컬 합의 루프, 단일 노드
  엔진, TCP 멀티노드 데모가 서로 다른 블록 생성·실행·저장·네트워크 계약을 가졌다.
- 검증자 집합과 리더/쿼럼 계산이 구성 배열과 개수 기반 경로에 의존했고, BLS와 피어
  인증은 개발 설정과 런타임 경로에 따라 선택적으로 빠질 수 있었다.
- 블록 payload가 비어 있거나 잘못된 경우 노드별 로컬 mempool을 대신 사용할 수 있어,
  같은 블록을 받아도 노드가 서로 다른 트랜잭션을 실행할 위험이 있었다.
- API 서버와 개발용 입출금 경로가 합의 상태를 직접 변경할 수 있었고, `hl-server`는
  네트워크 HotStuff-2 검증자 런타임이 아니었다.

### 트랜치 후

이번 변경으로 합의 검증 경로의 최소 안전 경계를 좁혔다. canonical 위원회, 엄격한
가중 쿼럼, 인증된 BLS transport, ViewChange/QC 검증, 개발 전용 API, payload-only
실행, 고정 Rust/CI를 적용했다. 그러나 이는 **메인넷 준비 완료가 아니다**. 현재도
production canonical 런타임, 서명 트랜잭션 envelope, 완전한 durable recovery, 동적 epoch/위원회
전환 증명, 이더리움 브리지 검증이 남아 있다. 정적 epoch 0 `committee_hash` binding은
이번 후속 결과로 Block·Proposal·Vote·QC·Timeout/TC·VC/VCC/NewView·Safety·equivocation
detector/proof·persisted state·sync object context까지 적용했지만, 다음 epoch 전환은
여전히 비활성이다. Staking slashing evidence envelope의 context/app_hash 보존은 별도
P0로 남아 있다.

## 2. 아키텍처 감사: 합의 경로가 세 개다

메인넷 관점에서 다음 세 바이너리는 하나의 런타임 설정 변형이 아니라 서로 다른
런타임 계약이다.

| 바이너리 | 현재 동작 | 이번 트랜치에서의 판단 |
| --- | --- | --- |
| `hl-server` | REST/WebSocket 서버. validator 모드에서는 자체 `run_consensus_loop`, RPC 모드에서는 `ActiveSyncClient`를 실행한다. | 로컬 API/데모 서버다. 네트워크 HotStuff-2와 동일한 합의·QC·recovery 소유자가 아니며, 이제 `dev`에서만 실행되도록 막았다. |
| `hl-node` | shared genesis/node loader와 환경변수 BLS seed를 읽는 canonical runner다. 같은 바이너리로 N=1 또는 N=4를 실행하고, `--blocks`로 finite committed-height에서 종료한다. 현재 store는 memory다. | 이번 tranche에서 구성·피어·BLS 인증 경로를 하나로 묶었지만 `MODE=dev`만 허용한다. chain domain이 consensus context에 없고 durable recovery도 없어 production runtime은 아니다. |
| `multinode` | TCP `ConsensusRunner`와 BLS 피어 인증을 쓰는 3노드 데모다. | 이번 트랜치의 가중 위원회/인증 경로를 exercise하는 개발 harness일 뿐이다. loopback 주소, 결정적 개발 키, 메모리 저장소를 사용하므로 배포 프로필이 아니다. |

추가로 oracle fetcher나 API handler가 합의 상태를 직접 바꾸는 경로도 production
계약으로 인정하지 않는다. 외부 oracle 업데이트는 향후 서명되고 순서가 정해진
consensus transaction이어야 한다.

### Canonical runtime tranche

- `GenesisFile`과 node file을 shared loader가 함께 검증한다. genesis의 epoch-0 committee와
  node의 identity/listen/peer 설정은 파일에 두고, 원본 BLS seed만 `bls_secret_seed_env`가
  가리키는 환경변수에서 읽는다. seed를 JSON이나 이미지에 넣지 않는다.
- 동일 `hl-node` 바이너리로 N=1 single smoke와 N=4(`f=1`) local network를 실행한다.
  `view_timeout_ms`와 `--peer-wait-ms`가 finite local run의 진행·peer readiness를 제한한다.
- 호스트 fixture는 고유 loopback 포트를, Docker fixture는 `validator0..3:9000` service DNS를
  사용한다. Compose의 gossip은 현재 semantic admission blocker 때문에 꺼 둔다. 실행 명령은
  [config/local README](../../config/local/README.md)에 고정했다.
- TCP reconnect는 connection generation을 확인해 오래된 cleanup이 새 연결을 삭제하지
  않도록 한다. duplicate peer address는 설정 단계에서 거부한다.
- 후속 TCP hardening은 duplicate/self peer를 거부하고, lower-ID 쪽만 outbound dial을
  소유하도록 inbound를 제한한다. writer/reader 중 하나가 끝나면 양쪽을 함께 teardown하며,
  안정적으로 연결된 뒤에는 backoff를 초기화하고 불안정한 연결은 상한 5초까지 재시도한다.
- `ConsensusConfig`의 `Debug`는 BLS secret 존재 여부와 public-key 개수만 표시하며 seed
  bytes/hex와 전체 public-key bytes를 포맷하지 않는다.

이 tranche는 canonical local execution model을 만든 것이며 memory store, 미결합 chain ID,
dev-only 환경 때문에 mainnet runtime을 의미하지 않는다. Cosmos와의 비교도 동일 바이너리의
N=1/N-validator local model과 curated genesis에서 시작해, 감사된 epoch transition 이후
permissionless validator set으로 확장하는 계획에 한정한다.

## 3. 완료한 변경과 필요한 이유

### 3.1 Stake-weighted canonical committee

- `ConsensusConfig`의 validator, voting power, BLS key를 canonical `Committee`로
  정규화했다. 위원 순서를 `NodeId`로 정렬하고 중복 ID, 0 stake, 배열 길이 불일치,
  overflow를 거부하며 committee hash를 계산한다.
- 리더 선택도 canonical 정렬과 stake 가중 결정적 슬롯을 사용한다. 입력 배열 순서가
  proposer schedule을 바꾸지 않는다.
- 모든 live `ConsensusRunner`는 유효한 위원회와 각 위원의 BLS key를 확인하고,
  로컬 secret key가 등록된 public key와 일치하지 않으면 시작하지 않는다.

**필요한 이유:** 검증자 순서나 로컬 설정 배열이 달라도 모든 노드가 같은 위원회,
리더, voting power를 해석해야 한다. 다만 이 위원회는 아직 주로 configuration-backed
static committee다. stake 등록이 permissionless PoS나 epoch 전환을 완성했다는 뜻은
아니다.

### 3.2 Strict `> 2/3` weighted quorum

- QC, ViewChange certificate, timeout 관련 집계에서 위원회 구성원만 인정하고 signer
  중복을 거부한다.
- 판정식은 `3 * signer_voting_power > 2 * total_voting_power`이다. 정확히 2/3인
  경우는 통과시키지 않으며, 단순 validator 개수나 배열 위치로 stake를 복제할 수 없다.

**필요한 이유:** BFT 안전성의 기준을 개수 기반 임계치에서 실제 voting power에
고정하고, 경계값·중복 signer·비위원회 signer가 QC를 만들지 못하게 해야 한다.

### 3.3 BLS 서명, 키 일치, authenticated transport

- vote는 위원회에 등록된 BLS public key와 일치해야 하며, 서명과 공개키 형식 및
  aggregate 서명을 검증한다. QC aggregate는 동일한 `(view, block_hash, app_hash)`
  메시지를 서명한 키 집합으로만 검증한다.
- TCP handshake는 BLS로 peer identity를 인증한다. `testnet`/`mainnet`에서는 인증된
  peer와 usable committee key가 필수이며, local secret key와 committee key가
  다르면 실패한다.
- `multinode`의 키는 의도적으로 deterministic development fixture이고 loopback
  주소도 개발 전용이다. 운영 검증자 키로 재사용해서는 안 된다.

**필요한 이유:** 네트워크에서 validator를 사칭하거나 위조 vote/QC를 주입하는 것을
막고, transport peer identity와 합의 signer identity가 같은 주체인지 확인해야 한다.

### 3.4 ViewChange/QC fail-closed

- QC를 적용하기 전에 view, block hash, app hash, committee member, key, BLS aggregate,
  signer 중복과 strict weighted quorum을 모두 검사한다.
- ViewChange와 ViewChangeCertificate는 현재 view보다 앞서야 하고 미래 범위를 제한하며,
  각 서명과 embedded high QC, signer 집합 및 가중 쿼럼을 검증한다.
- 검증 실패 시 pacemaker, safety state, high/locked QC를 갱신하지 않는다. 동적 위원회
  갱신은 static epoch 0 context binding만 존재하며, transition certificate와 historical
  committee 검증이 마련될 때까지 적용하지 않는다.

**필요한 이유:** malformed/forged NewView나 QC가 안전성 상태를 먼저 오염시키면 이후의
정상 서명만으로 복구할 수 없다. 인증되지 않은 증명은 상태 변경 전에 폐기해야 한다.

### 3.5 개발 전용 API와 서버 경계

- `hl-server`는 `MODE`를 명시하지 않거나 `dev`가 아니면 시작하지 않는다. 이 바이너리는
  로컬 prototype이며 향후 통합 인증 합의 런타임을 대신하지 않는다.
- simulated `deposit`/`withdraw` 및 legacy mutation route는 `dev` router에만 mount한다.
  `testnet`/`mainnet`에서는 해당 route가 존재하지 않는다.
- API의 read/routing 계층은 남아 있지만, API handler·oracle task가 production 합의
  상태를 직접 mutate하는 계약은 인정하지 않는다.

**필요한 이유:** faucet과 simulated balance 생성/소각은 이더리움 입금 증거가 아니며,
prototype 서버가 실자산 경로로 오인되면 가짜 잔액·무권한 상태 변경이 발생한다.

### 3.6 Deterministic payload-only execution

- block execution은 committed block의 payload만 transaction source로 사용한다. 빈
  payload면 빈 실행을 하고, malformed payload도 local mempool로 fallback하지 않는다.
- local mempool은 block payload에 실제로 들어간 transaction만 제거한다. 각 노드가
  자신에게만 도착한 pending transaction을 임의로 실행할 수 없다.
- 현재 malformed payload는 결정적인 no-op로 관찰되지만, production proposal validation은
  이를 vote 전에 reject해야 한다. no-op가 launch 승인이나 payload 검증 완료를 뜻하지는 않는다.

**필요한 이유:** 같은 block에 대해 노드마다 mempool 내용·도착 순서가 다르다는 것은
합의 상태 divergence와 서로 다른 app hash를 만든다. payload를 합의 입력으로 고정해야
동일 block replay가 같은 state root로 수렴한다.

### 3.7 Rust toolchain pin과 CI

- `rust-toolchain.toml`에 Rust `1.97.1`과 `clippy`/`rustfmt`를 pin했다.
- `.github/workflows/ci.yml`는 clang 의존성을 설치한 뒤 `cargo test --locked
  --all-targets --all-features`를 실행한다.

**필요한 이유:** compiler/의존성 변화로 합의·직렬화 동작과 검증 결과가 조용히 달라지지
않게 재현 가능한 도구 체인과 모든 target/feature 검증을 고정해야 한다.

## 4. 결정성 회귀를 발견하고 고친 기록

payload-only 실행을 적용한 뒤 기존 테스트 fixture가 빈 payload block을 만들고, 예전의
암묵적인 local mempool fallback에 기대고 있던 사실이 드러났다. 그 fixture에서는 주문과
입금이 실행되지 않아 상태 해시/결과가 기대와 달라지는 결정성 회귀가 발생했다. 이는
각 검증자가 서로 다른 mempool을 실행하던 숨은 계약을 제거한 결과를 테스트가 반영하지
못한 것이었다.

수정은 두 부분이다.

1. 테스트와 e2e fixture가 block을 만들 때 parent 기준 `prepare_payload`로 명시적인
   transaction payload를 만들도록 바꿨다.
2. 빈 payload와 malformed payload가 local mempool을 drain하지 않고 동일한 실행 결과를
   내는 회귀 테스트를 추가했다. 이후의 transaction execution은 payload 바이트를
   재생하는 경로로만 검증한다.

### BLS seed와 bincode 회귀 수정

- multinode는 이전에 `BlsSecretKey::to_bytes()` 결과를 `from_seed()`의 seed처럼
  저장했다. 이는 원래 seed가 아니므로 재구성된 secret key의 public key가 committee
  key와 달라지는 원인이었다. 생성 시 원본 `[u8; 32]` seed를 보존하고, signing/transport
  에서는 그 seed로 key를 재구성하도록 수정했다.
- `Certificate`/`Vote`의 `skip_serializing_if`가 bincode의 positional 필드를 생략해
  BLS QC를 decode할 때 voter/public-key metadata가 밀릴 수 있었다. 조건부 생략을
  제거하고 기본값만 유지해 항상 같은 필드 순서를 직렬화하며, BLS Prepare bincode
  round-trip 테스트로 voter/public-key/context 보존을 확인했다.

### Fresh runtime 확인

- shared genesis/node loader를 사용하는 `hl-node` single run은 committed height 2에서
  정상 종료했다.
- host 4-node fixture는 네 노드가 같은 block hash를 height 3까지 commit했다.
- `Dockerfile.local` build와 Compose 4-container run은 네 컨테이너가 같은 genesis로
  height 3에 도달한 뒤 exit 0으로 종료했다.
- fresh 3-node multinode audit은 세 loopback 프로세스가 BLS-authenticated TCP로
  연결되고 strict `>2/3` voting power(동일 stake 3/3)를 사용했다. 12초 관찰 동안
  node0/node1/node2의 QC event는 각각 28/21/19회였고, 이후 관찰 view는 각각
  69/64/67이었다. `deserialize` 실패 0회, high-QC reject 0회였다. 이 실행은 관찰
  후 의도적으로 중단한 개발 데모이며, 3-node는 `f=1` BFT 구성이 아니다.

## 5. 검증 체크포인트

기존 `ignore`-tagged doctest 5개를 runnable example으로 전환했고 Rust source의
ignored code fence 및 `#[ignore]` marker는 0건이다.

- 최종 `cargo test --locked --all-features --no-fail-fast --quiet`: exit 0.
- non-doc tests: **570 passed, 0 failed, 0 ignored**.
- doctests: **6 passed, 0 failed, 0 ignored**.
- 합계: **576 passed, 0 failed, 0 ignored**.
- 이전 `cargo test --locked --lib` checkpoint는 **376 passed, 0 failed, 0 ignored**였으며
  최종 full-suite 수치와 혼동하지 않는다.
- `cargo check --locked --all-targets --all-features`: 성공.
- single committed height 2, host 4-node 동일 hash height 3, Docker 4-container height 3
  exit 0 결과는 위 Fresh runtime 확인을 참조한다. 이 결과만으로 독립 감사, 운영 복구
  검증, 브리지 안전성 또는 메인넷 준비를 주장하지 않는다.

## 6. Ingress 검증의 두 단계와 현재 blocker

1. **Transport gate:** framing/size 제한, bincode decode, authenticated peer, gossip
   `msg_id`·TTL·dedup 같은 전송 경계를 먼저 확인한다.
2. **Semantic consensus gate:** 예상 `ConsensusContext`, message type, proposer/leader,
   BLS 서명·committee membership, parent/payload/app hash, QC/TC/VCC 및 quorum을
   합의 상태를 바꾸기 전에 검증한다.

현재 gossip 수신기는 semantic 검증 전에 `msg_id`를 seen cache에 넣고 inner message를
deliver/relay한다. 따라서 유효하지 않은 메시지도 cache를 소비하거나 relay로 증폭될 수
있다. semantic admission을 `mark_seen`/deliver/relay보다 앞세우거나, context-aware
검증 실패 메시지를 별도 추적하는 것이 남은 P0 blocker다.

## 7. 환경변수의 운영 경계

환경변수는 프로토콜 합의의 권위 있는 입력이 되어서는 안 된다. 운영에서 허용하는
범위는 운영상 필요한 **secret과 endpoint**뿐이다. 예를 들어 로그 수준, bind/API 주소,
peer/RPC URL, 데이터 디렉터리, API credential, validator/relayer secret key는 배포
환경 또는 secrets manager에서 주입할 수 있다.

반대로 chain/genesis ID, consensus rule, committee voting power, epoch 전환 규칙,
contract/token address, confirmation/finality threshold, decimals, limits, pause
policy, genesis allocation과 같은 replay 가능한 규칙은 signed genesis 또는 on-chain
state에 있어야 한다. 개발용 env flag가 존재하더라도 그것이 mainnet에서 해당 규칙이나
잔액을 바꿀 수 있어서는 안 된다.

## 8. 남은 차단 요소와 다음 계획

### P0 — canonical runtime

`hl-node`가 networking, mempool admission, consensus, deterministic execution, durable
storage/recovery, finalized query를 모두 소유하는 유일한 validator/RPC runtime이 되게
한다. `hl-server`는 그 런타임의 API component/thin wrapper로 축소하고, `multinode`는
동일 runtime을 사용하는 test harness로 남긴다. direct oracle mutation, alternate
production consensus loop, dev mutation path는 제거하거나 명시적으로 차단한다.

### P0 — signed transaction envelope

API에서 내부 `Transaction`으로 변환하는 것만으로는 부족하다. version, domain/chain/genesis
identifier, signer 또는 delegated key, nonce, expiry/bounds, exact payload bytes와
signature를 포함하는 canonical signed envelope를 정의한다. admission, mempool ordering,
block payload, validator re-verification, receipt가 같은 envelope bytes를 사용하게 한다.

### P0 — durable recovery와 commit

finalized block/payload hash, state root, receipt/events root, QC와 consensus safety state를
원자적으로 durable write하고 commit marker 이후에만 finalized 결과를 노출한다. snapshot
검증, block replay, crash 중단, restart, sync 응답을 검증하고 마지막 finalized block과
voted/locked/high QC를 보존하는 recovery invariant를 만든다. 현재 in-memory demo와
부분 persistence는 이 요구를 충족하지 않는다.

### P0 — epoch/committee binding

정적 epoch 0 context와 committee hash는 proposal, vote, QC, timeout certificate,
ViewChange/NewView, sync proof, commit/safety state에 binding했다. 다음 단계는 finalized
state의 다음 위원회를 historical committee와 transition certificate로 검증하고,
key rotation/entry/exit 규칙을 증명한 뒤 경계에서만 활성화하는 것이다. 그 전까지
staking API나 configuration list를 permissionless PoS로 광고하지 않는다.

### 현재 unresolved P0

- `ConsensusContext`에 chain/genesis domain이 아직 없고, BLS proof-of-possession도 없다.
- Vote equivocation evidence가 충돌 vote의 `app_hash`와 완전한 context/signed bytes를
  함께 보존해야 한다.
- HTTP ActiveSync는 expected-context committee-bound 검증이 완전하지 않으므로 canonical
  active sync가 될 수 없다. 그 전까지 dev-only로 유지한다.
- durable atomic execution+recovery, pre-relay semantic admission, PoP key registration,
  chain/genesis domain, canonical active sync, epoch transition이 모두 남은 P0다.
- Docker/host 4-validator fixture는 존재하지만 durable restart/fault-injection 증거는 없다.

### P1/P2 — Ethereum bridge

relayer는 Ethereum의 deposit proof/finality evidence를 transaction으로 제출할 뿐 잔액을
직접 쓰지 않는다. 모든 validator가 proof 또는 합의된 attestation을 독립 검증하고,
withdrawal은 finalized outbound intent와 검증된 completion으로 회계 처리한다. replay
방지, message ID, reorg/finality, accounting conservation, rate limits, pause/recovery,
키 custody, 독립 bridge/proof review를 별도 testnet에서 검증하기 전에는 실제 브리지를
열지 않는다.

### 향후 별도 EVM domain

EVM은 현재 perp/orderbook 실행 모델에 섞지 않고 향후 application/execution interface가
소유하는 별도의 versioned execution domain으로 추가한다. domain ID/VM version,
transaction/mempool 규칙, gas schedule, state root, receipts, deterministic resource limit을
정의하고 블록이 각 domain 결과를 commit하게 한다. EVM 상태와 perp 상태를 암묵적으로
공유하지 않으며, JSON-RPC 호환만으로 EVM 통합이나 안전성 경계가 완성되었다고 보지 않는다.

위 계획과 [메인넷 준비도 감사](MAINNET_READINESS.md)의 P0–P2 gate가 수용되기 전까지
공식 상태는 계속 **not mainnet ready**이다.
