# Commitment v2 performance baseline

The repository contains a small, dependency-free `cargo bench` harness for
tracking the current artifact encoding/root cost before incremental Commitment
v2 work changes the implementation.

Run it in the repository root:

```bash
cargo bench --bench commitment_v2
```

The bench uses a deterministic workload with one 64-byte `FILL` event per
receipt and runs receipt counts of 100, 1,000, and 5,000. It reports:

- `canonical_bytes`: exact `CommitmentV2::canonical_bytes()` length.
- `encode_ms_avg`: average wall-clock time over three canonical encodings.
- `root_ms_avg`: average wall-clock time over three combined root computations
  (`receipts_root` + `events_root` + combined root).
- `checksum`: a small output sink to keep the measured work observable.

The second table is a current-implementation application baseline. It executes
a block containing the same number of deterministic `System::Deposit`
transactions and reports payload size, total `AppState::execute` time, and a
separate legacy state-hash and schema-v3 full-state-root timing. `execute_ms` includes the current
block validation, action execution, artifact collection, post-transaction
processing, and its final state hash; it is not a microbenchmark of only one
phase.

The third table exercises a speculative child through `CanonicalAppHook`.
`dirty_candidate_execute_ms` includes execution, invariant validation, state
clone, and partial component-tree derivation. It must not be compared as if it
were a leaf-only hash microbenchmark. The following preflight full-tree
cross-check is intentionally outside that timer. `candidate_hit_ms` measures a
fully formed duplicate block lookup, not a component-leaf cache hit.

Timings are host/build dependent. Use release-mode output from `cargo bench`
for comparisons, keep the workload constants unchanged, and compare trends
across revisions rather than treating any wall-clock value as a correctness
threshold. The harness intentionally adds no benchmark dependency and no
timing assertions to the test suite.

## 2026-08-12 local release sample

```text
commitment_v2:
100    receipts: 15,120 bytes   encode: 0.094 ms   root: 0.518 ms
1000   receipts: 151,020 bytes  encode: 1.088 ms   root: 7.045 ms
5000   receipts: 755,020 bytes  encode: 6.970 ms   root: 33.113 ms

app_state:
100    payload: 4,208 bytes    execute: 0.960 ms   full hash: 0.053 ms
1000   payload: 42,008 bytes   execute: 6.378 ms   full hash: 0.632 ms
5000   payload: 210,008 bytes  execute: 41.286 ms  full hash: 5.539 ms
```

## 2026-08-14 dirty component-tree local release sample

```text
accounts  fresh full root  dirty candidate execute  candidate hit
100       0.213 ms         0.525 ms                 0.266 ms
1000      1.728 ms         6.260 ms                 2.295 ms
5000      9.466 ms         28.603 ms                7.831 ms
```

The dirty path currently improves only candidate tree construction. Vote and
commit boundaries still recompute the complete tree as an independent safety
oracle, so this tranche is not an end-to-end throughput win. Removing that
oracle requires stronger mutation encapsulation and a separate activation
decision.

## 2026-08-14 schema-v3 consensus activation sample

After `Block::app_hash` was changed to the schema-v3 full-state root, the same
release harness produced:

```text
accounts  execute     fresh full root  dirty candidate execute  candidate hit
100       0.544 ms    0.067 ms         0.228 ms                 0.067 ms
1000      2.179 ms    0.520 ms         1.988 ms                 0.581 ms
5000      11.131 ms   2.435 ms         10.284 ms                3.060 ms
```

No new block, vote, or QC field was added: the existing 32-byte `app_hash`
now carries the root, so consensus wire size is unchanged. These single-run
local timings are regression evidence, not throughput or latency guarantees.
Fresh vote/commit recomputation remains enabled as the safety oracle.

## 2026-08-21 V5 commitment-root activation sample

The V5 block header adds one 32-byte `commitment_root`. Receipts and events remain in the
persisted Commitment v2 artifact; only their combined root is carried by the block and therefore
authenticated by proposer signatures, votes, and QCs.

```text
receipts/events  canonical bytes  encode       combined roots
100              15,120 bytes     0.101 ms     0.511 ms
1000             151,020 bytes    1.031 ms     5.915 ms
5000             755,020 bytes    7.738 ms     35.195 ms

accounts/txs      execute          fresh full-state root
100               1.307 ms         0.138 ms
1000              8.109 ms         1.546 ms
5000              41.485 ms        10.607 ms
```

These are one-machine release samples, not finality or throughput guarantees. The 5,000-item
case shows that a maximal artifact needs explicit production-hardware and WAN load testing;
the consensus path deliberately keeps the independent root check enabled.
