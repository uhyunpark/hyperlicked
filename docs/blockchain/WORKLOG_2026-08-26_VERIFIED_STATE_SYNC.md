# Verified state sync 작업 기록 (2026-08-26)

> 상태: HTTP startup bootstrap을 위한 verified block import 구현 기록. **전체 체인의
> MAINNET READY 판정이 아니다.**

## 문제

기존 sync 경로는 peer에서 block bytes를 다운로드하고 저장하는 역할에 머물렀다. 수신 batch가
실제로 finalized되었다는 terminal two-chain proof와 trusted committee 검증 경계가 startup
import에 연결되어 있지 않았다. 또한 `AppSnapshot`에는 orderbook이 포함되지 않아 snapshot을
canonical application state로 바로 복구하면 노드마다 state root가 달라질 수 있다.

따라서 이번 단계의 우선순위는 snapshot fast-import가 아니라, complete block replay와 terminal
finality proof 검증이다.

## 구현 및 trust roots

데이터 흐름은 다음과 같다.

```text
HTTP peer status/verified batch
  -> local genesis + trusted BLS committee 확인
  -> exact local store/app anchor 확인
  -> private CanonicalAppHook에서 finalized blocks + commit child 순차 replay
  -> app_hash(full state root) + Commitment v2 root + block/QC 검증
  -> 다음 child 하나를 bounded speculative journal에 저장
  -> finalized block/state/commitment/state-root atomic commit
  -> live application commit
  -> terminal child speculative candidate 복원
```

- `VerifiedBlockImporter::import`는 network DTO를 받지 않고 `CanonicalAppHook`,
  `PersistentStore`, `ConsensusContext`, trusted `Committee`, finalized blocks, terminal child,
  child QC를 직접 받는다.
- peer는 bytes와 height hint만 제공한다. chain domain, epoch, committee hash, genesis hash와
  BLS QC 서명은 local genesis/committee를 기준으로 다시 검증한다.
- 각 block은 `Block::app_hash`에 묶인 schema-v3 full state root와 Commitment v2 root를
  private replay 결과와 비교한다. terminal block은 child의 QC와 child `justify` QC를 포함한
  2-chain proof로 확인한다.
- import 중간에는 child 하나만 speculative로 유지한다. 다음 finalized block의 상태에는
  `high_qc=QC(child)`, `locked_qc=child.justify`를 기록하고, 최종 상태에는
  `high_qc=child_qc`, `locked_qc=commit_child.justify`를 기록한다.

## Fail-closed 및 crash semantics

- local store head, application height/hash/root, canonical height index, consensus state가
  서로 다르면 쓰기 전에 중단한다. durable non-genesis block의 Commitment/state-root 또는
  consensus state가 없거나 손상되어도 중단한다.
- 마지막 block의 app hash, Commitment root, QC가 틀리면 scratch replay 단계에서 거부하며
  live application, finalized store, consensus metadata를 변경하지 않는다.
- import plan의 모든 QC/safety state/view 산술/commitment 계산은 첫 speculative write 전에
  끝낸다. 따라서 view overflow처럼 뒤늦게 발견되는 오류도 store나 live state를 남기지 않는다.
- 각 finalized block의 child journal write와 block/state/artifact/root write는 순차적이고,
  store commit은 atomic하다. durable prefix 뒤 process가 종료되어도 다음 startup에서
  현재 head를 확인해 남은 suffix를 재시도할 수 있다.
- final child는 bounded speculative storage와 live candidate 양쪽에 남기므로 restart 뒤
  persisted high/locked QC를 다시 실행할 수 있다. 이미 반영된 prefix와 동일한 batch를 다시
  넣는 retry와 RocksDB reopen도 idempotent하게 처리한다.
- finality API는 consensus metadata와 RocksDB의 exact committed block head가 다르면 증명을
  제공하지 않는다. restart replay도 persisted speculative block의 proposer가 해당 view의
  scheduled leader인지 확인한 뒤 application preflight를 수행한다.

## API 및 CLI

공개 library 경계는 다음 두 함수다.

- `VerifiedBlockImporter::import(...)`
- `import_verified_blocks(...)`

`hl-node`는 `--sync-peer <BASE_API_URL>`가 있을 때 genesis/committee를 먼저 고정하고,
API와 consensus listener를 열기 전에 HTTP verified batch를 반복 import한다. 단일
`./scripts/local-node`는 `single-genesis.json`/`host-single/node.json`(`:8080`)만 사용하므로
같은 node를 source와 destination으로 동시에 띄울 수 없다. 실제 bootstrap은 같은
`config/local/genesis.json`과 committee를 사용하되 서로 다른 node/포트 설정을 사용한다.

현재 `TcpNetwork::wait_for_peers`는 configured peer 전부가 ready일 때까지 startup을
진행하지 않는다. 따라서 validator0/1/2만 먼저 띄운 상태에서 offline validator3를
bootstrap할 수 없다. 네 validator를 각 `HL_LOCAL_BLS_SEED_N`과 `host-4/nodeN.json`으로
먼저 정상 기동하고 validator0 API(`:8180`)가 ready가 된 뒤, validator3만 중지한다. source
validator0/1/2를 계속 실행한 상태에서 validator3를 새 data-dir로 다음처럼 재시작한다.

```bash
# After all four validators have been ready, stop only validator3.
# Keep validators0-2 running; restart validator3 with a fresh data directory.
export HL_LOCAL_BLS_SEED_4=04000000000000000000000000000000000000000000000000000000000000be
MODE=dev cargo run --locked --bin hl-node -- \
  --genesis config/local/genesis.json --config config/local/host-4/node3.json \
  --sync-peer http://127.0.0.1:8180 --data-dir "$(mktemp -d)"
```

sync 실패는 부분 복구 상태로 서비스를 시작하지 않고 process를 중단한다. peer URL은
`/api/v1/sync/status`와 `/api/v1/sync/blocks`를 제공하는 base URL이어야 한다.

## 검증

- `cargo test --locked --lib state_sync::tests`: 9 passed
  - multi-block prefix별 recovery QC 정확성
  - 성공 import 및 terminal child candidate
  - same-process retry 및 RocksDB reopen retry
  - speculative write 직후/atomic commit 직후 장애 복구
  - invalid terminal app hash/Commitment root/QC와 view overflow의 live/store 무변경
- `cargo test --locked --lib network::active_sync::tests`: 23 passed
- `cargo test --locked --lib api::routes::sync::tests`: 10 passed
- `cargo test --locked --bin hl-node`: 13 passed
  - 실제 HTTP status -> blocks -> finality 요청 경로와 reopen
  - import 뒤 `ConsensusRunner` recovery handshake
  - persisted speculative non-leader/invalid QC/commitment fail-closed
- `cargo test --locked --lib`: 717 passed, 0 failed
- `cargo test --locked --test e2e`: 95 passed, 0 failed
- `cargo check --locked --all-targets --all-features`, `cargo fmt --all -- --check`: 통과

## 명확한 한계

- 현재 운영 연결 경로는 **HTTP startup bootstrap**이다. full snapshot materialization,
  generic P2P state sync, peer failover/다중 peer orchestration은 후속 작업이다.
- 기존 `AppSnapshot`은 orderbook 누락 때문에 canonical import에 사용하지 않는다. snapshot
  import을 재활성화하려면 orderbook을 포함한 완전한 authenticated state format과 별도 검증
  경계를 먼저 설계해야 한다.
- 이번 구현은 block replay와 finality proof를 완성한 것이며, sync API의 장기 serving policy,
  운영 retry/backoff, 대규모 state transfer 최적화는 아직 남아 있다.
