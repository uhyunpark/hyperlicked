# Finalized Transaction Lifecycle

## 목적

기존 제출 API는 합의 전에 임시 `orderId`를 성공 결과처럼 반환했고, Commitment v2에
확정된 실행 결과가 있어도 사용자가 트랜잭션 해시로 다시 조회할 방법이 없었다. 또한
WebSocket 알림이 영구 저장보다 먼저 보이면 재시작 후 실제 체인 상태와 알림이 어긋날 수
있었다.

이번 단계의 기준은 단순하다.

- 제출 성공은 **실행 성공이 아니라 노드 접수 성공**이다.
- 실행 결과의 원본은 최종 블록에 인증된 **Commitment v2 receipt/event**다.
- 조회와 사용자 알림은 최종 블록이 영구 저장되고 canonical application commit까지 끝난
  뒤에만 노출한다.

## 최종 흐름

```text
서명 트랜잭션 제출
  -> { status: "pending", tx_hash }
  -> gossip/rebroadcast 및 블록 포함
  -> 결정론적 실행 + Commitment v2 생성
  -> RocksDB에 블록/합의 상태/commitment/state root/receipt index 원자 저장
  -> canonical application commit
  -> transactionFinalized WebSocket 알림
  -> GET /api/v1/transactions/:tx_hash 로 보존된 finalized history에서 재조회
```

`tx_hash`는 서명 envelope의 canonical hash다. 주문·취소·트리거 주문 제출 API는 더 이상
합의 전에 가짜 `orderId` 또는 `triggerOrderId`를 만들지 않는다. 실제 주문 ID와 체결 정보는
확정 receipt의 `ORDER_UPDATE`/`FILL` event에서 얻는다.

## 웹 canonical 서명

웹의 기존 주문 서명은 EVM `chainId`와 `verifyingContract`를 쓰는 legacy EIP-712였다. 현재
consensus envelope는 그 형식이 아니라 다음 값을 서명한다.

- domain: `HyperLicked`, version `1`, live genesis hash를 `bytes32 salt`로 사용
- message: `chainDomain`, wallet signer, nonce, `validAfter`, `validUntil`, `actionHash`
- `actionHash`: `HYPERLICKED-ACTION-V1\0 || bincode(Transaction)`의 Keccak hash

웹은 `/sync/block/0`에서 live genesis domain을 읽고 캐시한다. 주문, reduce-only 포지션 종료,
TP/SL, 일반 주문 취소와 트리거 취소가 모두 `signatureScheme=eip712-v1`을 사용한다. Rust와
TypeScript에는 Place/Cancel/Trigger action bytes, hash와 EIP-712 digest가 동일한 golden
vector가 있어 enum 순서나 encoding 변경을 즉시 탐지한다.

현재 canonical envelope는 `signer == action.trader`를 강제한다. 따라서 legacy agent key로
서명하면 정상적으로 거부되며, 웹의 canonical 거래는 연결된 wallet으로 직접 서명한다. Agent
key를 다시 활성화하려면 delegation 자체를 consensus action/signature domain에 인증하는 별도
프로토콜 변경이 필요하다.

한 번에 main order와 TP/SL을 접수할 수 있도록 nonce는 `n`, `n+1`, `n+2`로 할당한다. 노드는
bounded nonce gap(`MAX_NONCE_GAP=10`) 안에서 순서가 뒤바뀌어 도착한 envelope를 mempool에
보관하지만, consensus payload에는 signer별로 연속된 nonce만 `n -> n+1 -> n+2` 순서로 넣는다.
선행 nonce가 아직 없으면 해당 signer의 future transaction만 미루고 다른 signer의 준비된
transaction은 계속 제안한다. follower도 같은 연속 순서를 강제하므로 Byzantine proposer가 gap,
duplicate 또는 replay nonce를 넣은 block은 무효다. 실패한 action은 failure receipt를 남기면서
nonce를 소비하고, nonce counter overflow는 wrap하지 않고 fail-closed 처리한다. nonce 조회 API는
JavaScript의 정수 정밀도를 잃지 않도록 decimal string을 반환한다.

## 영구 receipt index

