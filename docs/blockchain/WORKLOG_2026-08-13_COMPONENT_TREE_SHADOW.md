# Fixed component-tree shadow root 작업 기록 (2026-08-13)

> 후속 상태: 이 문서의 shadow root는 2026-08-14
> [consensus activation](WORKLOG_2026-08-14_CONSENSUS_STATE_ROOT_ACTIVATION.md)에서
> `Block::app_hash`와 Vote/QC에 결합됐다. 아래 내용은 활성화 전 구현 기록이다.

## 결론

기존 flat full-state shadow preimage을 변경하지 않고, 별도 schema v3/domain으로 fixed
component-tree root를 추가했다. `AppState::compute_full_state_root()`는 이제 매 호출마다
다음 9개 component를 canonical encoder로 새로 직렬화하고 leaf hash를 계산한다.

`metadata -> accounts -> orderbooks -> market_configs -> prices -> funding -> staking ->
triggers -> oracle`

각 leaf는 component domain, schema v3, 고정 index, component 이름, canonical bytes를
포함한다. 부모 root는 root domain, schema v3, chain domain, component count, 각 index/name/
leaf hash를 포함한다. 따라서 component 순서 변경, root/leaf domain 혼동, 다른 chain 재사용이
모두 별도 root를 만든다.

## 경계

- `AppState`에는 root cache가 아니라 직렬화되지 않는 transient dirty bitmask만 둔다. 검증된
  부모 candidate가 있으면 해당 bit의 leaf만 재인코딩하고, root와 나머지 leaf는 candidate
  tree에서 파생한다. 새 상태·unknown bit·chain-domain 변경·recovery는 9개 leaf를 모두
  fresh recompute한다. `AppState` clone은 tracker를 unknown/all로 리셋해 branch가 clean
  baseline을 상속하지 않게 하며, 검증된 child/trial clone만 각각 clean/preserved-mask
  경계를 사용하고 Candidate 생성 뒤 tracker는 clean으로 되돌린다.
- Canonical speculative candidate가 한 번 보관한 root seal은 preflight뿐 아니라 direct
  `commit`에서도 candidate state의 전체 tree를 fresh recompute해 재검증한다. 불일치하면
  state-corrupted 상태로 fail-closed하며, 이 shadow root는 `Block::app_hash`와 연결되지
  않는다. 따라서 dirty derivation은 최적화일 뿐 안전성 경계가 아니다.
- 기존 schema-v2 flat encoder와 golden vector는 compatibility audit용으로 보존했다.
- `Block::app_hash`, vote, QC semantics와 protocol activation은 변경하지 않았다.
- RocksDB state-root row는 이제 little-endian schema-v3 tag와 32-byte hash를 포함하는
  고정-width versioned record으로 저장한다. decoder는 unsupported schema, raw legacy 32-byte
  row, truncated/trailing bytes를 모두 거부한다. 기존 v2/raw shadow DB migration은 만들지
  않았으며, 해당 row는 삭제 후 v3 record로 다시 생성해야 한다.
- transient queue/cache 및 derived index는 기존 schema-v2 경계와 동일하게 component bytes에서
  제외된다. Snapshot은 primary state를 복구한 뒤 같은 component tree를 재계산한다.
- 블록 phase의 invalidation은 metadata(높이/view/timestamp), accounts(거래·청산·funding
  적용), funding(premium sample/rate 적용), staking(epoch transition/reward) 경계로
  나뉜다. 미지의 bit와 누락 가능성이 있는 복구 경로는 항상 전체 recompute로 fallback한다.
- `restore_speculative_chain`은 nonzero canonical height에서 committed replay가 먼저 주입한
  trusted exact head hash 없이는 anchor를 받지 않는다. 복구 branch 전체를 임시 hook의 빈
  candidate map에서 replay하고 모든 블록이 검증된 뒤에만 후보와 anchor hash를 publish한다.
  valid child 뒤 invalid grandchild가 와도 기존 candidate-map, committed hash, canonical
  state는 그대로 유지된다.
- `multinode` fixture도 `ConsensusConfig`의 genesis/context를 `AppState`에 주입해 validator별
  zero-domain shadow root가 생기지 않게 했다.

## 검증

- schema-v3 component-tree golden vector
- fixed ordering/domain separation
- 각 authoritative component mutation이 해당 leaf와 최종 root만 변경
- map insertion-order independence
- transient/derived exclusion
- snapshot round-trip root stability
- `cargo test --locked --all-features --lib app::state::full_state_hash::tests -- --nocapture`
- component-tree/dirty-derivation tests: 11 passed, 0 failed, 0 ignored
- canonical candidate/preflight/commit/recovery tests: 23 passed, 0 failed, 0 ignored
- `cargo check --locked --all-targets --all-features`
- `cargo fmt --all -- --check`
- fresh schema-v3 RocksDB에서 `hl-node` restart/commit `0 -> 3 -> 5` 통과
- `cargo check --locked --all-targets --all-features` passed; focused component-tree and
  canonical suites passed after the dirty-subtree safety-boundary changes.

2026-08-14 local release benchmark의 full-state root fresh recomputation은 account/deposit
workload에서 100 accounts `0.213 ms`, 1,000 accounts `1.728 ms`, 5,000 accounts
`9.466 ms`였다. 동일 크기의 child candidate execution은 각각 `0.525 ms`, `6.260 ms`,
`28.603 ms`였지만 여기에는 transaction execution, invariant validation, state clone,
component derivation이 함께 포함되므로 fresh-root 숫자와 직접 비교할 수 없다. 또한
preflight/direct commit은 안전성을 위해 여전히 전체 tree를 fresh 검증한다. 이는 large live
orderbook/staking/trigger workload를 대표하지 않으며 정확성·회귀 baseline으로 사용한다.

Dirty-subtree derivation은 fresh full-tree cross-check를 동반하는 shadow 최적화일 뿐이며,
persisted cache가 아니다. Proof/manifest와 app-hash consensus activation은 후속 tranche다.
