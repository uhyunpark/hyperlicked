# Schema-v3 State Root Consensus Activation

> 날짜: 2026-08-14
> 상태: 로컬 genesis-wide hard fork 완료, **NOT MAINNET READY**

## 의도

이전 단계의 schema-v3 full-state root는 deterministic하고 durable했지만 block/QC가
인증하지 않는 shadow artifact였다. 공격자나 결함 노드가 서로 다른 full state를 만들더라도
legacy `app_hash`만 같으면 그 차이가 consensus certificate에 직접 드러나지 않는 구조였다.

공개 네트워크와 호환해야 할 기존 chain이 없으므로 높이별 migration 분기를 만들지 않고
genesis부터 하나의 protocol로 활성화했다.

## 활성화 규칙

- `Block::app_hash`는 schema-v3 nine-component full-state root 자체다.
- block hash domain은 V4이며 `app_hash`를 preimage에 포함한다.
- proposer signature, Vote와 QC는 기존 block-hash/app-hash 서명 경로를 통해 root를 인증한다.
- genesis domain V2는 block-hash protocol V4와 state-root schema v3를 포함한다. 구버전
  validator의 timeout/view-change/control-plane 메시지도 같은 chain context로 인증되지 않는다.
- 기존 V3 DB, V1 genesis domain과 PoP는 migration하지 않는다. local fixture PoP를 V2 domain으로
  다시 생성했으며 기존 개발 DB는 새 data directory에서 시작해야 한다.

## 실행 및 저장 경계

- leader는 실행 root와 fresh preflight root가 일치하지 않으면 proposal을 broadcast하지 않는다.
- follower/observer는 local execution, fresh preflight, block header가 모두 같기 전에는 vote하거나
  block을 받아들이지 않는다.
- canonical candidate의 dirty-subtree tree와 fresh full tree가 다르면 fail closed한다. dirty tracking은
  최적화일 뿐 consensus oracle이 아니다.
- finalized storage는 non-genesis state-root row가 `Block::app_hash`와 다르면 synced batch 전에 거부한다.
- application chain domain과 block genesis domain이 다르면 payload 실행 전에 거부한다.
- `ConsensusRunner`는 constructor 기본 `NoOpApp` 상태로 live loop를 시작할 수 없다. `with_app`으로
  application hook을 명시적으로 장착해야 한다.

## 순환성 및 성능

Full-state root preimage에는 block hash나 transient Commitment v2 artifact가 포함되지 않는다.
Execution artifact도 최종 block hash를 commitment root preimage에 넣지 않으므로
`state root -> app_hash -> block hash`에 순환 의존성이 없다.

새 32-byte wire field는 추가하지 않았다. 기존 `app_hash`를 사용하므로 block, vote, QC 크기는
변하지 않는다. 실행 시 full root 계산은 이전 shadow preflight에서도 수행되던 작업이다.
안전성을 위해 vote와 commit 경계의 fresh recomputation은 유지한다.

## 검증

- schema-v3 root와 `AppState::execute()` 결과 동일성
- root mutation에 따른 V4 block hash 변경
- wrong-root proposal이 safety persistence/vote 전에 거부됨
- missing explicit application hook live-start 거부
- legacy genesis-domain V1과 V2 domain 분리
- application/block chain-domain mismatch 거부
- storage root와 authenticated `app_hash` mismatch 거부
- 갱신된 single/4-validator genesis PoP validation
- single-node fresh start `0 -> 1` 및 같은 RocksDB restart `1 -> 2`
- `cargo test --locked --all-targets --all-features`

## 남은 mainnet gate

- ~~Commitment v2 receipt/event root의 별도 consensus activation~~ — 2026-08-21 완료.
  [activation worklog](WORKLOG_2026-08-21_COMMITMENT_V2_CONSENSUS_ACTIVATION.md)
- application-executed verified block import 및 chunked snapshot manifest/proof
- epoch transition certificate와 historical committee registry
- bridge proof/accounting, 운영 보안, 장기 WAN/Byzantine 테스트와 독립 감사
