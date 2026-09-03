# HYCK 고정 공급과 epoch 전환 staging 작업 기록

## 결정

- native token 이름은 `HYCK`다.
- 소수점은 6자리다. `1 HYCK = 1,000,000` base units다.
- genesis 총 발행량은 정확히 `1,000,000,000 HYCK`, 즉
  `1,000,000,000,000,000` base units다.
- 최대 공급량은 10억 HYCK로 고정한다. 이 중 `388,880,000 HYCK`는 genesis부터 별도
  staking emissions reserve에 있고, 초기 treasury/사용자/validator가 배분할 수 있는 양은
  `611,120,000 HYCK`다.
- staking reward는 reserve에서만 이동한다. 블록마다 무담보 신규 발행하는 mint 경로는 없다.
- 보상 곡선은 총 eligible stake에 대한 정수 inverse-square-root 방식이며,
  `400,000,000 HYCK` stake에서 연 `2.37%`가 되도록 고정했다.
- perp 거래용 collateral 잔액과 native HYCK 잔액은 별도 원장이다.

## 구현한 회계 경계

genesis에서 emissions reserve를 먼저 분리하고 나머지 HYCK만 `system:treasury`에 넣는다.
이후 validator self-bond와 명시적인 사용자 allocation을 treasury에서 차감해 옮긴다.
따라서 genesis 설정은 총 공급을 새로 더하는 명령이 아니라, 이미 발행된 고정 공급의
소유권 배분이다.

공급량 검사는 다음 authoritative bucket을 모두 합산한다.

1. 계정의 liquid HYCK
2. bonded stake
3. unbonding queue
4. validator/delegator pending rewards
5. genesis에서 이미 발행된 emissions reserve

위 합은 모든 canonical application preflight와 복구 상태에서 항상 총 발행량과 같아야
한다. slash된 HYCK는 소각하지 않고 treasury로 이동한다. unstake와 reward claim도 한
bucket에서 다른 bucket으로 옮길 뿐 총량을 바꾸지 않는다.

## 사용자 경로

- `Account`에 perp collateral과 분리된 `hyck_balance`를 추가했다.
- genesis는 canonical `hyck_allocations`를 받을 수 있다.
- `TransferHyck`는 signed envelope의 signer와 `from` 주소가 일치해야 한다.
- 잔액 부족과 recipient overflow는 debit 전에 검사해 실패 시 두 계정 모두 그대로다.
- account API는 HYCK base-unit/whole-unit 표시와 nonce를 반환한다.
- state root schema v5와 snapshot에 native HYCK 잔액, emissions reserve, reward clock,
  정수 remainder, auto-compound cursor와 epoch-min eligible stake를 포함했다.

외부 wallet이 `TransferHyck`를 제출하는 전용 HTTP UX와 dev-only HYCK faucet은 아직
추가하지 않았다. 현재의 collateral faucet은 HYCK 발행 경로가 아니다. local showcase에서
쓸 wallet 주소는 genesis allocation으로 미리 배분할 수 있다.

## staking reward 모델

- reward accounting은 매 블록 호출하지만 90분 경계 전에는 O(1)이다.
- 새 validator/delegation 또는 증가한 stake는 진행 중인 reward epoch에 소급 참여하지 않는다.
  감소와 slash는 eligible stake를 즉시 낮춘다.
- 경계에서 발생한 보상은 validator self-stake와 delegator stake 비율로 나누고, validator
  commission을 적용한다. largest-remainder 배분으로 모든 base unit을 정확히 귀속시킨다.
- pending reward는 24시간마다 자동 compound한다. 그 전에는 기존 claim 경로로 liquid
  HYCK를 받을 수도 있다.
- reserve가 소진되면 추가 지급은 0이며 미지급 보상을 부채나 신규 발행으로 만들지 않는다.
- reward settlement는 transaction보다 먼저 speculative candidate에서 실행되고, 확정된
  state root와 Commitment v2 `EPOCH` system event에 함께 인증된다. event schema v1은
  validator self reward, commission, delegator net reward와 실제 auto-compound 내역을
  canonical recipient 순서로 기록한다.
