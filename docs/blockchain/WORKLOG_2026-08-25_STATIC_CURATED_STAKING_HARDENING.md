# Static curated staking hardening 작업 기록 (2026-08-25)

> 상태: local/dev curated epoch-0 구현 기록. **Dynamic PoS 또는 MAINNET READY 판정이 아니다.**

## 결정한 최소 모델

- Native staking 단위는 HYCK다.
- `1 HYCK = 1,000,000 base units`로 고정한다.
- Genesis의 `voting_power`는 whole-HYCK bonded power로 해석한다.
- 초기 self stake는 `voting_power * 1,000,000` base units다.
- 다음 committee 후보 power는 `total bonded base units / 1,000,000`의 정수 부분이다.
- Active validator는 bonded power 내림차순 top 21이며 기존 deterministic tie-break를 유지한다.
- 초기 2~3 validator 운영은 가능하지만 Byzantine fault tolerance는 `f=0`이다. Equal-power
  strict `>2/3` quorum에서 3개는 모두 참여해야 하며, 4개부터 한 validator 장애를 견딜 수 있다.

복잡한 reward/commission/delegation tokenomics를 이번 단계에서 새로 만들지 않았다. 기존 필드와
transaction 경계를 유지하면서 consensus와 staking의 단위만 일치시켰다.

## 발견한 문제

1. Canonical application은 view 54,000에서 epoch를 증가시키고 validator update를 만들지만,
   consensus runner는 dynamic update를 지원하지 않아 durable commit 뒤 panic했다.
2. Genesis는 `voting_power * 1,000,000`을 stake로 만들면서 다음 committee update에는 raw base-unit
   stake를 power로 전달해 power 단위가 달라졌다.
3. Genesis/Committee는 21명을 초과할 수 있지만 staking active set은 21명으로 잘랐다.
4. Static committee record의 BLS key를 즉시 회전시키면 runtime consensus key와 application
   evidence key가 달라졌다.
5. 한 delegator의 `ClaimUnstaked`가 모든 delegator의 만료 queue를 drain한 뒤 자기 금액만
   반환해 다른 사용자의 자금을 잃을 수 있었다.
6. Staking API가 HYCK base units를 여전히 USD cents로 표시했다.
7. Snapshot 복구 뒤 trusted committee를 다시 주입하기 전에는 static-mode 표시는 남아 있지만,
   key rotation과 dynamic epoch transition 경로가 잠시 열릴 수 있었다.
8. `ClaimUnstaked`가 queue를 먼저 제거한 뒤 account에 입금해, 잔액 overflow 시 claim 자금이
   사라질 수 있었다.
9. Snapshot에 static committee member의 staking record가 빠져도 runtime committee bind가
   성공해 해당 validator를 slash할 수 없는 상태가 될 수 있었다.
10. Epoch-0 static committee에 nonzero staking epoch snapshot을 bind하는 검증이 없었다.

## 구현 결과

- Consensus와 application이 공유하는 committee 상한을 21로 고정하고 genesis load 단계에서도
  `1..=21`을 검증한다.
- Checked `stake_to_voting_power` 변환을 사용해 next-set 후보 power를 whole HYCK 단위로 만든다.
- Trusted static committee가 bind된 canonical AppState는 자동 epoch transition/update를 만들지
  않는다. 따라서 dynamic protocol이 완성되기 전까지 epoch 0을 장기 유지한다.
- Static mode에서는 BLS key rotation을 명시적으로 거부한다.
- `hl-node`와 `multinode` recovery는 genesis early return 전부터 block/config/application epoch와
  pending validator update를 검사한다.
- `ClaimUnstaked`는 요청한 delegator의 만료분만 제거한다. 다른 사용자와 미만료 요청은 유지한다.
- Snapshot 복구 상태는 trusted committee가 다시 bind될 때까지 block 실행과 key rotation을
  fail closed한다. Static mode가 dynamic epoch 경로로 일시 우회되지 않는다.
- Static committee bind는 epoch-0 staking snapshot과 위원회 전체의 slashable validator record가
  정확히 존재하는지 확인한다. 불완전하거나 다른 epoch의 snapshot은 실행 전에 거부한다.
- Claim 금액과 account 잔액을 checked arithmetic으로 먼저 검증하고, 입금에 성공한 뒤에만
  unstake queue를 제거한다. 실패하면 잔액과 queue가 모두 유지된다.
- Staking API는 raw base-unit 정수와 함께 `*_hyck` 표시 필드를 제공하고 잘못된 `*_usd` 필드를
  제거했다.

## 의도적으로 활성화하지 않은 기능

- Historical committee registry
- Evidence expiry와 unbonding evidence hold
- Epoch transition certificate
- Cross-context first-new-epoch block
- Runtime committee/network/safety/pacemaker 교체

이 기능들은 일부만 켜면 old QC를 새 committee로 검증하거나 restart 이후 다른 committee로
evidence를 검증할 수 있다. 필요한 전체 경계는
[`HISTORICAL_COMMITTEE_AND_EPOCH_TRANSITION_PLAN.md`](./HISTORICAL_COMMITTEE_AND_EPOCH_TRANSITION_PLAN.md)에
정리했다.

## 검증

- `cargo test --locked --lib`: 648 passed, 0 failed, 0 ignored
- `cargo test --locked --bin hl-node --bin multinode`: 8 + 3 passed
- `cargo check --locked --all-targets --all-features`: 통과
- `cargo test --locked --all-features --lib consensus::engine::tests`: 8 passed
- `cargo fmt --all -- --check`, `git diff --check`: 통과
- ignored/`should_panic` test: 없음
- fresh canonical `hl-node`: height 0 → 2 commit
- 동일 RocksDB restart: height 2 → 3 recovery 및 commit

최종 smoke용 `/tmp/hyperlicked-hyck-static-final2.owMaPu`는 검증 후 macOS Trash로 이동했다.

## 남은 경계

- HYCK는 이번 단계에서 staking/genesis protocol 단위로 정의했다. Perp collateral과 실제 native
  asset ledger/issuance/bridge를 분리하는 작업은 아직 필요하다.
- Static epoch 0에서는 committee 변경과 key rotation을 지원하지 않는다.
- Dynamic transition 전에는 historical evidence를 활성화하지 않는다.
- Unbonding queue는 이번에 claim 격리만 고쳤다. Historical slashing 전에는 slashable unbonding
  hold를 추가해야 한다.
