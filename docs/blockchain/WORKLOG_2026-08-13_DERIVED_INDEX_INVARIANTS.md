# Derived Index 불변식 작업 기록 (2026-08-13)

## 결론

Orderbook, staking, trigger의 파생 인덱스를 합의 원본 상태와 분리했다. 스냅샷/import는
원본에서 인덱스를 원자적으로 재구축하고, 정상 블록 실행과 private replay는 재구축 없이
불일치를 검사한다. 검증에 실패한 실행 결과는 candidate에 들어가지 않으며 commit/event도
발행하지 않는다.

이에 따라 full-state shadow root는 파생 인덱스를 제외하고 원본 상태만 커밋한다. 인코딩
경계가 바뀌었으므로 `FULL_STATE_SCHEMA_VERSION`을 1에서 2로 올렸다. 아직 public chain과
activation height가 없으므로 구 schema migration은 만들지 않았다.

## Primary와 derived 경계

| 영역 | 합의 원본(primary) | 재구축 가능한 derived |
|---|---|---|
| Orderbook | symbol, sequence, last price, bid/ask price-level FIFO queues | order ID lookup, trader별 open-order count |
| Staking | operator별 validator record | node ID → operator lookup |
| Trigger | trigger ID별 order, trigger sequence | trader/symbol별 ID vector, CLOID lookup |

Snapshot/import에서는 임시 상태 전체를 만든 뒤 모든 derived index를 rebuild/validate하고 성공한
경우에만 교체한다. 정상 실행에서는 block execution 직후 한 번 validate하고 성공한 상태만
immutable candidate로 저장한다. Candidate preflight/commit은 같은 O(n) 검사를 반복하지 않는다.
Candidate가 없는 recovery fallback은 private replay 직후 한 번 검사한다.

## 구현 내용

- Orderbook rebuild는 빈 price level, queue side/price/symbol 불일치, duplicate order ID,
  counter overflow, runtime order sequence 역행을 거부한다.
- Staking rebuild는 validator map key/operator 불일치와 duplicate node ID를 거부한다.
- Trigger rebuild는 map key/order ID 불일치, duplicate CLOID, stale sequence, 잘못된 secondary
  reference를 거부하며 stable ID 순서를 만든다.
- Snapshot `try_from_snapshot_with_chain_domain`은 duplicate account/market/map key/trigger
  ID/CLOID를 조용히 덮어쓰지 않고 `Result`로 거부한다.
- 파생 인덱스를 훼손해도 full-state root는 바뀌지 않지만 invariant 검사는 실패한다. 즉 root는
  primary truth를 커밋하고 runtime admission guard가 cache 일관성을 보장한다.

검토 과정에서 새 검사가 정상 실행을 오판할 수 있었던 기존 문제도 수정했다.

- 주문 취소/전량 maker fill 뒤 trader count가 0인 key를 남기던 문제
- 마지막 trigger 제거 뒤 빈 trader/symbol vector key를 남기던 문제
- `T1..T12`를 문자열 순서로 정렬해 `T10`이 `T2`보다 앞서던 문제

Trigger rebuild와 실행은 이제 `T<digits>`를 숫자 sequence로 정렬하고, 비표준 ID 또는 숫자
동률은 전체 문자열로 deterministic fallback한다.

## 성능 결정

Derived validation은 open order/validator/trigger 수에 대해 O(D)이고 임시 map을 만든다. 매
block마다 execute 직후 한 번만 수행하며 candidate의 commitment/root/commit preflight에서는
반복하지 않는다. Full-state root는 여전히 전체 consensus state에 대해 O(S)이므로 mainnet
activation 전 component tree 또는 dirty-subtree incremental root가 필요하다.

2026-08-13 local release benchmark (`cargo bench --locked --bench commitment_v2`):

| Accounts | Execute | Legacy hash | Full-state root v2 |
|---:|---:|---:|---:|
| 100 | 0.631 ms | 0.034 ms | 0.100 ms |
| 1,000 | 3.006 ms | 0.369 ms | 0.727 ms |
| 5,000 | 14.752 ms | 1.888 ms | 3.814 ms |

이 workload는 deposit/account 위주라 large live orderbook/trigger 검증 비용을 대표하지 않는다.
Activation 전에 open-order/trigger cardinality benchmark와 state/snapshot size limit을 별도로
추가해야 한다.

## 검증 결과

- `cargo test --locked --all-targets --all-features`: 전체 통과
  - library 504 tests, ignored 0
  - node 4, multinode binary 1, e2e 98 및 나머지 integration/bench target 통과
- Orderbook focused: 23 통과
- Trigger focused: 13 통과
- Canonical corruption focused: candidate 미생성/private replay 거부 및 publish 없음 통과
- full-state root: schema-v2 golden vector, insertion-order independence, transient/derived exclusion 통과
- `cargo fmt --all -- --check`, `git diff --check`: 통과

## 다음 단계

1. ~~Snapshot/import용 `validate_primary_state()`를 별도로 정의한다.~~ 2026-08-13 완료.
   상세 내용은 [Primary State 불변식 작업 기록](WORKLOG_2026-08-13_PRIMARY_STATE_INVARIANTS.md)을
   참고한다.
2. Snapshot byte/record cardinality와 장기 state growth limit을 정하고 adversarial benchmark를
   추가한다.
3. ~~불완전한 legacy `incremental_hash` production 경로 제거~~ — 2026-08-13 완료.
4. ~~Fixed component-tree schema-v3 shadow 구현~~ — 2026-08-13 완료.
5. Component dirty-subtree cache를 fresh component-root와 교차 검증한다.
6. 그 후 `app_hash = H(version, full_state_root, receipt_root, event_root)` activation 규칙과
   cross-validator/chaos test를 구현한다.
7. Indexer bounded-range API는 그 다음 단계로 유지한다.

현재 full-state root는 계속 shadow 상태이며 QC/header에 아직 결합되지 않았다. 따라서 이번
작업은 activation prerequisite를 완료한 것이고, mainnet state commitment 완료를 뜻하지 않는다.
