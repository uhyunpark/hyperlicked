# 자원 및 복구 경계 하드닝 작업 기록 (2026-08-22)

> 상태: 현재 작업 트리의 코드·테스트를 확인한 로컬 기록. **MAINNET READY가 아니다.**

이번 기록은 speculative candidate/pending 자원 상한, ActiveSync 다운로드 상한, legacy
`Engine` feature 격리, application recovery handshake의 네 경계를 다룬다. 항목 1은
candidate snapshot과 pending/journal을 서로 다른 자원으로 기록하고, 아래 표에 남은
검증·liveness caveat를 분리한다.

## 현재 상태 요약

| 항목 | 현재 코드에서 확인한 상태 | 현재 판정 |
|---|---|---|
| 1. speculative candidate / pending cap | candidate 16개·depth16, pending ordinary soft63/QC hard64·depth16, journal soft48 MiB/hard96 MiB·per-row48 MiB, atomic application admission과 rolling progress admission, first-write-wins, delayed-QC exact replay, production Memory cache pruning, startup QC private validation, writer mutex/orphan reconciliation이 소스와 targeted test에서 확인된다. | core/all-target 검증 통과, caveat 유지 |
| 2. ActiveSync 자원 상한 | 요청 window 1,000 blocks, HTTP page raw JSON 32 MiB, 한 download window raw body 누적 128 MiB를 검사한다. exact-boundary와 초과 targeted test가 통과했다. | core/all-target 검증 통과 |
| 3. legacy `Engine` 격리 | 기본 feature에 `legacy-engine`이 없고 module/re-export 및 관련 통합 테스트가 `cfg(feature = "legacy-engine")`이다. feature를 명시하면 호환 Engine은 여전히 빌드·공개된다. | 기본 경로 격리 확인; 제거 또는 강제 test-only는 아님 |
| 4. recovery exact-head/domain/root handshake | runner가 exact persisted head와 application hook을 맞교환하고, canonical hook이 domain·height·hash·fresh state root를 검증한다. committed replay와 speculative QC branch replay도 별도 검증하며, restart resume view와 timeout reset도 확인한다. | core/bin + local smoke 통과, caveat 유지 |

최종 검증 snapshot은 `cargo test --locked --lib` 587/587, `cargo test --locked --bin hl-node`
6/6, `cargo check --locked --all-targets --all-features`, `--no-default-features`, doc 5/5,
fmt/diff check 통과다. smoke 수정 직전 `cargo test --locked --all-targets --all-features`도
593개 lib test를 포함해 통과했고, 수정 후 core 587개와 bin 6개를 재검증했다. fresh DB
height 0→1 및 같은 DB restart height 1→2 local smoke도 통과했다.

## 1. Speculative candidate와 pending resource cap

### 의도와 현재 문제

Consensus proposal 또는 view-change가 commit보다 빠르게 쌓이면, application candidate는 깊은
`AppState` clone을 보유하고 runner의 pending map과 저장소는 block을 보유한다. Byzantine
proposer가 fork를 반복하거나 QC가 지연될 때 이 구조가 무제한이면 메모리와 디스크가 함께
증가한다. 반대로 단순 FIFO/age eviction은 늦게 도착한 QC가 참조하는 branch의 ancestor를
없애 safety 또는 liveness를 깨뜨릴 수 있다.

현재 작업 트리에는 이 문제를 줄이기 위한 count/depth, byte journal, protected-ancestor,
private replay 및 rolling progress admission 설계가 들어가 있다. 다만 candidate snapshot의
실제 메모리는 별도 caveat로 남는다.

### 현재 구현

- `src/api/canonical.rs`의 `CanonicalAppHook`은 candidate snapshot 16개와
  `MAX_SPECULATIVE_DEPTH = 16`을 고정 상한으로 두고, `validate_block`/`execute`가 canonical
  height 대비 깊이와 candidate 수를 검사한다. 같은 hash의 재검증은 slot을 다시 소비하지
  않는다. runner의 pending/replay depth도 `MAX_PENDING_DEPTH = 16`으로 정렬되어 있다.
