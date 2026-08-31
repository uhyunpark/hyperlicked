# Versioned State Root 및 Runtime Parity 작업 로그

> 날짜: 2026-08-14
> 상태: 로컬 개발 검증 완료, **NOT MAINNET READY**

> 후속 상태: 이 문서가 남긴 consensus activation은 같은 날
> [state-root activation 작업](WORKLOG_2026-08-14_CONSENSUS_STATE_ROOT_ACTIVATION.md)에서 완료됐다.

## 의도

직전 단계의 schema-v3 full-state root는 결정론적 상태를 계산했지만 디스크에는 bare hash로
저장됐다. 이 형식은 향후 root schema가 바뀌었을 때 예전 hash를 새 schema의 hash로 오인할
수 있다. 또한 개발용 `multinode`가 raw `AppState`를 runner에 연결하면 `hl-node`가 사용하는
speculative/commit 경계와 Commitment v2/state-root preflight를 건너뛸 수 있었다.

이번 단계는 wire protocol을 바꾸지 않고 다음 두 경계를 고정한다.

1. durable root row가 자신이 어떤 schema로 계산됐는지 명시한다.
2. 실제 실행 binary는 모두 `CanonicalAppHook`을 통해 같은 application lifecycle을 사용한다.

## 변경

### Versioned durable state-root record

- RocksDB `state_roots` 값은 `(u16 little-endian schema_version, [u8; 32] root)`의 34-byte
  canonical record로 저장한다.
- 현재 version은 component-tree schema v3와 단일 상수로 연결한다.
- finalized block, consensus state, Commitment v2, state root를 기존 synced write batch 안에서
  함께 기록한다.
- 같은 block hash에 동일 record를 다시 기록하는 retry는 허용하고, 다른 root나 손상된 기존
  record를 덮어쓰는 것은 거부한다.
- load/restart 시 raw 32-byte legacy 값, 미지원 version, truncated/trailing bytes를 거부한다.

프로젝트가 아직 public network가 아니므로 자동 migration은 넣지 않았다. 잘못된 형식을
추측해서 변환하는 것보다 개발 DB를 명시적으로 다시 만드는 쪽이 안전하다.

### Runtime parity

- `hl-node`와 `multinode` 모두 `AppState -> SharedState -> CanonicalAppHook -> ConsensusRunner`
  경계를 사용한다.
- 따라서 두 executable 모두 candidate state에서 실행하고, fresh full-state root와 Commitment
  v2를 preflight한 뒤 durable persist와 canonical commit 순서를 따른다.
- deterministic BLS key와 loopback peer를 쓰는 `multinode` 자체는 계속 개발 fixture이며
  production 배포 profile이 아니다.
- Runner lifecycle mock과 legacy Engine/direct-state 테스트는 의도적으로 바꾸지 않았다.
  이들은 특정 hook 동작이나 state transition 단위를 검사하며 live runtime 경로가 아니다.

## 검증 기준

- current schema record encode/decode round-trip
- unsupported version, raw legacy root, truncated/trailing record 거부
- state root + Commitment v2 + finalized metadata atomic round-trip
- RocksDB reopen 후 동일 root 복원 및 손상 row fail-closed
- commit retry의 immutability
- `multinode`가 canonical commitment/state-root preflight를 실제 호출
- 전체 feature/target test에서 실패 및 ignored test 0개

## 아직 하지 않은 것

이 단계 시점에는 state root가 block `app_hash`나 QC가 인증하는 consensus commitment가 아니었다. 이를 바로
wire field에 넣으면 기존 validator와 새 validator가 서로 다른 block/QC를 만들 수 있으므로,
다음 단계에서 protocol version과 activation height를 먼저 고정해야 한다.

이후 순서는 다음과 같다.

1. activation rule과 block-result encoding 정의
2. state root를 block/QC 서명 대상에 결합
3. mixed-version/activation-boundary/restart 테스트
4. trusted finalized anchor 기반 verified snapshot 및 block import

후속 genesis-wide V4 activation으로 1~3은 완료됐다. verified snapshot/import와 독립 감사가
남아 있으므로 여전히 mainnet-ready commitment라고 주장하지 않는다.
