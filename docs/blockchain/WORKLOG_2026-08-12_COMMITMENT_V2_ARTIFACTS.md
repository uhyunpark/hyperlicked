# Commitment v2 execution artifacts

> Historical status: this document records the original deterministic shadow phase.
> The combined root was activated as `Block::commitment_root` on 2026-08-21; see the
> [activation worklog](WORKLOG_2026-08-21_COMMITMENT_V2_CONSENSUS_ACTIVATION.md).

## Why this tranche exists

The previous application hash was enough for validators to compare one opaque
execution result, but it did not provide an indexer contract for answering:

- which transaction occupied each block position,
- whether it succeeded or failed,
- which stable error category it produced,
- which transaction-scoped events it emitted, and
- which deterministic protocol events occurred after transaction execution.

Commitment v2 introduces that missing execution-artifact boundary before the
full state commitment is changed. Keeping it in shadow mode first lets local
single-node and multi-validator runs compare deterministic results without
changing the consensus header format prematurely.

## Indexer contract

An indexer can use these stable keys:

```text
block:        (height, block_hash)
receipt:      (height, tx_index)
transaction:  tx_id
tx event:     (height, tx_index, event_index)
system event: (height, system_event_index)
```

Each receipt stores a numeric transaction type, success/failure status, stable
numeric error code, fixed-width resource counters, and ordered events. Signed
transaction IDs hash the exact canonical signed-envelope bytes. Protocol/local
system actions use their canonical action identity. The finalized block payload
remains the source for decoding the original action and signer.

Events produced by one action are attached to that receipt. Liquidation, ADL,
funding, and trigger processing performed by deterministic post-transaction
phases are stored as separate system events; they are not incorrectly assigned
to the last user transaction. All indices are contiguous and validated.

The schema has independent domains for receipt leaves, transaction-event
leaves, system-event leaves, receipt/event roots, and the combined root. It
uses explicit numeric versions and bounded canonical bincode encoding. Unknown
future numeric types can be retained by an indexer instead of failing enum
decoding.

## Finality and persistence order

The canonical runtime now uses this order:

```text
speculative execute
  -> validate Commitment v2
  -> vote/finalize
  -> synced RocksDB WriteBatch(block + height index + consensus state + artifact)
  -> promote canonical AppState
  -> publish BlockCommitted/API/WebSocket visibility
```

Unfinalized proposals never receive a queryable artifact. A finalized block and
its artifact are committed in the same synced batch, and a retry cannot replace
an existing artifact with different bytes. On restart, `hl-node` re-executes
the finalized chain and compares every stored Commitment v2 artifact before
promoting application state. A non-genesis finalized block with a missing
artifact fails recovery; there is no legacy compatibility exception because
this chain has not been opened publicly.

This makes an indexer restart-safe: notification delivery is only a wake-up
signal; the durable finalized height and RocksDB artifact are the source of
truth. An indexer can resume from its own last processed height and replay.

## Determinism and resource boundaries

- receipt order follows block payload order;
- event order follows deterministic execution phase order;
- funding markets and accounts are sorted before state mutation/event output;
- trigger markets and trigger IDs are sorted;
- liquidation priority has an address tie-breaker;
- receipts, events, payload bytes, individual receipts, and the complete
  artifact have hard bounds;
- artifact construction failure rejects before a vote or durable proposal
  write in the canonical runtime.

The application emits zeroed resource counters for now. Real compute/storage
metering must be introduced as a separately reviewed consensus rule rather than
filled from machine-dependent wall-clock values.

## Performance checkpoint

The dependency-free benchmark is documented in
[COMMITMENT_V2_BENCHMARK.md](COMMITMENT_V2_BENCHMARK.md). On the development
machine used for this tranche, release-mode samples were:

| Receipts/events | Canonical bytes | Encode | Combined root |
| ---: | ---: | ---: | ---: |
| 100 | 15,120 | 0.094 ms | 0.518 ms |
| 1,000 | 151,020 | 1.088 ms | 7.045 ms |
| 5,000 | 755,020 | 6.970 ms | 33.113 ms |

These are comparison samples, not performance guarantees. The canonical hook
reuses a speculative execution result and only performs private replay when a
candidate is unavailable, such as recovery. Commitment preflight borrows the
candidate artifact instead of cloning the complete account/orderbook state,
and transient artifacts are removed when a candidate is promoted so later
state clones do not carry the preceding block's bundle. No wall-clock
assertion is placed in CI.

At the time of this historical measurement, the live shadow path validated and persisted
canonical bytes without placing the combined root in a block header. The 2026-08-21 V5
activation now places it in the dedicated `commitment_root` field rather than `app_hash`.
At 1,000 deterministic deposit actions the complete release-mode application
execution baseline was 6.378 ms on the same machine; RocksDB fsync latency must
be measured separately under the intended hardware and topology.

## Verification completed

- `cargo test --all-targets --all-features --locked`: passed, including 476
  library tests; zero failed and zero ignored.
- `cargo bench --bench commitment_v2`: passed.
- single-node durable restart: height 0 -> 3, restart on the same RocksDB path,
  then height 3 -> 5; persisted commitments regenerated and matched.

## What is deliberately still missing

1. **Consensus activation:** the Commitment v2 root is not yet included in the
   signed block result/QC. External consumers therefore cannot prove a receipt
   from the block header alone.
2. **Full state commitment:** accounts, positions, order books, staking,
   oracle, and all other consensus state still need a versioned complete root
   and snapshot proof design.
3. **External indexer transport:** storage read methods exist by block hash and
   finalized height, but an independent process still needs a bounded finalized
   range/export API or durable stream cursor.
4. **Membership proofs:** the current ordered roots commit the full list but do
   not expose per-receipt/event Merkle proofs.
5. **Stable public payload specification:** event type IDs are stable, while
   the typed payload schemas and cross-language encoding contract still need a
   published versioned specification before third-party indexers are promised
   compatibility.
6. **Existing scaling debt:** speculative candidate count needs a hard bound,
   finalized range reads should stop at the requested limit instead of loading
   the remaining chain tail, and the no-candidate recovery fallback still
   replays once for preflight and once for canonical promotion.
7. **Stable trigger failure codes:** failed trigger events currently preserve a
   human-readable reason string. Replace it with a versioned numeric reason
   before promising long-term third-party payload compatibility.

The safe next sequence is full-state schema and deterministic vectors, then
`app_hash = H(version, state_root, receipt_root, event_root)`, followed by
snapshot/range proof verification and the external indexer export API. Because
the project has no public chain history yet, that activation can be a clean
genesis/protocol version change without a backward migration.