- candidate map이 가득 찬 경우 runner가 high/locked QC와 다음 proposal parent를 protected
  root로 계산하고, 각 root의 완전한 ancestor closure를 남긴다. atomic application admission은
  private staged clone에서 보호 branch pruning, reserve slot 확인, ancestor restore를 모두
  통과한 뒤에만 live candidate map을 publish한다. commit 뒤에는 새 committed block의
  descendant만 보존해 conflicting branch와 stale ancestor를 제거한다.
- fresh-DB height 0→1 smoke에서 발견된 leader 생성 실패는 private preflight가 zero
  `app_hash`/commitment root draft를 final block처럼 비교한 문제였다. 이제 staged private state에서
  draft를 먼저 실행하고, 실행 결과를 넣은 `executed_block`으로 commitment와 state-root를
  계산·검증한다. follower가 보내온 non-zero roots는 기존처럼 exact 비교하므로 인증된
  follower 입력의 검사를 약화하지 않는다.
- restart 시 `restore_speculative_chain`은 임시 hook에 branch 전체를 stage한 뒤 context,
  height, parent, application hash, Commitment root를 모두 확인하고 성공한 경우에만
  candidate map에 publish한다. 늦은 branch 오류가 canonical state에 부분 반영되지 않도록
  한다.
- `src/consensus/runner.rs`는 pending hard64/depth16과 ordinary soft63을 둔다. verified
  QC(`justify`가 검증된 proposal)는 예약된 64번째 slot을 사용할 수 있고 ordinary proposal은
  soft cap에서 backpressure를 받는다. proposal/투표 block을 pending에 넣기 전 depth와
  count를 검사하고, 같은 block hash는 허용한다. 63개 ordinary branch 뒤 hard slot을 사용한
  A가 아직 자체 QC로 확정되지 않은 상태에서 더 높은 view의 verified child B가 오면,
  B와 같은 parent를 가진 A의 unprotected lower-view sibling branch 및 descendant closure만
  rolling replacement한다. high/locked QC와 다음 parent의 protected ancestor closure는
  보존한다.
- speculative block은 `save_speculative`로 canonical height index와 분리해 저장한다. journal
  aggregate budget은 ordinary soft48 MiB/QC hard96 MiB이고, 단일 serialized row는 48 MiB로
  제한된다. runner는 admission 전 serialized size를 예약하고, round 시작 및 proposal/vote
  경로에서 protected roots 기준으로 prune한다. MemoryBlockStore(실행 중 cache)와 RocksDB
  journal 모두 이 count/byte 경계를 적용한다.
- `admit_speculative_with_rolling_victim`은 각 store의 writer mutex 아래에서 target,
  protected closure와 victim branch를 계획한다. RocksDB는 여기에 orphan manifest reconciliation을
  포함해 victim/orphan 삭제와 새 row/manifest 삽입을 하나의 synced batch로 기록하고, Memory
  store도 같은 journal mutex로 check/prune/insert를 직렬화한다. orphan manifest는 body가 없을
  때 manifest만 reconciliation 대상으로 삼고 canonical height index/committed metadata는
  건드리지 않으며, malformed manifest/body는 fail closed한다.
- hash가 `justify`를 포함하지 않는 점을 이용해 Memory/RocksDB speculative write는
  first-write-wins다. 나중에 더 큰 certificate가 붙은 같은 hash가 기존 body나 journal row를
  덮어쓰지 않는다.
- candidate snapshot이 eviction된 뒤에도 delayed QC는 locally persisted body를 exact
  context/height/parent closure로 읽어 private application replay를 하고, admission 후에만
  live candidate를 복원한다. startup의 `replay_speculative_application`도 high/locked QC를
  certificate·app hash와 함께 private validate한다. 실제 `63 forks → QC child → commit`
  경로는 `verified_qc_progress_uses_reserved_slot_then_commit_reopens_ordinary_admission`
  test로 확인된다.

