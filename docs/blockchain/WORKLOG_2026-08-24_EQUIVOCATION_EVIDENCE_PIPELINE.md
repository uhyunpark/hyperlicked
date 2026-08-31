# Equivocation evidence pipeline 작업 기록 (2026-08-24)

> 상태: 로컬 개발 체인의 mainnet hardening 기록. **전체 체인의 MAINNET READY 판정이 아니다.**

## 문제

Consensus runner는 같은 validator가 같은 view에서 서로 다른 block에 서명한 사실을 감지하고
두 BLS vote를 `EquivocationProof`로 만들 수 있었다. 그러나 production
`CanonicalAppHook::submit_equivocation_evidence`가 항상 `false`를 반환해 실제 staking
slashing으로 이어지지 않았다.

단순히 네트워크 수신 시 canonical staking state를 직접 변경하는 수정도 안전하지 않다. 증거를
관측하는 시점과 순서는 노드마다 다르므로, 그렇게 하면 동일한 finalized chain을 실행한 노드들의
state root가 달라질 수 있다. 또한 증거가 leader의 메모리에만 있으면 view change나 process
restart 때 사라진다.

## 선택한 구조

```text
verified double vote
  -> canonical proof 정규화 및 committee/BLS 검증
  -> bounded RocksDB evidence journal에 sync 기록
  -> evidence 전용 local proposal queue
  -> self-authenticating proof gossip
  -> proposer가 System(SubmitEvidence)를 block payload에 포함
  -> 모든 follower가 동일 proof를 block preflight에서 재검증
  -> durable block commit
  -> canonical staking slash 및 evidence journal GC
```

전체 user/system mempool persistence와 generic transaction gossip을 새로 만드는 대신, 크기가 작고
자체 BLS 인증되는 double-vote proof만 전용 경로로 다룬다. 현재 요구에 필요한 최소 범위이며
일반 transaction gossip의 nonce·expiry·spam 정책까지 함께 바꾸지 않는다.

## 결정론 및 권한 경계

- 두 signed vote tuple은 canonical 순서로 정렬한다. 노드가 A/B를 반대로 관측해도 같은 proof와
  transaction identity가 생성된다.
- 로컬 관측 시각은 인증된 proof가 아니므로 evidence timestamp를 `0`으로 고정한다.
- `SubmitEvidence`는 `ConsensusTransaction::System`에서만 허용한다. Signed user envelope는
  admission·block validation·execution에서 거부한다.
- System evidence의 submitter는 `system:equivocation:<full offender hex>` 형식만 허용한다.
- context, offender validator, 두 96-byte BLS signature, 서로 다른 block hash를 follower가
  application mutation 전에 모두 검증한다. 하나라도 틀리면 failed receipt가 아니라 block
  자체를 거부한다.
- 네트워크 ingress는 mempool/journal만 변경한다. Validator status와 stake는 증거 transaction이
  finalized block에서 실행될 때만 바뀐다.
- 이미 tombstoned된 validator에 대한 유효한 중복 proof는 재슬래시하지 않는다.

## 자원 및 복구 경계

- Evidence는 일반 transaction age eviction 대상이 아니다.
- 일반 bucket-0/per-address spam이 evidence를 막지 못하도록 별도 proposal reserve를 사용한다.
  Context/offender당 하나만 보관하고, count cap에서 FIFO eviction하지 않고 fail closed한다.
- RocksDB는 전용 evidence column family를 사용한다. Context/offender key는 first-write-wins이며,
  journal은 최대 256 records / 1 MiB다.
- Journal wire signature는 고정 `[u8; 96]` 형식이라 corrupt length가 대형 `Vec` 할당을 유도하지
  못한다. Malformed/noncanonical/mis-keyed/over-cap row는 startup/load에서 fail closed한다.
- Proof는 application queue나 gossip보다 먼저 sync journal에 기록한다. Broadcast 실패나 crash가
  발생해도 다음 round/restart가 journal을 다시 enqueue/relay한다.
- Journal 삭제는 block durable commit 이후에만 수행한다. Commit 뒤 delete 전 crash는 proof를
  남길 뿐이며, recovery의 tombstone/no-op과 committed-chain reconciliation으로 정리할 수 있다.
