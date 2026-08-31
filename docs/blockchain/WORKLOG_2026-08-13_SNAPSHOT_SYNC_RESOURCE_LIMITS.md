# Snapshot/Sync 자원 한도 작업 기록 (2026-08-13)

## 결론

단일 JSON `AppSnapshot` 저장·로드는 64 MiB로 제한하고, HTTP/P2P block sync와 TCP wire
message는 32 MiB로 제한했다. 원격 HTTP 응답과 TCP length prefix는 큰 buffer를 만들기 전에
검사한다. Sync server는 요청 개수만 제한한 뒤 체인 전체 tail을 읽던 경로를 제거하고,
요청한 committed height window만 순차 조회한다.

이 작업은 리소스 고갈 방어다. Snapshot manifest/proof, chunk download, historical committee
검증, application replay를 구현하거나 ActiveSync가 state를 직접 교체하도록 활성화하지 않았다.

## 구현 경계

- Snapshot raw JSON은 역직렬화 전에 64 MiB를 초과하면 거부한다.
- 저장, load, hash, API/P2P export는 동일 bounded snapshot codec을 사용한다.
- Snapshot import는 새 `AppState`를 만들고 primary/derived 검증을 마친 뒤에만 반환하므로
  실패 시 기존 state를 변경하지 않는다.
- Record limit은 별도의 숨은 consensus rule을 만들지 않는다. 기존 `MAX_NONCE_GAP=10`과
  `MAX_ACTIVE_VALIDATORS=21`에서 직접 도출되는 항목만 중복 확인한다.
- HTTP 단일/range block 응답과 P2P `SyncResponse`는 32 MiB를 넘지 않으며, range 응답은
  한도 전에 page를 나눈다.
- HTTP ActiveSync는 `Content-Length`와 chunk 누적 길이를 모두 검사한다.
- TCP는 송신 직렬화 전에 예상 크기를 검사하고, 수신 length prefix를 검사한 뒤에만 buffer를
  할당한다. Bincode와 debug JSON 송신 모두 같은 상한을 사용한다.
- Sync 중 committed height가 누락되거나 height index가 다른 block을 가리키면 이후 높이를
  건너뛰지 않는다.
- `/sync/status`의 snapshot height 조회는 snapshot JSON 전체를 decode하지 않는다.

## 성능 결정

P2P response 후보마다 전체 `Vec<Block>`을 clone/serialize하는 O(n²) 방식은 사용하지 않는다.
Empty envelope와 각 block의 bincode size를 O(n)으로 누적하고 마지막에 debug assertion으로
실제 message 크기를 재확인한다. HTTP도 block JSON 크기를 누적하며, snapshot base64는 최종
envelope 크기가 32 MiB 이하임을 계산한 뒤에만 문자열을 만든다.

## 검증 결과

- `cargo test --locked --all-targets --all-features`: 전체 통과
  - all-feature library 540 passed, 0 failed, 0 ignored
  - node 4, multinode binary 1, e2e 98 및 나머지 integration/bench target 통과
- Default library: 540 passed, 0 failed, 0 ignored
- Snapshot byte boundary, runtime-derived record boundary, malformed stored JSON metadata lookup,
  oversized import no-partial-mutation 통과
- P2P/HTTP/TCP exact boundary와 `+1`, large-page 분할, committed-height gap 통과
- `cargo check --locked --all-targets --all-features`, `cargo fmt --all -- --check`,
  `git diff --check` 통과

## 의도적으로 남긴 한계

- 64 MiB는 runtime state의 합의상 최대치가 아니라 현재 monolithic snapshot의 운영 상한이다.
  Runtime state가 이보다 커질 수 있으므로 snapshot 저장 실패를 consensus commit 실패로 연결하면
  안 된다. 현재 canonical commit/recovery는 snapshot을 사용하지 않는다.
- JSON은 raw input을 제한해도 decode 중 내부 allocation이 raw bytes보다 커질 수 있다.
- 저장 가능한 24~64 MiB snapshot 일부는 base64/32 MiB HTTP 또는 P2P wire 한도 때문에 직접
  전송할 수 없다.
- Mainnet snapshot fast-sync 전에는 chunk manifest, per-chunk hash, finalized state root proof,
  trusted context/historical committee 검증, resume/timeout와 전체 import atomicity가 필요하다.

## 다음 단계

1. ~~Legacy `incremental_hash` 경로 제거~~ — 2026-08-13 완료
2. ~~Fixed component-tree schema-v3 shadow 구현~~ — 2026-08-13 완료
3. Component dirty-subtree cache와 fresh component-root 교차 검증
4. `app_hash = H(version, full_state_root, receipt_root, event_root)` activation 규칙
5. Cross-validator disagreement, restart, Byzantine/chaos 검증
6. 이후 verified chunked snapshot manifest/proof와 indexer bounded-range/cursor API
