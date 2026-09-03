# Historical Committee 및 Epoch Transition 계획

> **상태: 설계 문서. 미구현.**
>
> 현재 HyperLicked의 canonical protocol phase는 signed genesis로 고정된 static
> epoch 0 curated committee다. 이 문서는 historical committee, epoch transition,
> delayed equivocation evidence를 mainnet에 넣기 위한 안전 경계와 구현 순서를 정의한다.
> 아래 내용을 일부만 활성화했다고 해서 permissionless PoS나 mainnet readiness가 달성되는
> 것은 아니다.

## 1. 범위와 가정

이 문서의 첫 전환 설계는 복잡한 tokenomics를 구현하지 않는다. 검증 가능한 최소 경제 모델은
다음과 같다.

- native HYCK의 bonded power만 voting power의 원천으로 사용한다.
- 한 validator의 bonded power는 self-bond와 위임된 HYCK의 합으로 계산하되, 보상·수수료·복잡한
  파생상품은 이 단계의 committee 선택에 포함하지 않는다.
- active committee는 bonded power 내림차순의 top 21이다. 동률은 canonical node/operator
  identifier의 lexicographic 순서로 결정한다.
- committee hash는 정렬된 `(node_id, BLS public key, bonded power)`의 canonical encoding으로
  계산한다. 모든 노드는 같은 finalized state에서 같은 결과를 재구성해야 한다.
- local bootstrap은 2~3 validator를 허용한다. 이 크기에서는 Byzantine 장애 허용을 주장하지
  않고 `f = 0` 운영 fixture로만 취급한다.
- 4 validator부터 이 문서의 local BFT fixture는 `f = 1` Byzantine tolerance를 시험한다.
  일반적으로 `f = floor((n - 1) / 3)`와 stake-weighted `> 2/3` quorum을 적용한다.
  2~3개 노드가 1 Byzantine을 견딘다는 의미가 아니며, validator 수가 적을 때 liveness와
  safety가 자동으로 보장된다고 주장하지 않는다.

이 가정은 전환 경계를 검증하기 위한 최소 모델이다. permissionless validator entry/exit,
commission, reward distribution, unbonding economics는 별도의 경제 설계와 감사를 거쳐야
한다.

## 2. 현재 구현 상태와 고정된 fail-closed 경계

현재 구현은 historical committee를 지원하지 않는다.

| 영역 | 현재 상태 | 근거 |
| --- | --- | --- |
| Committee config | config epoch이 0이 아니면 거부 | `src/types/config.rs:206-214` |
| Context validation | epoch 0과 현재 committee hash만 허용 | `src/types/config.rs:338-349` |
| Consensus proof | 하나의 현재 `Committee`와 expected context로 검증 | `src/consensus/committee.rs:82-142` |
| Runner | current config의 committee/context를 verifier에 전달 | `src/consensus/runner.rs:1171-1179` |
| Network | 하나의 committee/context를 gossip validation에 보관 | `src/network/mod.rs:70-80,127-145` |
| App staking | static epoch-0 authoritative committee에 묶임 | `src/app/staking/slashing.rs:74-142` |
| Epoch update | dynamic validator update가 아직 panic 경계 | `src/consensus/runner.rs:3030-3038`, `src/consensus/engine.rs:930-940` |
| Proof schema | `EquivocationProof`와 `Vote`에 offense height가 없음 | `src/consensus/equivocation.rs:46-85`, `src/types/certificate.rs:10-33` |
| Snapshot | runtime committee/context를 복구 전까지 비움 | `src/app/state/consensus.rs:497-508,642-649` |

따라서 현재의 거부 동작은 결함이 아니라 안전한 static boundary다. 다음 동작은 historical
registry가 구현되기 전까지 계속 거부해야 한다.

- transition certificate 없는 epoch activation
- current committee와 다른 context의 vote/QC/evidence
- registry에서 정확히 조회되지 않는 historical evidence
- snapshot/restart 후 committee binding 전에 수행되는 evidence 검증
- 검증되지 않은 validator update를 pacemaker, network, app state에 반영하는 것