### 보안·성능 trade-off

count/depth admission과 journal byte cap을 fail closed로 하면 공격자가 후보 state clone과
serialized body를 무한히 쌓는 것을 막을 수 있고, protected QC branch를 임의로 버리지 않아
safety를 보존한다. rolling replacement는 unprotected lower-view sibling이 남아 있을 때
새 verified progress가 hard cap에서 멈추지 않도록 liveness를 개선하지만, protected branch만
남으면 여전히 absolute cap/backpressure로 새 proposal 또는 valid delayed continuation이
거부된다. writer mutex와 RocksDB synced batch는 concurrent admission을 직렬화해 일관성을
높이는 대신 contention·I/O latency를 추가한다. candidate 하나는 COW 없는 full `AppState`
deep clone이므로 candidate count와 depth만으로 aggregate heap을 정확히 제한하지 못하며,
protected-ancestor 계산과 exact delayed-QC replay도 hash/block scan과 CPU 비용을 추가한다.

### 현재 미해결 사항과 검증 상태

- journal의 soft48 MiB/hard96 MiB/per-row48 MiB와 first-write-wins는 source와 targeted
  tests에서 확인됐다. 그러나 application candidate map에는 COW 없는 full `AppState` clone을
  합산하는 byte cap/count-only snapshot accounting이 없으므로 journal cap을 candidate
  aggregate memory cap으로 간주하면 안 된다.
- delayed-QC exact replay는 현재 local speculative body/journal을 전제로 한다. 검증된
  network refetch API가 없어서 local body가 eviction·손상으로 사라진 경우를 복구하는
  경로는 mainnet 후속 caveat다.
- bounded indexer API는 사용자 요청으로 이번 범위에서 deferred이며, 현재 기록의 cap은
  speculative candidate/pending/journal admission에 한정된다.
- `speculative_candidates_are_bounded_without_eviction_of_protected_ancestors`,
  `verified_justify_receives_reserved_pending_slot_after_soft_cap`,
  `verified_qc_progress_uses_reserved_slot_then_commit_reopens_ordinary_admission`,
  `delayed_qc_rehydrates_evicted_canonical_candidate_from_store_body`,
  `memory_speculative_hash_reuse_is_first_write_wins`,
  `delayed_higher_view_sibling_rolls_a_body_and_reopens_after_commit`,
  `rolling_admission_preserves_protected_sibling_without_partial_writes`,
  `production_memory_cache_keeps_exact_head_without_canonical_history_growth`,
  `concurrent_speculative_admission_at_count_cap_allows_only_one_writer`,
  `speculative_orphan_manifest_is_atomically_removed_without_touching_canonical_metadata`,
  `leader_draft_private_preflight_uses_executed_roots_without_publishing` 및 관련 저장소
  경계 test는 targeted 실행에서 통과했다. core/all-target 검증 결과는 위 최종 snapshot에
  기록했으며, 이 항목의 caveat가 해소됐다는 뜻은 아니다.

## 2. ActiveSync 요청 span 1,000 / page 32 MiB / total 128 MiB

### 의도와 현재 문제

HTTP response 한 페이지에만 상한을 두면 downloader가 여러 page를 하나의 `Vec<Block>`에
계속 누적해 전체 memory를 고갈시킬 수 있다. 반대로 너무 작은 page는 정상 catch-up의
round-trip을 늘린다. 요청 범위, page raw body, 전체 download window를 서로 다른 경계로
고정하는 것이 목적이다.

### 현재 구현

- `src/network/active_sync.rs`의 `MAX_ACTIVE_SYNC_BLOCKS = 1_000`은 trusted anchor부터
  target height까지의 inclusive block span을 HTTP 요청 전에 검사한다. 1,001개 요청은
  peer URL을 접속하기 전에 거부된다.