- height 1이 chain time을 anchor하며 local wall clock의 과거/미래 30초 범위 안에 있어야
  한다. 이후 블록 timestamp는 부모보다 감소할 수 없고 한 블록당 최대 30초만 전진하며,
  live proposal은 local wall clock보다 30초 넘게 미래일 수도 없다. 첫 anchor의 과거 제한은
  허위 과거에서 보상 epoch를 압축하는 공격을 막는다. 이후 블록에는 wall-clock 과거 제한을
  강제하지 않아 장시간 네트워크 중단 뒤에도 parent부터 정상적으로 따라잡을 수 있다.

이 모델은 [Hyperliquid staking 문서](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/staking)의
future-emissions reserve, inverse-square-root APY, 주기적 accrual과 auto-redelegation 구조를
참고했다. HYCK의 reserve 크기, 90분 settlement cadence, 24시간 compound와 claim 병행은 이
프로젝트의 현재 showcase 파라미터이며 Hyperliquid와 완전히 같은 tokenomics라는 뜻은 아니다.

## validator 전환 상태

동적 committee 활성화는 아직 켜지 않았다. 대신 finalized old-committee block/QC,
old application state root, 다음 top-21 validator update를 함께 인증하는 bounded transition
proof를 staging할 수 있게 했다. proof는 application이 만든 정확한 validator update와
일치해야 하며, 다른 유효 BLS key를 끼워 넣어도 거부된다.

현재 activation mode는 `StagedOnly`다. historical committee registry, block-carried proof,
Safety/Pacemaker/Aggregator/network의 원자적 context 교체, first-new-epoch block 검증,
동적 recovery가 완성되기 전에는 runtime이 committee update를 적용하지 않고 fail-closed한다.
따라서 현재 static curated phase에서 slashing/jailing은 application stake와 상태에는 반영되지만
epoch-0 consensus committee의 voting power를 즉시 바꾸지는 않는다. 실제 위원회 퇴출은 위
authenticated transition activation이 완료된 뒤에만 켠다.

local fixture는 실행 편의를 위해 validator당 1 HYCK self-stake만 둔다. inverse-square-root
곡선 특성상 이 수치는 실제 네트워크의 APY 예시로 사용하면 안 된다. 공개 testnet 전에는
10억 HYCK 배분 안에서 현실적인 validator/delegator stake 분포를 별도 genesis로 정해야 한다.

## 검증 기준

- 고정 공급 및 전체 library 회귀 테스트
- signed HYCK transfer의 signer binding과 실패 원자성
- genesis allocation의 canonical validation, domain binding, 공급 보존
- inverse-square-root anchor, reserve exhaustion, commission/dust conservation, flash-stake 방지
- static curated runtime의 실제 block reward, authenticated epoch event와 snapshot/replay 일치
- timestamp regression/과도한 parent-relative jump의 무변경 거부
- `hl-node`/`multinode` binary tests
- all-target/all-feature compile
- fresh single-node commit과 동일 RocksDB restart recovery
- ignored test 목록 점검

## 최종 검증 결과

- `cargo test --locked --lib`: 693 passed, 0 failed, 0 ignored
- `cargo test --locked --test e2e`: 95 passed, 0 failed, 0 ignored
- `cargo test --locked --bin hl-node --bin multinode`: 11 passed
- `cargo check --locked --all-targets --all-features`: passed
- fresh single-node를 height 3까지 실행한 뒤 같은 RocksDB로 재시작해 height 5까지 복구/진행
- `cargo fmt --all -- --check`, `git diff --check`, ignored-test 검색: passed

## 다음 작업

1. canonical application transition record와 historical committee registry를 영속화한다.
2. epoch 경계의 모든 consensus/network component를 한 번에 교체하는 activation handshake를
   구현한다.
3. 4-node 환경에서 validator rotation, 지연 QC, crash/restart를 함께 검증한다.
4. reward cadence/reserve/commission 변경을 위한 별도 governance와 upgrade activation
   절차를 명세한다.
5. 그 다음 Sepolia 수준의 lock/mint 또는 canonical bridge adapter를 별도 신뢰 모델과 함께
   붙인다. bridge mint/burn 권한은 일반 staking/reward 코드와 분리한다.
