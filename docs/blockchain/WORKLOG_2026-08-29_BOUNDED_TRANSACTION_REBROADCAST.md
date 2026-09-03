# Bounded 트랜잭션 재전파 작업 기록

## 목적

최초 gossip 순간에 연결이 끊겼거나 peer writer queue가 막히면, 서명 트랜잭션은 제출 노드의 mempool에는 남지만 현재 리더에게 전달되지 않을 수 있다. 이 작업은 아직 commit되지 않은 canonical `SignedEnvelope`를 제한된 비용으로 다시 전파해 해당 공백을 복구한다.

재전파는 이미 실행된 트랜잭션을 다시 실행하는 기능이 아니다. 수신 노드는 기존 signature/domain/validity 검증과 hash·signer nonce admission을 그대로 수행하므로 이미 처리한 envelope는 중복 실행되지 않는다.

## 구현 정책

- canonical mempool만 재전파 원본으로 사용한다. 별도의 메모리·디스크 outbox 상태를 만들지 않는다.
- 최초 제출은 기존 gossip fanout을 사용한다.
- 재시도는 2초 간격으로 수행한다.
- 한 tick에서 최대 32개, envelope canonical encoding 합계 최대 2 MiB만 선택한다.
- rotating cursor로 큰 mempool에서도 뒤쪽 항목이 영구적으로 굶지 않게 한다.
- 전체 mempool을 `Vec`으로 복사하지 않고 큐를 직접 순회해 선택된 batch만 clone한다.
- 재시도 때는 deterministic gossip fanout을 반복하지 않고 현재 연결된 모든 validator peer에게 raw `UserTransaction`을 직접 전달한다.
- peer writer에는 `try_send`를 사용한다. 한 peer queue가 가득 차도 다른 peer 전송과 다음 트랜잭션 처리를 막지 않는다.
- application lock을 해제한 뒤 네트워크 전송을 수행한다.

## 재전파 대상에서 빠지는 조건

- 블록 commit으로 canonical mempool에서 제거됨
- mempool `max_age_ms`를 초과함
- envelope의 `valid_until`이 지남
- envelope의 `valid_after`에 아직 도달하지 않음
- unsigned system transaction 또는 equivocation evidence임

commit과 batch snapshot이 동시에 일어나면 commit 직후 한 번 더 wire에 실릴 수 있다. 이는 안전하다. 수신 노드의 nonce/mempool 검증이 stale transaction을 거부하며, 다음 tick부터는 원본 mempool에서 사라진다.

## 노드 생명주기

재전파 worker는 `hl-node`의 consensus/API와 같은 `tokio::select!` 생명주기에서 실행된다. 노드가 종료되면 future도 함께 취소되고, worker가 예기치 않게 종료되면 노드가 오류로 중단된다. 별도의 orphan task를 남기지 않는다.

## 검증 범위

- 첫 전송 실패 후 peer 재연결 시 동일 envelope 전달
- 모든 연결 peer에 direct retry 전달
- full writer queue가 worker를 block하지 않으며 정상 peer는 계속 수신
- count 및 encoded-byte 상한
- byte 경계에서도 rotating cursor가 다음 항목으로 진행
- mempool age, 미래 validity, expiry, system transaction 필터
- commit 후 retry source에서 제거
- periodic worker가 다음 tick에 exact envelope를 재시도

## 남은 경계

canonical mempool은 프로세스 메모리 상태이므로 노드 프로세스가 재시작되면 미포함 트랜잭션도 복원되지 않는다. 이는 일반적인 mempool 정책과 같으며, 향후 실제 mainnet 운영에서 필요성이 확인되면 durable mempool 또는 client resubmission 정책을 별도로 설계한다.