- 한 HTTP 요청 URL에는 `limit=100` (`MAX_BLOCKS_PER_REQUEST`)을 사용한다. server route의
  일반 최대 limit 1,000과 혼동하지 말아야 한다. ActiveSync client는 page를 100개 단위로
  받아도 전체 span은 1,000개를 넘지 않는다.
- `MAX_SYNC_RESPONSE_BYTES`는 `32 * 1024 * 1024`이고, client의
  `MAX_BLOCK_RANGE_RESPONSE_BYTES`가 이를 page raw JSON envelope cap으로 사용한다.
  `read_bounded_json`은 Content-Length뿐 아니라 매 chunk를 append하기 전에 검사하므로
  chunked response와 거짓 length header도 같은 경계를 넘을 수 없다.
- `MAX_ACTIVE_SYNC_TOTAL_BYTES = 4 * MAX_BLOCK_RANGE_RESPONSE_BYTES`로 한 download window의
  raw HTTP body 누적을 128 MiB로 제한한다. 다음 page에는 남은 예산만 전달하고, 누적 예산을
  넘으면 deserialization/append 이후 다음 단계로 진행하지 않는다.
- `src/api/routes/sync.rs`도 block range JSON envelope의 정확한 serialized size를 계산해
  32 MiB를 넘는 page를 자르고, 단일 block 자체가 한도를 넘으면 `PAYLOAD_TOO_LARGE`를
  반환한다. block payload의 별도 10,000,000-byte limit은 그대로 적용된다.

### 보안·성능 trade-off

page cap은 한 번에 할당하는 raw response를 제한하고 total cap은 page pagination을 이용한
누적 DoS를 제한한다. 128 MiB는 raw body 기준이므로 JSON deserialization과 `Vec<Block>`의
추가 heap overhead까지 128 MiB로 보장하는 것은 아니다. 1,000 block span은 정상 sync의
round-trip을 줄이지만, 그보다 큰 catch-up은 여러 window로 나누어야 한다. page 32 MiB와
100-block 요청은 큰 payload에서 추가 왕복을 만들 수 있다. ActiveSync는 여전히 verified
block download 결과만 반환하며 application state를 직접 교체하지 않는다.

### 검증 상태

소스에 `requested_block_span_is_bounded`, `oversized_requested_span_is_rejected_before_http`,
`response_chunk_budget_accepts_boundary_and_rejects_one_byte_over`,
`total_response_budget_accepts_exact_boundary_and_rejects_one_byte_over`,
`cumulative_response_budget_rejects_second_page`,
`cumulative_response_budget_accepts_exact_boundary`, `ordinary_verified_range_succeeds`
테스트가 있다. API route에는 `serialized_range_size_accepts_exact_limit_and_rejects_one_byte_over`,
테스트가 있다. 이 route test와 `cargo test --locked --lib active_sync`(15 passed)가 최신
작업 트리에서 통과했다.
이는 ActiveSync와 route의 targeted 결과이며, 587개 library 검증과 all-target checks에도
포함됐다.

## 3. Legacy `Engine` non-default feature isolation

### 의도와 현재 문제

canonical validator는 `ConsensusRunner`와 `CanonicalAppHook`을 통해 state root,
Commitment v2 artifact, durable commit/recovery 경계를 사용한다. 이전 in-memory `Engine`은
호환·단위 테스트 용도로 남아 있으며 이 canonical lifecycle과 동일한 atomic artifact/root
commit 경계를 보장하는 production runtime이 아니다. 기본 빌드에서 이 경로를 우연히 선택하지
않도록 feature isolation을 둔다.

### 현재 구현

- `Cargo.toml`의 `legacy-engine = []`는 `default = ["bls_batch_verify"]`에 포함되지 않는다.
- `src/consensus/mod.rs`의 `engine` module 선언과 `Engine` re-export가 모두
  `#[cfg(feature = "legacy-engine")]`다. `src/lib.rs`도 production entry point가
  `ConsensusRunner`이고 legacy `Engine`은 opt-in compatibility feature라고 명시한다.
