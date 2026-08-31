# Storage Reference

RocksDB-based persistence with column families for blocks, consensus state, and snapshots.

## Column Families

| Name | Key format | Value format | Description |
|------|-----------|--------------|-------------|
| `blocks` | Block hash (32 bytes) | Block JSON | Finalized blocks and hash-addressable proposal targets |
| `height_index` | Height (u64 big-endian) | Block hash (32 bytes) | Height → block lookup |
| `consensus` | `"state"` (literal) | ConsensusState JSON | Single consensus state entry |
| `snapshots` | Height (u64 big-endian) | AppSnapshot JSON | Periodic app state snapshots |
| `meta` | `"committed_height"` / `"committed_hash"` | String values | Quick-access metadata |
| `block_artifacts` | Block hash (32 bytes) | Canonical Commitment v2 bytes | Required non-genesis receipt/event bundle authenticated by `Block::commitment_root` |
| `transaction_receipts` | Signed envelope transaction ID (32 bytes) | Versioned canonical receipt index row | Finalized signed transaction ID → block location and receipt; system-action receipts remain block-local and reads re-authenticate against the artifact/root |
| `state_roots` | Block hash (32 bytes) | `u16` little-endian schema version + root (32 bytes) | Versioned schema-v3 root authenticated as `Block::app_hash` |

## ConsensusState Fields

```
high_qc:               QuorumCertificate  // Highest QC seen
locked_qc:             QuorumCertificate  // Locked QC (safety rule)
epoch:                 u64                 // Consensus epoch binding
committee_hash:        Hash                // Active committee binding
genesis_hash:          Hash                // Chain/genesis domain binding
voted_views:           HashSet<View>      // Views we voted in
current_view:          View               // Current consensus view
committed_height:      Height             // Last committed block height
committed_hash:        Hash               // Last committed block hash
consecutive_timeouts:  u32                // Timeout counter for pacemaker
vc_sent_for_view:      Option<View>       // View change message tracking
```

## AppSnapshot Fields

```
height:           u64                         // Block height at snapshot
timestamp:        u64                         // Block timestamp (ms)
accounts:         Vec<Account>                // All accounts with balances/positions
market_configs:   Vec<MarketConfig>           // Market configurations
mark_prices:      Vec<(Symbol, Price)>        // Mark prices per symbol
insurance_fund:   i64                         // Insurance fund balance (cents)
funding_rates:    Vec<(Symbol, i64)>          // Current funding rates (bps)
last_funding_times: Vec<(Symbol, u64)>        // Last funding payment times (ms)
staking:          Option<StakingState>        // Validators, delegations, epochs
oracle:           Option<OracleState>         // Oracle price feeds
trigger_orders:   Vec<TriggerOrder>           // TP/SL trigger orders
premium_samples:  Vec<(Symbol, Vec<i64>)>     // Premium samples for funding
trigger_seq:      u64                         // Trigger order sequence number
mark_price_ema:   Vec<(Symbol, Price)>        // Mark price EMA per symbol
```

**Not snapshotted** (rebuilt from block replay): orderbook open orders, mempool, trade history.

## Recovery Flow

1. Load `ConsensusState` and committed metadata from `consensus`/`meta` CFs
2. Load finalized blocks from the genesis height through `committed_height`
3. Validate parent links, contexts, QC references, and committee signatures
4. Replay the complete chain through the canonical application hook
5. Resume consensus from `current_view`

`AppSnapshot` is not used for canonical restart recovery because it omits
orderbook state. It remains available for sync and operational tooling.

Snapshot storage/import is fail-closed: raw JSON is checked against the
64 MiB `MAX_APP_SNAPSHOT_BYTES` bound before deserialization, and decoded
cardinalities that already have consensus invariants (pending nonces and the
active-validator/liveness set) are checked before an application state is
constructed. Snapshot writes use the same bounded encoder, so oversized or
malformed values are never persisted as valid snapshots. Other state totals
remain governed by their existing runtime/application validation rather than
introducing a snapshot-only consensus cap.

## WriteBatch Atomic Commit

Each finalized commit writes atomically with WAL fsync: block data → `blocks`
CF, height mapping → `height_index`, consensus state → `consensus`, and updated
`committed_height`/`committed_hash` → `meta`. Proposal targets use a separate
hash-only, fsynced write and never replace a finalized height index. When
provided, canonical Commitment v2 bytes and the derived `transaction_receipts`
rows are written in the same synced batch.
Every non-genesis commit requires the artifact and verifies its combined root
against `Block::commitment_root`; raw-byte callers cannot bypass this check.
Every non-genesis commit includes the versioned full-state-root record in the
same batch and it must equal the block's authenticated `app_hash`. Raw
32-byte roots, unsupported schema versions, and malformed fixed-width records
are rejected during load/restart.

## Pruning

- `prune_old_blocks(keep_recent: u64)` — removes blocks older than `committed_height - keep_recent`
- `prune_old_snapshots(keep_recent: u64)` — removes snapshots older than latest height - keep_recent

Do not enable block pruning in the canonical validator runtime yet. Restart
currently replays from genesis; pruning becomes safe only after a verified,
state-root-bound snapshot anchor recovery path is implemented.

## Key Config

- `DATA_DIR` env var — RocksDB data directory (default: `data/`)
- `SNAPSHOT_INTERVAL` — blocks between snapshots (default: 1000)