### 2.1 왜 부분 활성화하면 안 되는가

historical 기능을 registry, expiry, network 중 일부에만 넣으면 다음과 같은 합의 분기가 생긴다.

1. current committee verifier가 old proof를 잘못 거부하거나, 반대로 current key를 old proof에
   적용한다.
2. runtime registry는 과거 committee를 알고 있지만 snapshot/state root에는 없어 restart한
   노드가 다른 committee로 같은 proof를 검증한다.
3. gossip에서 통과한 evidence가 block execution의 current-context 검증을 우회하거나, app
   mutation 뒤에 expiry가 발견된다.
4. 만료된 journal row를 recovery에서 reject만 하면 재시작 때마다 같은 row에서 멈춘다.
5. unbonding 자산을 evidence window 전에 claim할 수 있어, 늦게 도착한 유효한 proof가 slash할
   자산을 잃는다.

이 문서의 전환은 `proof verification → authenticated registry → state transition → durable
activation → runtime swap`을 하나의 경계로 다룬다. 어느 하나라도 빠지면 해당 전환은 활성화하지
않고 fail closed한다.

## 3. 최소 `EpochTransitionProof`

epoch `e`의 old committee가 finalized transition block을 인증하고, 다음 committee가 언제부터
유효한지 증명하는 최소 wire object를 정의한다. 필드는 모두 canonical encoding과 versioned
domain으로 서명/해시되어야 한다.

```text
EpochTransitionProof {
    schema_version: u16,
    old_context: ConsensusContext,
    old_qc: QuorumCertificate,
    next_epoch: u64,
    next_committee: Vec<CommitteeMember>,
    next_committee_hash: Hash32,
    effective_height: u64,
    state_root: StateRootReference,
}

StateRootReference {
    height: u64,
    schema_version: u16,
    root: Hash32,
}
```

최소 필드의 의미는 다음과 같다.

- `old_context`: QC가 어떤 chain/genesis/epoch/committee domain에 속하는지 고정한다.
- `old_qc`: old committee의 stake-weighted quorum이 transition block을 finalized했다는
  증거다. QC 안의 block hash, height, view는 transition block header와 일치해야 한다.
- `next_committee`와 `next_committee_hash`: 다음 committee를 노드가 다시 계산할 수 있게 한다.
  hash만 전달하고 members를 외부 설정에서 읽으면 검증 결과가 달라질 수 있으므로, 최소 proof에는
  canonical member material을 포함한다.
- `effective_height`: 다음 committee가 처음 유효한 block height다.
- `state_root`: next committee 후보와 bonded power를 계산한 finalized state의 인증 anchor다.
  단순히 proposer가 주장한 next set이나 로컬 config를 신뢰하지 않는다.

이 문서의 height convention은 명확히 고정한다.

- transition block은 old context의 height `H - 1`이다.
- `old_qc`는 해당 transition block을 인증한다.
- `state_root.height == H - 1`이고 transition block의 committed state root와 같다.
- `effective_height == H`이며, height `H`가 first-new-epoch block이다.

현재 `Vote`/`EquivocationProof`에는 height가 없으므로, 이 proof를 기존 evidence schema에
임의로 unsigned height로 덧붙여 expiry에 사용할 수 없다. height 기반 evidence가 필요해지는
시점에는 `Vote` signing data, proof/evidence wire schema, full-state encoding, journal version을
동시에 version bump해야 한다. 그 전까지 evidence window는 서명된 `view` 기반으로만 둔다.

## 4. Transition proof 검증 규칙

각 노드는 다음 순서로 proof를 검증한다.

1. `old_context.genesis_hash`가 local chain domain과 같고, `old_context.epoch`가 현재
   finalized epoch와 정확히 다음 전환 관계인지 확인한다.
2. `old_qc`의 block hash, parent, height, view, context를 저장된 finalized transition block과
   비교한다.
