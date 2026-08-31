# Legacy incremental hash 제거 작업 기록 (2026-08-13)

## 결론

서로 다른 bucket preimage을 사용하던 legacy `incremental_hash` feature와
dirty-tracking machinery를 제거했다. 이제 모든 build에서 `AppState::compute_state_hash()`가
동일한 legacy full-state application hash를 사용하므로 feature 조합에 따른 validator 간
app-hash 불일치 경로가 없다.

당시 schema-v2 `compute_full_state_root()`는 authoritative state 기반 shadow commitment로
유지했다. 후속 tranche에서 별도 domain의 fixed component-tree schema v3로 교체했지만,
`Block::app_hash`, vote, QC에 결합하는 activation은 여전히 포함하지 않았다.

## 변경 범위

- Cargo feature와 `src/app/state/incremental_hash.rs` module/file 제거
- 실행 경로의 account/global dirty hook 및 호출 제거
- canonical app-hash와 shadow root의 분리·결정성 회귀 테스트 유지
- snapshot round-trip에서 shadow root가 보존되는 테스트 유지

## 검증

- `cargo test --locked --all-features --test app_hash_test`
- `cargo test --locked --no-default-features --test app_hash_test`
- `cargo test --locked --all-features --lib full_state_root`
- `cargo check --locked --all-targets --all-features`
- `cargo fmt --all -- --check`, `git diff --check`
- `cargo test --locked --all-targets --all-features`: library 537 passed,
  전체 target 0 failed, 0 ignored
- 동일 RocksDB를 사용한 `hl-node` restart: committed height `0 -> 3 -> 5`

Component tree/dirty-subtree 구현과 app-hash consensus activation은 별도 작업으로 남긴다.
