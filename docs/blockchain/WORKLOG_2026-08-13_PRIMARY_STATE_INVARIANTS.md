# Primary State 불변식 작업 기록 (2026-08-13)

> 후속 상태: 이 문서 작성 당시 비활성 상태였던 두 root는 이후 consensus에 연결됐다.
> [state-root activation](WORKLOG_2026-08-14_CONSENSUS_STATE_ROOT_ACTIVATION.md),
> [Commitment v2 activation](WORKLOG_2026-08-21_COMMITMENT_V2_CONSENSUS_ACTIVATION.md)

## 결론

합의 상태의 authoritative record에 대해 `AppState::validate_primary_state()`를 구현했다.
이제 잘못된 스냅샷은 derived index를 재구축하기 전에 거부되고, 블록 실행 결과는 primary와
derived 검증을 모두 통과해야만 speculative candidate가 된다. Candidate가 없는 recovery
fallback도 private replay 직후 동일하게 검사한다.

검사는 candidate 생성 시 한 번만 수행한다. 검증된 immutable candidate의 commitment/state-root
preflight와 commit에서는 전체 상태를 다시 순회하지 않는다. 이번 작업은 correctness guard이며,
full-state root와 receipt/event root를 아직 `app_hash`/QC에 활성화한 것은 아니다.

## 검증 범위

- Market/config: map key와 symbol 일치, tick/lot/notional, bounded fee/funding/EMA/size/depth,
  config/orderbook/positive mark-price key 집합 일치
- Orderbook: queue symbol/side/price, positive aligned price/size, original/remaining size,
  partial-fill minimum-notional semantics, duplicate/runtime sequence, IOC non-retention,
  depth/open-order limit, last trade tick alignment
- Accounts/positions: canonical lowercase address, non-negative locked collateral, bounded pending
  nonce, known market, position-size bound, zero/nonzero position과 entry-price 일관성
- Staking: validator/operator/node/BLS key와 PoP, stake/delegation/global totals, reward/commission,
  unbonding records, epoch snapshot/liveness, jail status, pending evidence의 실제 context/BLS 검증
- Trigger: runtime ID/sequence, market, positive size/trigger/limit price, CLOID uniqueness,
  side/type/condition, reduce-only/Pending status
- Oracle: config, source ID/price/weight/timestamp, source uniqueness/count, weighted median,
  confidence와 stored aggregate 일치
- Global: non-negative insurance fund, funding market/rate/sample/timestamp, oracle가 참조하는
  market 존재, positive mark price와 valid optional EMA

Negative account balance는 liquidation/ADL 전 insolvency 상태에서 발생할 수 있어 허용한다.
현재 position 존재 여부도 pending trigger의 구조 불변식으로 강제하지 않는다. Position이 주문
배치 뒤 사라지는 정상 경로가 있기 때문이다.

## Mutation 경로 정렬

사후 검증이 transaction 하나 때문에 블록 전체를 무효화하지 않도록 runtime 입력 검증도 같은
규칙으로 맞췄다.

- `AddMarket`은 저장 전에 전체 `MarketConfig`를 검사한다.
- Oracle update는 unknown market, 빈/중복 source ID, non-positive price, 잘못된 weight,
  future source timestamp를 저장 전에 거부한다.
- Trigger placement는 non-positive size/limit price와 빈 CLOID를 sequence/index mutation 전에
  거부한다.
- Maker fee는 현재 accounting semantics에 맞춰 bounded negative rebate를 허용하고, taker
  fee는 non-negative로 유지한다.

## Snapshot과 PoP

Staking의 consensus context/domain은 snapshot에 직렬화되지 않는 runtime trust input이다.
Snapshot import는 primary 검증 전에 node가 제공한 `chain_domain`을 staking state에 다시
설정한다. 따라서 다른 chain domain으로 만든 validator BLS proof-of-possession은 import에서
실패한다. Primary 검증을 통과한 복사본에서만 orderbook/staking/trigger derived index를
원자적으로 재구축하고 원본 state를 교체한다.

## 성능 결정

Primary validation은 전체 authoritative state에 대해 O(S), derived validation은 open
order/validator/trigger 수에 대해 O(D)이다. Full-state shadow root도 O(S)이므로 현재 후보
생성 경로는 같은 블록에서 세 개의 선형 단계를 가진다. 검증 결과와 root는 candidate에
보존하여 반복 preflight/commit 비용은 추가하지 않았다.

현재 validator 수는 curated phase 상한 21이고 PoP/evidence BLS 검증 비용도 제한된다. 그러나
큰 account/order/trigger 상태에서 매 블록 full scan은 장기적으로 적합하지 않다. Activation
전 component tree/dirty-subtree 검증과 cardinality/byte limit, adversarial benchmark가
필요하다.

## 검증 결과

- `cargo test --locked --all-targets --all-features`: 전체 통과
  - library 524 passed, 0 failed, 0 ignored
  - node 4, multinode binary 1, e2e 98 및 나머지 integration/bench target 통과
- `cargo check --locked --all-targets --all-features`: 통과
- invalid primary canonical state: candidate 미생성, canonical/event 미발행
- invalid snapshot mark price와 wrong-domain validator PoP: import fail-closed
- Oracle/trigger invalid input: primary mutation 전에 거부되고 부분 상태 없음
- `cargo fmt --check`, `git diff --check`: 통과
- persistent local `hl-node` restart: 동일 RocksDB에서 committed height `0 -> 3 -> 5`

All-features debug benchmark의 5,000 deposit block execute는 약 225 ms였다. 이 값은 payload
검증, 실행, artifact, state hash를 모두 포함하고 release 측정은 아니므로 throughput claim으로
사용할 수 없다. Primary validation을 분리 측정하는 release benchmark와 large live orderbook/
trigger workload는 activation 전 성능 gate로 남긴다.

## 다음 단계

1. Snapshot/Sync byte 및 record cardinality limit과 large-orderbook/trigger adversarial benchmark
2. ~~불완전한 legacy `incremental_hash` 경로 제거~~ — 2026-08-13 완료
3. ~~Fixed component-tree schema-v3 shadow 구현~~ — 2026-08-13 완료
4. Component dirty-subtree cache와 fresh component-root 교차 검증
5. `app_hash = H(version, full_state_root, receipt_root, event_root)` activation 규칙 구현
6. Cross-validator disagreement, restart, Byzantine/chaos 및 snapshot manifest/proof 검증
7. 이후 indexer bounded-range/cursor API