- `tests/e2e.rs`와 `tests/recovery_test.rs`에서 `Engine`/`MemoryBlockStore`를 사용하는
  legacy test와 import도 같은 feature로 gate되어 기본 test target이 legacy API를 요구하지
  않도록 했다.
- `hl-node`와 canonical runtime 경로는 `ConsensusRunner`를 사용한다. `MemoryBlockStore`와
  `NoOpApp` 자체는 기본 API에 남아 있으므로, 이것이 legacy Engine을 완전히 제거했거나
  모든 in-memory 사용을 private test-only로 만들었다는 뜻은 아니다.

### 보안·성능 trade-off

기본 binary/API에서 Engine implementation과 re-export가 빠지므로 실수로 호환 경로를
production runtime으로 선택할 가능성과 기본 build surface가 줄어든다. 명시적으로
`--features legacy-engine`을 켜면 기존 테스트·호환 사용자는 유지되지만, 그 선택은
canonical runner의 durable artifact/state-root atomicity를 자동으로 보장하지 않는다.
따라서 feature gate는 안전한 기본값이지, feature 활성화를 cryptographically 금지하는
격리는 아니다. Engine을 제거하지 않고 보존하는 대신 compile/test compatibility와 개발
편의성을 유지하는 trade-off가 있다.

### 검증 상태

현재 `Cargo.toml`, `src/consensus/mod.rs`, `src/lib.rs`, 두 integration test의 cfg 경계를
소스 대조로 확인했다. 기본 feature에 `legacy-engine`이 없고 all-target/all-feature 및
no-default-features check가 통과했다. feature를 켜도 Engine이 존재한다는 점은 의도된
호환 상태이며 완료된 제거로 기록하지 않는다.

## 4. Application recovery exact-head/domain/root handshake

### 의도와 현재 문제

consensus metadata만 복구하고 새 application state를 붙이면, 같은 height의 다른 fork,
다른 chain domain, stale 또는 손상된 state root를 가진 application이 투표와 commit을
재개할 수 있다. Snapshot은 orderbook 등 canonical execution state를 모두 포함하지 않으므로
snapshot height만 신뢰하는 방식도 exact application head를 증명하지 못한다. 복구된 consensus
head와 application이 같은 block hash, domain, fresh state root를 보유한다는 handshake가
필요하다.

### 현재 구현

- `ConsensusState`는 epoch, committee hash, genesis domain, committed height/hash와 high/locked
  QC를 함께 저장한다. `ConsensusRunner::new_with_recovery`는 context가 현재 config와
  일치하는지 확인하고 finalized chain의 canonical genesis, 연속 height/parent, QC, committed
  metadata와 persisted QC reference를 검증한다.
- `src/bin/node.rs`의 canonical startup은 새 `AppState`를 만들고 genesis부터 모든 finalized
  block을 replay한다. 각 block의 context/parent/QC를 검증하고, 저장된 Commitment v2와
  application이 재생성한 artifact/root 및 block header root를 비교한 뒤에만 `commit`한다.
  마지막에 replay된 height/hash가 persisted committed metadata와 일치해야 한다.
- 같은 startup은 high/locked QC target의 speculative branch도 context, target app hash,
  certificate와 contiguous parent closure를 검증한 뒤 `restore_speculative_chain`으로
  별도 candidate state에 stage한다. branch 전체가 성공한 뒤에만 candidate를 publish하므로
  canonical application state를 speculative replay가 직접 바꾸지 않는다.
- `ConsensusRunner::initialize_live`는 명시적으로 `with_app()`으로 application hook이
  붙었는지 확인하고, 저장된 exact committed hash와 height의 block을 찾은 후
  `AppHook::validate_recovery_head`를 호출한다. 기본 hook은 non-genesis에서 fail closed다.