RocksDB의 `transaction_receipts` column family는 다음 위치 정보를 저장한다.

- signed transaction ID
- finalized block hash와 height
- block 내부 transaction index
- Commitment v2 receipt

블록, height index, 합의 상태, Commitment v2, state root, receipt index는 하나의 synced
write batch로 저장된다. 일부만 기록된 확정 상태는 허용하지 않는다.

조회할 때 index row 자체를 신뢰하지 않고 canonical height/block, 원본 payload의 signed
envelope hash, Commitment canonical bytes/root, receipt index와 `tx_id`를 다시 대조한다.
손상이나 불일치가 있으면 잘못된 결과를 반환하지 않고 실패한다.

프로토콜 내부 `System` transaction은 전역 hash lookup 대상이 아니다. 동일한 system action이
여러 블록에 등장할 수 있어 hash가 전역적으로 유일하지 않기 때문이다. 사용자 서명이 있는
`Signed` transaction만 index한다.

운영자가 오래된 canonical block을 prune하면 해당 block의 signed receipt index도 같은 batch에서
삭제된다. 장기 보존·범위 조회는 추후 indexer가 담당하고, 현재 노드 API는 자신이 보존한
finalized history만 제공한다.

## API 계약

제출 응답:

```json
{
  "status": "pending",
  "tx_hash": "<64 lowercase hex>"
}
```

확정 조회:

```http
GET /api/v1/transactions/:tx_hash
```

- `200`: 확정 receipt, block 위치, execution status/error, resource usage, event 목록
- `404`: 아직 확정되지 않았거나 존재하지 않음
- `400`: 잘못된 hash 형식
- `503`: persistent store가 없거나 인증된 조회를 수행할 수 없음

모든 event는 `event_type`, `event_name`, 정확한 `payload_hex`를 제공한다. 현재
`ORDER_UPDATE`와 `FILL`은 안전하게 canonical decode될 때만 JSON `payload`도 함께 제공한다.
알 수 없는 event나 decode 실패는 원본 hex를 유지하며 내용을 추측하지 않는다.

## WebSocket 내구성 경계

`transactionFinalized`는 다음 순서를 모두 지난 signed transaction에만 해당 signer 주소로
전송한다.

1. finalized block과 Commitment/state root의 durable write 성공
2. canonical application commit 성공 및 app hash 일치
3. Commitment와 block payload의 transaction/receipt 정합성 재검증

speculative execution이나 application commit만으로는 알림을 보내지 않는다. 재시작과 verified
sync도 과거 WebSocket 알림을 재생하지 않는다. WebSocket은 bounded best-effort 채널이므로 느린
클라이언트가 알림을 놓치면 receipt API로 복구해야 한다.

non-dev private subscription은 wallet 소유권을 증명해야 한다. 웹은 연결할 때마다 새 Unix-second
timestamp로 `Subscribe to {lowercase address} at {timestamp}`를 EIP-191 `personal_sign`하고,
서버는 5분 replay window 안에서 복구한 주소를 확인한 뒤에만 `transactionFinalized`를 전달한다.
서명 승인 중 account나 socket이 바뀌면 오래된 frame은 폐기한다. 로컬 dev의 무인증 구독은 기존
동작을 유지한다.

## 의도적으로 미룬 범위

- indexer용 block/range API
- 과거 WebSocket event replay
- 기존 로컬 DB receipt index backfill

이 프로젝트는 아직 공개 네트워크가 아니고 migration compatibility가 필요 없다는 전제다.
기존 개발 DB에는 과거 receipt index가 자동 생성되지 않으므로 이 기능을 처음 검증할 때는
로컬 chain data를 초기화하고 새 genesis에서 시작하는 것이 명확하다.

## 검증 기준

- 성공/실패 receipt 모두 hash로 조회 가능
- DB reopen 후에도 같은 결과 조회
- 저장 실패 시 receipt index가 부분 노출되지 않음
- 직접 application commit만으로 WebSocket 알림이 발생하지 않음
- durable consensus commit 뒤 정확히 한 번 live 알림 발생
- non-leader 제출/gossip, consensus, receipt 저장과 조회 경로의 기존 회귀 테스트 통과