3. old historical registry record의 committee로 old QC origin, BLS signature, quorum power를
   검증한다. current committee로 old QC를 검증하지 않는다.
4. `state_root`가 transition block의 authenticated root와 일치하고, 해당 state를 replay 또는
   verified snapshot anchor로 확인한다.
5. 그 state에서 HYCK bonded power를 계산해 top 21과 tie-break를 재구성한다.
6. 재구성한 members의 canonical hash가 `next_committee_hash`와 같고, 각 BLS key/PoP,
   node identity, positive bonded power, 중복 여부를 검증한다.
7. `effective_height == old_qc.block_height + 1`을 확인한다. height skip, rollback, 이전 epoch
   재활성화는 거부한다.
8. 모든 검증이 끝난 뒤에만 transition proof를 registry candidate와 runtime activation candidate로
   저장한다.

proof의 `next_committee`가 state root에서 계산한 결과와 다르면 failed receipt로 처리하지 않고
block/transition 자체를 거부한다. transition certificate 없는 configuration reload, gossip
message, local environment variable은 consensus input이 아니다.

## 5. First-new-epoch block 규칙

height `H`의 first-new-epoch block은 old parent와 new child 사이의 명시적인 cross-context
경계다.

```text
old transition block (H - 1, context e)
        │ old_qc + state_root + transition proof
        ▼
first new block (H, context e + 1, next_committee_hash)
```

필수 규칙:

- height `H` block의 parent는 반드시 finalized old transition block height `H - 1`이다.
- block header context는 `EpochTransitionProof.next_committee_hash`와 일치해야 한다.
- first-new-epoch block에는 transition proof가 포함되거나, 동일 proof가 이미 durable activation
  record로 존재해야 한다. 둘 중 어느 경우든 block validation이 먼저 proof를 재검증한다.
- block `H`의 proposer signature와 new-context vote는 new committee로 검증한다.
- old committee는 height `H`의 new context에 투표할 수 없고, new committee는 old context의
  height `H - 1` transition block을 새로 finalize할 수 없다.
- height `H`에 old context block을 계속 제안하거나 `H + 1`로 건너뛰는 것은 거부한다.
- first-new block이 거부되면 runtime committee를 부분 교체하지 않는다. node는 old finalized
  context에서 transition candidate를 다시 검증하거나, 동기화된 valid proof를 받을 때까지
  vote/propose를 멈춘다.
- 이후 block은 new context/QC만 사용한다. old QC는 historical evidence와 transition audit에
  남지만 new-epoch consensus certificate로 재사용되지 않는다.

view numbering은 implementation 전에 고정해야 한다. 현재 proof가 `(epoch, view)`를 서명하므로
전환 시 view를 재사용한다면 epoch/context domain이 반드시 preimage에 포함되어야 한다. 가장
안전한 선택은 transition block이 정의한 next view를 deterministic하게 사용하고, pacemaker
state를 그 값과 함께 durable하게 저장하는 것이다.

## 6. Authenticated historical registry

registry는 다음 정보를 보존하는 bounded application state다.

```text
HistoricalCommitteeRecord {
    context: ConsensusContext,
    start_height: u64,
    end_height_exclusive: Option<u64>,
    start_view: View,
    end_view_exclusive: Option<View>,
    members: Vec<CommitteeMemberRecord>,
    activation_state_root: StateRootReference,
}
```

최소 API는 다음과 같다.

```text
insert_genesis(context, committee, start_height, start_view)
activate(transition_proof)
lookup(context, offense_view, offense_height?)
validate_for_evidence(proof, validation_view)
```

구현 원칙:

- record는 context와 member canonical hash를 함께 검증한다.
- height/view 범위는 겹치지 않고, epoch transition 경계에서 연속적이어야 한다.
- registry 정렬 순서와 member 정렬 순서는 canonical이어야 한다.
- 현재 active record와 historical records 모두 full-state root preimage에 포함한다.
- `AppSnapshot`에 registry를 포함하고 resource limit/max record/max member 수를 검증한다.
- runtime `Committee`는 registry에서 재구성한 cache일 뿐이며, cache 자체를 authority로 삼지
  않는다.
