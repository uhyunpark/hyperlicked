# Speculative AppState COW 작업 기록 (2026-08-23)

> 상태: 로컬 개발 체인의 구현·검증 기록. **MAINNET READY 판정이 아니다.**

## 목적

합의가 commit 전에 여러 proposal branch를 보관할 때 각 candidate가 `AppState` 전체를 deep
clone하면, candidate 수 상한이 있어도 계정·호가·mempool 크기에 비례해 메모리가 급증한다.
이번 작업은 consensus safety 규칙을 바꾸지 않고 speculative state를 versioned copy-on-write
(COW) snapshot으로 전환해, sibling candidate가 읽기 전용 데이터를 공유하고 실제로 수정한
부분만 분리하도록 만드는 것이 목적이다.

## 구현 결과

- `Shared<T>`는 `Arc<T>` 기반의 얕은 clone과 `Arc::make_mut` 기반 mutation isolation을
  제공한다. 계정, orderbook, oracle, candle, trigger, staking 및 주요 state collection에
  적용했다.
- 계정은 64개, orderbook 파생 index는 32개 shard로 나눴다. 한 계정/호가 변경이 전체
  collection을 복사하지 않고 대상 shard와 값만 분리한다.
- orderbook price level, candle queue, oracle symbol entry, trade/trigger map은 outer map과
  개별 value를 분리하는 nested COW를 사용한다.
- speculative child는 node-local mempool을 복제하지 않는다. candidate는 branch의
  `proposed_tx_hashes`로 중복 proposal을 막고, commit 시점에 현재 canonical mempool을 다시
  결합한 뒤 committed transaction만 제거한다. 따라서 proposal 후 늦게 들어온 transaction과
  abandoned branch의 transaction이 유실되지 않는다.
- per-block pending output과 execution artifact는 child 생성 시 pointer replacement로
  초기화한다. Candidate admission/replay/commit의 state-root 및 Commitment v2 검증 규칙은
  유지한다.
- snapshot state-root schema v3를 사용한다. 이 저장 형식은 v2와 byte-compatible하지 않지만,
  현재 체인은 공개되지 않은 local dev 상태이고 migration compatibility가 필요 없다는 사용자
  결정에 따라 기존 DB reset을 전제로 한다.

## 메모리 관찰값

`cargo bench --locked --bench state_cow`의 allocator 관찰값이다. 절대 성능 SLA가 아니라 같은
workload의 구조 변경 전후 비교이며, 환경에 따라 시간과 세부 allocation 값은 달라질 수 있다.

| 16개 sibling candidate | 변경 전 | COW/sharding 후 |
|---|---:|---:|
| clone-only allocated bytes | 약 25.46 MB | 6,272 B |
| consensus mutation allocated bytes | 약 25.46 MB 이상 | 약 3.29 MB |
| consensus + candidate-local mempool mutation | 약 34.46 MB | 약 20.47 MB |

마지막 행은 production candidate가 사용하지 않는 mempool mutation까지 의도적으로 포함한
stress 항목이다. 정상 speculative child는 빈 candidate mempool과 canonical reconciliation을
사용하므로 clone-only/consensus-mutation 값이 더 직접적인 지표다.

## 안전성 불변조건

1. Parent와 sibling은 child mutation으로 바뀌지 않는다.
2. Candidate 실행은 canonical state와 canonical mempool을 직접 mutate하지 않는다.
3. Commit은 exact block hash, height, state root, Commitment v2를 확인한 candidate만 publish한다.
4. Candidate/replay 실패는 기존 live candidate set을 부분적으로 교체하지 않는다.
5. Consensus 밖에서 도착한 equivocation 증거는 canonical staking state를 즉시 mutate하지 않고,
   블록 payload의 결정론적 실행을 거쳐서만 반영한다.

## 검증

- `cargo test --locked --lib`: 645 passed, 0 failed, 0 ignored (2026-08-25 static staking hardening 포함)
- `cargo test --locked --bin hl-node`: 8 passed
- `cargo test --locked --bin multinode`: 3 passed
- `cargo check --locked --all-targets`: 통과
- `cargo check --locked --all-targets --all-features`: 통과
- `cargo test --locked --all-features --lib consensus::engine::tests`: 8 passed
- `cargo fmt --all -- --check`, `git diff --check`: 통과
- fresh `hl-node` 2블록 commit 및 같은 RocksDB의 2→3블록 restart recovery: 통과

## 남은 범위

- Staking의 큰 nested collection은 collection-level COW다. 일반 block reward는 scalar 변경이라
  이득이 있지만 validator/delegation이 매우 커지는 permissionless 단계 전에는 값 단위 sharding
  또는 versioned storage가 필요하다.
- Candidate의 `Block` payload와 `proposed_tx_hashes`는 candidate-local 메모리다. 후보 수 16과
  payload 상한은 있지만 aggregate application-candidate byte budget은 아직 없다.
- Mark-price/EMA map은 scalar value이므로 우선순위가 낮지만 symbol 수가 크게 늘면 shard 또는
  value-level COW를 검토한다.
- 이 작업은 in-memory speculative execution 개선이다. 장기적으로는 상태를 RocksDB 위의
  immutable version/overlay로 두고 snapshot materialization을 줄이는 구조가 더 적합하다.
