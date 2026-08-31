# 트랜잭션 전파와 펀딩 보존성 작업 기록

## 작업 의도

이번 단계는 스테이킹 변경이 아니라 다음 두 문제를 해결한다.

1. 사용자가 어느 검증자 API에 제출하더라도 동일한 서명 트랜잭션이 현재 리더에게 전달되어 블록 후보가 될 수 있어야 한다.
2. 펀딩 지급자의 담보가 부족할 때 지급하지 못한 금액이 수취자 잔액으로 새로 생성되면 안 된다.

## 1. 서명 트랜잭션 전파

### 기존 문제

- API가 트랜잭션을 요청을 받은 노드의 로컬 mempool에만 넣었다.
- 그 노드가 현재 리더가 아니면 다른 검증자, 특히 현재 리더가 트랜잭션을 알 수 없었다.
- validator 합의 메시지와 user signer의 신원을 같은 것으로 취급할 수 없어 별도의 admission 경로가 필요했다.

### 구현

- wire message 끝에 `UserTransaction(SignedEnvelope)`를 추가해 기존 bincode variant 번호를 보존했다.
- `hl-node`의 API와 consensus runner가 동일한 `TcpNetwork`의 outbound-only broadcaster를 공유한다.
- API는 canonical `submit_envelope_at`으로 먼저 로컬 검증·mempool admission을 수행한 뒤 app lock을 해제하고 전파한다.
- 수신 노드는 transport에서 domain, validity, signature, action binding을 검사하고 runner에서 nonce와 mempool 정책을 다시 검사한다.
- runner의 proposal 대기, prepare 대기, vote 수집 루프 모두 같은 user transaction handler를 사용한다.
- application admission이 성공한 경우에만 seen cache를 확정하고 재전파한다. 잘못된 서명, 미래 nonce, stale nonce, mempool 포화 요청은 정상 재시도를 막지 않는다.
- 블록 payload에는 우회용 system transaction이 아니라 정확한 `ConsensusTransaction::Signed` envelope가 들어간다.
- `MODE=dev`에서만 development envelope 전파를 허용하며 testnet/mainnet transport는 계속 거부한다.

### 보장하는 경로

```text
비리더 API
  -> canonical local admission
  -> 같은 hl-node TCP broadcaster
  -> 인증된 validator transport
  -> 상대 runner canonical admission
  -> 리더 prepare_payload
  -> ConsensusTransaction::Signed
```

최초 전파는 연결된 peer에 대한 best-effort gossip이다. 이후 canonical mempool에 남은 트랜잭션을 제한적으로 재전파하는 기능은 2026-08-29 작업에서 추가했다. 상세 내용은 [bounded transaction rebroadcast 작업 기록](./WORKLOG_2026-08-29_BOUNDED_TRANSACTION_REBROADCAST.md)을 참고한다.

## 2. 펀딩 정산 보존성

### 기존 문제

- position이 명목 펀딩 전액을 누적 기록했다.
- 지급자 balance만 보유액으로 제한했지만 수취자는 명목 금액 전액을 받았다.
- 지급자 부족분만큼 전체 담보 합계가 증가할 수 있었고, balance와 cumulative funding/event가 서로 달랐다.

### 구현

- 정산을 `명목 계산 -> 양쪽 capacity 계산 -> 실제 정산 -> 상태 반영`의 두 단계 방식으로 바꿨다.
- 지급자는 `max(balance, 0)`보다 많이 낼 수 없다.
- 전체 지급 가능액과 전체 수취 가능액 중 작은 값만 이동한다.
- 여러 지급자와 수취자에게 비례 배분하며, 정수 나머지는 주소 정렬과 largest-remainder 규칙으로 결정한다.
- balance, `cumulative_funding`, funding event, `FundingResult`가 모두 실제 settled amount를 기록한다.
- 양수·음수 funding rate, 음수 balance, i64 경계에서도 동일 규칙을 적용한다.

핵심 불변식은 다음과 같다.

```text
sum(account balance delta) == 0
sum(user funding payment) == 0
receiver credit <= amount actually collected from payers
```

지급자 부족분은 이번 정책에서 사회화된 미정산분으로 사라지며 insurance fund나 debt로 만들지 않는다.

## 검증

- 실제 인증된 두 TCP 노드에서 비리더 API 제출부터 리더 payload 선택까지 통과
- transport transaction admission/dedup 테스트 29개 통과
- funding 단위 테스트 18개 통과
- library 테스트 728개 통과
- `hl-node` 테스트 13개 통과
- multinode 테스트 5개 통과
- E2E 테스트 95개 통과
- `cargo check --locked --all-targets --all-features` 통과
- `cargo fmt --all -- --check`, `git diff --check` 통과

기존 경고는 남아 있지만 이번 변경에서 새 오류나 실패 테스트는 없다.