- snapshot restore/restart 후 registry root와 activation marker가 확인되기 전에는 evidence,
  vote, proposal을 fail closed한다.
- registry pruning은 가장 오래된 record의 `end_view` 이후에도
  `MAX_EVIDENCE_AGE_VIEWS`와 unbonding evidence hold, recovery horizon을 모두 지난 뒤에만
  가능하다. 보존 기간을 모르는 상태에서 epoch 전환 직후 old committee를 삭제하지 않는다.

현재 full-state hash가 staking/evidence/snapshot material을 포함하는 위치는
`src/app/state/full_state_hash.rs:526-584,638-652`이며, snapshot 구조는
`src/storage/snapshot.rs:38-78`이다. registry를 이 경계 밖의 unrooted RocksDB cache로만 두면
corrupt/stale registry가 다른 slashing 결과를 만들 수 있으므로 mainnet 설계로 허용하지 않는다.

## 7. View-based evidence expiry와 journal GC

현재 `EquivocationProof`에는 offense height가 없고 `view`는 vote 서명에 포함되어 있다
(`src/types/certificate.rs:110-124`). 따라서 첫 구현은 다음 정책을 사용한다.

```text
MAX_EVIDENCE_AGE_VIEWS = protocol constant W

current_view < proof.view                    => future evidence 거부
current_view - proof.view > W                => expired evidence 거부
그 외                                        => historical registry로 검증 후 허용
```

`current_view`는 local wall clock이나 evidence timestamp가 아니다. block execution에서는
`block.view`, ingress/recovery에서는 canonical committed/latest view를 사용한다. evidence
timestamp는 현재 `0`으로 강제되어 있으므로 시간 기반 expiry를 추가하지 않는다.

검증과 mutation 순서는 모든 경로에서 같아야 한다.

```text
canonicalize
→ exact historical committee lookup
→ BLS/context/offender verification
→ view expiry verification
→ journal sync save
→ app evidence queue
→ proposal/relay
→ block execution and durable commit
→ journal delete
```

세부 불변조건:

- expired/invalid evidence는 journal save, mempool enqueue, seen-cache, relay를 일으키지 않는다.
- journal key는 `(context, offender)`를 유지한다. 동일 context/offender의 다른 proof pair는
  first-write-wins와 one-slash semantics를 공유한다. epoch/committee hash가 다른 record는
  별도 key다 (`src/consensus/equivocation.rs:242-253`, `src/app/mempool.rs:287-327`).
- expired row는 canonical parse와 historical validation이 끝난 뒤 recovery에서 sync delete한다.
  삭제하지 않고 reject만 하면 restart livelock이 된다.
- malformed, mis-keyed, unknown-context, invalid-signature row는 조용히 삭제하지 않고 fail
  closed한다.
- proposal 직전에 evidence queue의 expired item을 deterministic하게 제거한다. 현재 evidence가
  일반 age pruning을 건너뛰는 동작(`src/app/mempool.rs:329-377`)은 protocol expiry를 추가할 때
  그대로 두면 안 된다.
- durable finalized block/app state/consensus metadata가 commit된 뒤에만 journal을 삭제한다.
  commit 전 crash에서는 journal이 남아야 하며, recovery가 재검증 후 재제안할 수 있어야 한다.
- journal expiry cleanup은 canonical application mutation이 아니므로 sync delete는 idempotent해야
  한다. delete 실패는 recovery를 성공으로 표시하지 않는다.

height 기반 evidence window는 향후 signed height가 도입된 뒤 별도 schema version으로 추가한다.
unsigned height를 신뢰하거나 view와 height를 임의로 섞는 것은 허용하지 않는다.

## 8. Unbonding evidence hold

