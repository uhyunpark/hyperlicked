# Full-State Root Shadow 작업 기록 (2026-08-12)

> 후속 상태: 이 문서의 shadow root는 2026-08-14
> [consensus activation](WORKLOG_2026-08-14_CONSENSUS_STATE_ROOT_ACTIVATION.md)에서
> `Block::app_hash`와 Vote/QC에 결합됐다. 아래 내용은 활성화 전 구현 기록이다.

## 결론

합의에 영향을 주는 애플리케이션 상태를 결정론적으로 커밋하는 versioned full-state
root를 구현했다. 현재 root는 `Block.app_hash`나 QC에 넣지 않은 shadow 단계다. 각
validator는 proposal 실행 결과에서 root를 만들고, finalized block/consensus state/
Commitment v2와 같은 RocksDB synced batch에 저장한다. 재시작 replay에서 저장된 root와
재계산 root가 다르거나 non-genesis root가 없으면 노드는 시작하지 않는다.

Indexer bounded-range API는 후속 작업으로 미뤘다. Receipt/event artifact는 이미 블록별
행위를 보존하며, 이번 작업은 그 artifact와 결합할 신뢰 가능한 state root를 먼저 만든다.

## 구현 경계

초기 구현은 `FULL_STATE_SCHEMA_VERSION = 1`과 `HYPERLICKED_FULL_STATE_ROOT\0` domain separator를
사용한다. 문자열과 collection은 길이 prefix, 정수는 little-endian으로 직접 인코딩하며,
`HashMap`은 key 정렬 후 인코딩한다. serde/bincode map 순서에는 의존하지 않는다.

포함 상태:

- chain domain, timestamp, current view, committed height
- account, nonce/pending nonce, position 전체 필드
- orderbook의 모든 price level/FIFO order, order sequence, last price
- market/risk config, mark price/EMA
- insurance fund, premium sample queue, funding rate/time
- validator/PoP, delegation, unstake, epoch snapshot, liveness, evidence queue
- trigger primary records/sequence, oracle price/source/config/update state
- 현재 실행이 직접 읽는 orderbook/staking/trigger 파생 index (schema v1)

제외 상태:

- local envelope policy와 mempool
- pending WebSocket/event queue, last execution artifact
- trade history, candle, daily-stat API cache
- incremental hasher cache와 node-configured staking context

파생 index는 primary record에서 재구성 가능하지만 현재 실행이 직접 읽는다. Import/recovery
경계에서 rebuild와 invariant 검증을 강제하기 전까지는 index 손상도 root 차이로 감지하도록
포함했다.

> 2026-08-13 후속 작업에서 rebuild/validate 경계를 구현했고 파생 index를 root에서 제외했다.
> 따라서 현재 schema는 v2다. 상세 내용은
> [Derived Index 불변식 작업 기록](WORKLOG_2026-08-13_DERIVED_INDEX_INVARIANTS.md)을 참고한다.

## 실행과 저장

- `AppState::compute_full_state_root()`가 새 schema root를 계산한다.
- 기존 `compute_state_hash_full()`과 `Block.app_hash` 규칙은 변경하지 않았다.
- `CanonicalAppHook`은 exact speculative candidate를 만들 때 root를 한 번 계산해 캐시한다.
  Proposal/vote/finalization preflight가 같은 O(n) scan을 반복하지 않는다.
- leader/follower는 root를 만들 수 없으면 proposal persistence와 vote 전에 거절한다.
- RocksDB `state_roots` column family에 block hash별 schema-v3 tagged root record을 저장한다.
  Raw legacy 32-byte row, unsupported version, truncated/trailing bytes는 restart load에서
  fail-closed한다.
- finalized block, height index, consensus safety state, Commitment v2, state root는 한 synced
  `WriteBatch`로 기록된다. 같은 block의 artifact/root를 다른 값으로 덮어쓸 수 없다.
- pruning은 block/Commitment v2와 함께 state-root row도 제거한다.
- restart는 non-genesis block마다 Commitment v2와 state root를 replay 결과와 비교한다.

## 검증 결과

- `cargo test --all-targets --all-features --locked`: 전체 통과
  - library 486 tests
  - node replay 4 tests
  - integration/e2e 및 bench target 포함
- `cargo check --all-targets --all-features --locked`: 통과
- `cargo fmt --all -- --check`, `git diff --check`: 통과
- local persistent restart: height `0 -> 3 -> 5` 통과
- focused state-root tests: golden vector, map insertion-order independence, authoritative state
  sensitivity, transient exclusion, derived-index corruption detection, exact candidate caching,
  atomic persistence/reopen, immutable retry, invalid write rollback, restart mismatch rejection 통과

Release benchmark (`cargo bench --bench commitment_v2 --locked`, Apple Silicon local run):

| Accounts | Legacy state hash | Full-state root |
|---:|---:|---:|
| 100 | 0.033 ms | 0.128 ms |
| 1,000 | 0.361 ms | 0.724 ms |
| 5,000 | 1.853 ms | 3.711 ms |

5,000 accounts 기준 full scan은 100 ms block budget의 약 3.7%다. Candidate는 child derivation을
위해 component tree seal을 보관하지만, preflight/direct commit은 corruption 방지를 위해 매번
전체 tree를 fresh recompute한다. 계정/order 수가 커지면 flat O(n) encoder를 계속 사용할 수
없으므로 activation 전에 component dirty-subtree 방식을 추가로 검증해야 한다.

## 남은 activation gate

1. ~~Snapshot/import에서 primary record로 파생 index를 rebuild하고 invariant를 검증한다.~~
   2026-08-13 완료.
2. ~~기존 `incremental_hash` feature와 불완전한 dirty cache 제거~~ — 2026-08-13 완료.
3. ~~Fixed component-tree schema-v3 shadow 구현~~ — 2026-08-13 완료.
4. ~~Transient dirty-subtree derivation을 fresh component-tree와 교차 검증한다.~~ — 2026-08-14
   완료. Persisted root cache는 두지 않고, preflight/direct commit에서 전체 tree를 다시
   계산해 candidate seal을 fail-closed 검증한다.
5. `app_hash = H(version, full_state_root, receipt_root, event_root)` 규칙과 protocol activation
   height를 정의하고 vote/QC/header에 결합한다.
6. 위 activation 후 cross-validator disagreement/chaos test와 snapshot proof를 추가한다.
7. 마지막으로 indexer bounded-range/cursor API를 추가한다. Indexer는 block hash/height,
   tx index, receipt/event roots, activated app hash를 checkpoint로 사용한다.

현재 단계의 root는 local corruption과 validator 실행 불일치를 조기에 탐지하지만, 아직 QC에
암호학적으로 묶이지 않았으므로 mainnet state commitment로 간주하면 안 된다.
