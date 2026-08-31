# Commitment v2 Consensus Activation

> 날짜: 2026-08-21
> 상태: 로컬 genesis-wide hard fork 완료, **NOT MAINNET READY**

## 의도

Commitment v2는 transaction receipt와 transaction/system event를 deterministic하게 만들고
RocksDB에 원자적으로 저장했지만, 이전 shadow 단계에서는 block/QC가 그 bytes의 root를
인증하지 않았다. 따라서 validator replay에는 유용했어도 외부 indexer가 finalized block만으로
artifact가 합의 결과임을 확인할 수는 없었다.

## 활성화 규칙

- `Block::app_hash`는 계속 schema-v3 full-state root다.
- 새 `Block::commitment_root`는 Commitment v2의 receipts/events combined root다.
- V5 block hash가 두 root를 포함하므로 proposer signature, Vote와 QC가 둘을 함께 인증한다.
- genesis domain V3는 block protocol V5, state schema v3, Commitment schema/version을 포함한다.
- 구 DB, V4 block, V2 genesis domain과 이전 PoP는 migration하지 않는다.

새 필드는 receipts/events 전체를 wire에 복제하지 않고 32-byte combined root 하나만 추가한다.
Indexer는 저장 artifact에서 receipts root와 events root를 재계산한 뒤 header root와 비교할 수
있다. 현재 root는 ordered flat commitment이므로 개별 항목 Merkle inclusion proof는 제공하지 않는다.

## 실행 lifecycle

```text
execute block
  -> schema-v3 state root
  -> transient receipts/events
  -> Commitment v2 combined root
  -> candidate를 최종 V5 block hash로 re-key
  -> 두 root fresh preflight
  -> proposer signature / broadcast / Vote / QC
```

Proposer만 root를 계산하기 전 zero placeholder를 잠시 사용한다. 서명, follower/observer vote,
commit, recovery, storage에서는 non-genesis zero/missing/mismatch를 모두 거부한다. Follower는
제시된 root를 로컬 실행 artifact로 재현하기 전 safety state나 vote intent를 변경하지 않는다.

## 저장 및 복구

- finalized block, consensus state, Commitment v2 bytes, schema-v3 state-root row를 하나의 synced
  RocksDB batch로 기록한다.
- storage는 non-genesis artifact 누락, non-canonical encoding, `commitment.root()`와 header root
  불일치를 write 전에 거부한다. raw artifact commit 경로도 이 검사를 우회하지 못한다.
- finalized replay는 stored artifact와 재실행 artifact가 같고 그 root가 header와 같은지 확인한다.
- persisted high/locked-QC speculative branch도 전체 branch를 staging하고 두 root가 모두 맞은
  뒤에만 candidate map에 publish한다.
- ActiveSync는 여전히 verified-download-only다. application-executed importer가 구현되기 전에는
  다운로드한 block을 canonical state로 직접 적용하지 않는다.

## 검증 및 성능

- V5 block hash가 state root와 commitment root mutation을 모두 감지
- proposer derive/seal/re-key 후 최종 candidate hash 유지
- follower/observer wrong 또는 missing root를 vote 전에 거부
- direct commit/storage/recovery mismatch를 fail closed
- single/4-validator V3 genesis PoP fixture 검증
- single-node fresh start `0 -> 1`, 같은 RocksDB restart `1 -> 2`
- `cargo test --locked --all-targets --all-features`: 실패 0, ignored 0
- 2026-08-21 local release sample: 1,000 receipt/event combined-root 약 5.915 ms

상세 수치는 [Commitment v2 benchmark](COMMITMENT_V2_BENCHMARK.md)에 기록했다. V5 header의
고정 wire 증가는 block당 32 bytes이며 Vote/QC에 별도 root 필드는 추가하지 않았다.

## 남은 mainnet gate

- application-executed verified block import와 chunked snapshot manifest/proof
- bounded artifact/range API 및 receipt/event Merkle inclusion-proof schema
- epoch transition certificate와 historical committee registry
- bridge proof/accounting, 장기 WAN/Byzantine/fault-injection 테스트와 독립 감사

## 다음 P1 권장 작업

현재 `hl-node` canonical commit 경로에서는 이번 활성화를 막는 P0가 남아 있지
않다. 다만 mainnet 준비를 위해 다음 범위를 별도 tranche로 처리해야 한다.

1. speculative candidate/pending map에 count, bytes, height-window 상한을 둔다. commit이
   지연되거나 Byzantine proposer가 지속적으로 후보를 만들어도 state clone이
   무한히 메모리를 소모하지 않아야 한다.
2. ActiveSync 전체 download window에 block/byte 총량 상한을 둔다. 현재는 각
   HTTP response는 32 MiB로 제한하지만 target height까지의 block을 하나의
   `Vec<Block>`에 누적한다.
3. production에서 사용하지 않는 legacy `Engine`을 test-only로 gate하거나 제거한다.
   이 경로는 canonical runner와 달리 application artifact/state-root를 하나의 atomic
   commit으로 저장하지 않는다.
4. public recovery constructor에 application replay handshake를 강제한다. production node는
   이미 별도 replay를 수행하지만, 재사용 가능한 API 경계 자체가 fail-closed여야
   다른 binary가 실수로 우회하지 못한다.