- same-DB restart smoke에서 persisted highQC와 같은 view에서 재투표할 수 있는 결함이
  발견되어, restart resume는 persisted `current_view`와 persisted high-QC view+1 중 큰 값을
  사용한다. high-QC 때문에 view가 전진하면 pacemaker의 timeout/view-change 상태를 reset하고,
  그렇지 않으면 persisted timeout 상태를 유지한다. 따라서 동일 view 재투표를 차단하면서
  정상적인 timeout accounting은 보존한다.
- `CanonicalAppHook::validate_recovery_head`는 block validation 뒤 application chain domain과
  block genesis domain, canonical committed height와 block height, 내부 committed block hash와
  consensus head hash를 비교하고, consensus-state invariant와 fresh component-tree root가
  `block.app_hash`와 같은지 확인한다. `NoOpApp`은 별도로 zero application root와 empty
  commitment root를 확인하는 stateless test 경계다.
- RocksDB state-root row는 schema version과 root를 함께 저장하고, finalized block/state,
  Commitment v2, root, committed metadata는 synced atomic write 경계에서 기록된다. 다른
  domain, raw/unsupported root record, mismatched head/root는 recovery 전에 거부된다.

### 보안·성능 trade-off

exact hash/domain/root handshake는 height만 같은 fork와 cross-chain replay를 막고, application
state가 합의 head와 다르면 투표를 시작하지 않게 한다. 실패 시 자동 복구나 추측성 migration을
하지 않고 fail closed하는 대신, canonical restart마다 genesis부터 replay하고 artifact와 fresh
root를 재계산하므로 startup CPU와 시간이 커진다. speculative QC branch replay는 추가 state
clone 비용이 있지만 canonical state를 오염시키지 않는다. Snapshot fast recovery와 verified
chunk import가 아직 이 handshake를 대체하지 않는다.

### 검증 상태

소스에 `recovered_runner_requires_non_genesis_application_head_handshake`,
`recovery_requires_the_complete_finalized_chain`, `validate_recovery_head`의 exact hash/root
검사, `speculative_replay_rejects_untrusted_same_height_anchor_without_mutation`,
`speculative_replay_publishes_no_partial_candidates_after_late_failure`,
`speculative_replay_rejects_context_parent_and_app_hash_mismatch`,
`recovery_resumes_after_the_persisted_high_qc_view` 테스트가 있다. 최신 targeted 실행은
recovery 이름 필터 library 6개와 exact-head handshake 1개, speculative replay 3개,
resume-view 1개, `hl-node` recovery 2개가 통과했다. 저장소에는 state-root record의
schema/raw/trailing-byte 거부와 atomic round-trip 테스트가 있다. fresh DB height 0→1 및
같은 DB restart height 1→2 smoke도 통과했다. 장기 WAN, crash/power-loss, Byzantine 및
독립 보안 검토는 남아 있으며, 이 기록만으로 mainnet 복구 readiness를 주장하지 않는다.

## 남은 공통 게이트

- application candidate의 COW 또는 aggregate memory accounting을 정하고, rolling replacement와
  absolute cap/backpressure가 허용하는 liveness를 Byzantine·reordering 조건에서 측정한다.
- verified delayed-QC continuation을 위해 local body가 없는 경우를 다룰 verified refetch
  API/proof 경계를 설계한다.
- bounded indexer API는 사용자 요청에 따라 deferred 상태로 유지한다.
- pending/store의 63→64 및 48/96 MiB boundary와 application admission은 모든
  feature/target에서 확인됐지만, 장기 WAN·crash/power-loss·storage corruption과 독립
  보안 검토는 별도 게이트다.
- full replay 비용을 줄일 verified snapshot/chunk manifest, trusted finalized anchor, proof와
  application-executed import를 설계한다.
- 장기 WAN, Byzantine, crash/power-loss, storage corruption, cross-validator disagreement
  및 독립 보안 검토를 수행한다.