historical evidence는 offense 당시 committee key를 사용하지만, slash 대상의 현재 stake 상태와
자산 보존도 별도로 맞아야 한다. 현재 undelegate는 stake를 즉시 current total에서 빼고 queue에
넣으며 (`src/app/staking/state.rs:621-689`), claim은 자산을 실제 잔액으로 이동한다
(`src/app/state/execution.rs:602-617`). 현재 evidence processing은 이미 queue에서 빠진/claim된
자산을 복구해 slash하지 않는다 (`src/app/staking/slashing.rs:260-330`).

따라서 최소 mainnet-safe 정책은 다음과 같다.

- `UnstakeRequest`에 `created_view`와 `evidence_hold_until_view`를 기록한다.
- `evidence_hold_until_view >= created_view + MAX_EVIDENCE_AGE_VIEWS`를 보장한다.
- timestamp unbonding delay가 먼저 끝나도 hold view가 지나기 전에는 `ClaimUnstaked`를 거부한다.
- hold 중 도착한 valid historical evidence가 queued amount에 미치는 slash 규칙을 명시한다.
  최소 구현은 claim 전 queued amount를 slashable ledger로 유지하는 것이다.
- hold 정보와 queued balance는 primary-state validation, full-state root, snapshot에 포함한다.
- evidence window보다 짧은 unbonding/claim을 허용하는 configuration은 거부한다.

더 복잡한 대안인 이미 claim된 자산의 slashable debt ledger는 이 단계의 범위가 아니다. hold나
동등한 보존 장치 없이 historical evidence만 활성화하면 validator가 undelegate/claim으로
slash를 회피할 수 있으므로 전환의 launch gate로 둔다.

## 9. 안전한 원자적 activation

epoch transition은 application, consensus, network, pacemaker, storage가 서로 다른 시점에
바뀌는 단순 setter가 아니다. 다음 상태를 하나의 durable activation boundary로 취급한다.

```text
1. old finalized block/QC/state root 확인
2. transition proof와 next committee stage
3. historical registry record stage
4. app state root와 consensus context/locked-high QC stage
5. activation marker와 finalized block/state metadata를 sync write
6. durable commit 확인
7. runtime committee/context 교체
8. pacemaker와 network admission context 교체
9. 그 뒤에만 new-context vote/propose/relay 허용
```

저장소가 여러 column family/파일에 걸쳐 진정한 atomic commit을 제공하지 않으면 activation
marker와 idempotent recovery를 사용한다. 다음 crash 지점 모두에서 old 또는 new 중 하나만
관찰되어야 한다.

- registry만 저장되고 runtime swap 전에 종료
- runtime swap 후 process 종료
- first-new block durable commit 전 종료
- app state commit 후 consensus metadata commit 전 종료
- network/pacemaker가 새 context를 받기 직전 종료

재시작은 다음 순서로 처리한다.

1. finalized block, consensus state, activation marker를 load한다.
2. old/new context와 state root를 replay/검증한다.
3. registry를 authenticated state에서 복구한다.
4. activation이 완전하지 않으면 old context를 유지하고 staged new data를 재검증하거나
   rollback한다. runtime cache만 보고 new context를 활성화하지 않는다.
5. 완전한 activation을 확인한 뒤에만 committee binding, pacemaker restore, network admission을
   수행한다.
6. journal을 historical lookup과 view expiry로 reconcile한다. valid row만 queue/relay하고,
   expired row는 sync delete한다.

### 9.1 Safety와 pacemaker

- consensus safety state에는 context, high QC, locked QC, voted view와 transition marker를
  함께 저장한다.
- old context의 lock을 proof 없이 new context로 옮기지 않는다.
- new context로 view를 reset할 경우 `(epoch, context, view)` domain이 달라야 하며, 재사용된
  숫자의 vote/QC를 old context와 혼동하지 않는다.
- transition candidate를 본 것만으로 pacemaker timeout을 new context로 발행하지 않는다.
- first-new block의 parent, effective height, proof, proposer signature가 모두 검증된 뒤에만
  new proposer schedule을 사용한다.