- Live runner는 store가 journal의 load/save/delete 세 연산을 모두 지원한다고 명시하지 않으면
  시작하지 않는다. 수신 증거의 journal write 실패도 warn-and-continue가 아니라 fail-stop이다.
- Gossip envelope ID는 payload에서 다시 계산한다. 검증된 `(context, offender)` 전용 bounded cache로
  동일 범죄의 다른 proof variant도 중복 제거하되, 위조 proof는 cache를 오염시키지 못한다.
- Direct mode는 gossip용 주기적 재전파를 수행하지 않는다. Durable journal recovery와 block proposal
  입력 복구는 유지하므로 전파 설정이 application 결정론을 바꾸지 않는다.

## 검증 항목

- valid ingress가 canonical state root를 바꾸지 않고 proposal input만 추가하는지
- forged/wrong-context/noncanonical System evidence block이 mutation 전에 거부되는지
- Signed `SubmitEvidence`가 admission과 block validation에서 거부되는지
- A/B swap과 중복 proof가 하나의 pending item 및 한 번의 slash로 수렴하는지
- ordinary mempool 포화 상태에서도 evidence reserve가 작동하는지
- journal save/load/reopen/cap/malformed/delete isolation이 지켜지는지
- restart 후 pending proof가 다시 제안되고, leader change 뒤 새 leader에게 relay되는지
- durable evidence block commit 뒤 local variant proof와 journal key가 함께 제거되는지

## 최종 검증

- `cargo test --locked --lib`: 645 passed, 0 failed, 0 ignored (2026-08-25 static staking hardening 포함)
- `cargo test --locked --lib network::`: 75 passed
- `cargo test --locked --lib consensus::runner::tests`: 42 passed
- `cargo test --locked --lib app::staking::slashing::tests`: 8 passed
- `cargo test --locked --lib app::state::consensus::tests`: 23 passed
- `cargo test --locked --lib node_config::tests`: 15 passed
- `cargo test --locked --bin hl-node`: 8 passed
- `cargo test --locked --bin multinode`: 3 passed
- fresh `hl-node`가 2블록을 commit하고, 같은 RocksDB로 재시작해 2→3블록 복구·진행
- `cargo fmt --all -- --check`, `git diff --check`: 통과

## 정적 curated committee와 staking 경계

- `hl-node`와 `multinode`는 합의 설정의 Committee/BLS 키를 application runtime에 다시 주입한다.
  이 runtime binding은 snapshot/serde와 state-root preimage에서 제외되므로, binding 자체가 root를
  바꾸지 않는다. Snapshot 복구 후에는 committee를 재주입하기 전까지 evidence를 fail closed한다.
- 새 canonical `AppState`는 합의 Committee만 알고 staking validator map은 비어 있었기 때문에,
  이전에는 실제 curated member의 proof가 검증되어도 `get_validator_by_node`에서 slash target을
  찾지 못했다. 이제 genesis의 PoP-bearing bootstrap record를 먼저 등록하고 정적 epoch snapshot을
  seed한 뒤 runtime committee를 bind한다. 따라서 유효한 member proof는 동일한 finalized block
  실행에서 tombstone/slash된다.
- 현재 local genesis schema에 operator와 경제적 stake가 없으므로 local/static mapping은
  `system:genesis:<node-id>`, `voting_power * MIN_SELF_STAKE`, commission `0`으로 고정한다.
  명시 필드를 넣더라도 이 관계를 만족해야 한다. 이는 개발용 deterministic bootstrap이며, 실제
  mainnet genesis에서는 operator, self-stake, commission 및 PoP를 명시적인 경제적 genesis 데이터로
  정의해야 한다. 이 mapping을 permissionless PoS의 최종 모델로 간주하지 않는다.

## 남은 범위

- 현재 slashing verifier는 정적 epoch-0 committee context에 묶여 있다. Permissionless validator-set
  activation 전에는 historical committee lookup과 evidence expiry/window 정책이 필요하다.
- Double-propose evidence는 vote와 서명 domain이 달라 현재 fail closed다. 전용 proposer-proof
  verifier를 추가하기 전에는 활성화하지 않는다.
- Journal은 local durability를, gossip은 leader-change delivery를 담당한다. 장기 network partition의
  재동기화 정책과 운영 alert/metrics는 별도 mainnet observability 단계에서 강화해야 한다.