### 9.2 Network와 sync

현재 network validation config는 하나의 committee/context만 보관한다. historical 단계에서는
exact context+view를 조회하는 shared registry resolver와 current canonical view가 필요하다.

- semantic validation 전에는 seen-cache/deliver/relay하지 않는다.
- old proof는 old registry record로, new block은 new record로 검증한다.
- unknown/future/expired context는 relay하지 않는다.
- ActiveSync는 old transition block, old QC, state root, transition proof, first-new block을
  순서와 parent로 검증한 뒤에만 import한다.
- peer가 제공한 registry/app snapshot을 검증 없이 runtime committee로 주입하지 않는다.

## 10. 단계별 구현 계획

### Phase 0 — static boundary 고정

- 현재 epoch-0 config/context 검증을 유지한다.
- dynamic update panic/fail-closed 경계를 테스트로 고정한다.
- transition proof 없는 context 변경, snapshot 후 committee 미주입 evidence를 명시적으로 거부한다.
- 이 단계에서는 historical proof를 받아들이지 않는다.

완료 기준: 기존 single-node와 4-validator local runtime이 동일한 epoch-0 root를 만들고,
non-zero context가 모든 ingress/replay 경로에서 거부된다.

### Phase 1 — bonded power와 deterministic next set

- HYCK bonded power와 validator/operator identity의 canonical state schema를 확정한다.
- top21, tie-break, minimum bond, duplicate BLS key/PoP 검증을 구현한다.
- next committee candidate와 hash를 finalized state에서 재현하는 pure function을 만든다.
- rewards/commission/복잡한 tokenomics는 이 단계에서 제외한다.

완료 기준: 모든 노드가 같은 state root에서 같은 next committee/hash를 계산하고,
property/fuzz test가 serialization 순서에 영향을 받지 않는다.

### Phase 2 — transition proof와 authenticated registry

- 위의 `EpochTransitionProof` canonical schema와 domain/version을 추가한다.
- old QC, state root reference, next set/hash, effective height 검증기를 구현한다.
- registry를 app state/full-state root/snapshot에 저장하고 runtime committee를 재구성한다.
- registry retention bound와 resource limit을 추가한다.

완료 기준: 잘못된 old QC, root, height, committee hash, PoP, quorum이 mutation 전에 거부되고,
snapshot/restart/replay가 같은 registry root를 만든다.

### Phase 3 — first-new-epoch consensus activation

- old transition block과 height `H` first-new block 규칙을 구현한다.
- activation marker, consensus state, app state root, registry의 durable commit/recovery를 구현한다.
- safety state/pacemaker/network admission을 atomic activation 이후에만 새 context로 바꾼다.
- old/new committee가 서로의 context에서 vote/QC를 재사용하지 못하게 한다.

완료 기준: crash injection의 모든 activation 지점에서 old 또는 new 상태로만 복구되고,
height skip/duplicate activation/old-context height `H` block이 거부된다.

### Phase 4 — evidence expiry와 unbonding hold

- 서명된 `view` 기반 `MAX_EVIDENCE_AGE_VIEWS`를 추가한다.
- network, runner, app validation, execution, mempool, journal recovery가 같은 helper와
  canonical validation view를 사용하게 한다.
- historical registry lookup 후에만 evidence를 검증한다.
- expired journal row GC와 queue pruning을 구현한다.
- `evidence_hold_until_view`와 claim guard를 state root/snapshot/primary validation에 추가한다.

완료 기준: future/expired evidence가 journal, queue, relay, app mutation 어느 것도 만들지 않고,
valid old proof가 current key rotation 뒤에도 정확한 historical key와 slashable balance를
사용한다.

### Phase 5 — verified sync/snapshot/restart

- transition proof와 registry를 포함한 verified sync import를 구현한다.
- snapshot manifest가 finalized block, state root, registry root, activation marker를 묶도록 한다.
- partial import, corrupted registry, stale runtime cache, restart 중 journal expiry를 fault-inject한다.

완료 기준: fresh replay, snapshot restore, sync import, crash/restart가 같은 finalized block,
state root, registry, committee context를 만든다.

### Phase 6 — permissionless testnet과 launch gate

- 4+ validator WAN/partition/Byzantine fixture에서 `f = 1`부터 검증한다.
- 2~3 validator는 `f = 0` local smoke로만 표시한다.
- validator entry/exit, key rotation, unbonding, delayed evidence, liveness와 economic attack을
  장기 실행한다.
- 독립 consensus/crypto/accounting/security audit과 reproducible release 검증 후에만
  permissionless mainnet 전환을 검토한다.

## 11. 필수 테스트 목록

### Schema와 registry

- transition proof canonical round-trip과 schema-version rejection
- old QC context/height/view/block hash mismatch rejection
- state root reference mismatch rejection
- top21 ordering, bonded-power tie-break, duplicate node/key/PoP rejection
- old committee가 active committee로 바뀐 뒤에도 historical lookup 성공
- unknown context, gap, overlap, wrong genesis rejection
- registry root와 snapshot round-trip equality

### Cross-context consensus

- `H - 1` old transition block → `H` first-new block 정상 경로
- transition proof 없는 first-new block rejection
- height skip, duplicate activation, old-context block at `H` rejection
- old committee/new committee의 교차 context vote/QC rejection
- proposer signature와 next committee hash mismatch rejection
- 2~3 node는 fault tolerance 없음으로 표시되고, 4 node에서 1 Byzantine fixture가
  safety/liveness 경계를 통과하는지 확인

### Evidence, journal, mempool

- `W` 경계 수락, `W + 1` 거부, future view 거부
- old committee key rotation 뒤 historical evidence 검증
- expired evidence가 journal save/queue/seen-cache/relay를 만들지 않음
- recovery가 valid row는 재등록하고 expired row는 idempotent sync delete
- malformed row는 조용히 삭제하지 않고 fail closed
- durable commit 전 crash에서는 journal 보존, commit 후 delete는 재시작에 안전
- 동일 `(context, offender)` 중복 slash 방지와 서로 다른 context의 독립 처리
- proposal 직전 expired queue item pruning

### Unbonding와 상태 보존

- evidence hold 전 claim 거부, hold 이후 claim 허용
- hold 중 historical evidence가 queued slashable amount에 적용
- hold/registry/evidence가 full-state root와 snapshot에 반영
- 이미 tombstoned된 동일 evidence가 추가 slash하지 않음

### Fault injection와 운영 경계

- registry write, activation marker write, app root write, consensus metadata write,
  runtime swap, pacemaker swap, network swap 각 지점의 crash/restart
- fresh genesis replay와 snapshot/verified sync 결과 비교
- single-node N=1 기능 테스트와 4-validator Docker network 비교
- long pause, duplicate relay, partition/rejoin, malformed peer response, bounded resource test

## 12. 구현 완료로 간주하지 않는 항목

다음은 이 문서 작성 시점에 구현 완료로 주장하지 않는다.

- historical committee registry
- `EpochTransitionProof` wire type와 verifier
- first-new-epoch cross-context block
- permissionless top21 activation
- view-based evidence expiry와 journal expiry GC
- unbonding evidence hold
- registry-aware snapshot/verified sync/restart

위 항목 중 하나만 구현된 상태에서 나머지를 우회하면 static epoch-0 fail-closed 경계를 깨게
되므로, Phase 3~5의 acceptance criteria를 모두 통과하기 전에는 epoch transition과 historical
slashing을 production profile에서 활성화하지 않는다.

최종 상태는 계속 **NOT MAINNET READY**다. 이 문서는 구현 승인서가 아니라, 동일한 committee,
state root, evidence 결과를 모든 노드가 독립적으로 재구성하기 위한 선행 설계와 검증 계약이다.
