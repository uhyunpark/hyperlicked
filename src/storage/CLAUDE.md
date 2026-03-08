# Storage Reference

RocksDB-based persistence with column families for blocks, consensus state, and snapshots.

## Column Families

| Name | Key format | Value format | Description |
|------|-----------|--------------|-------------|
| `blocks` | Block hash (32 bytes) | Block JSON | All committed blocks |
| `height_index` | Height (u64 big-endian) | Block hash (32 bytes) | Height → block lookup |
| `consensus` | `"state"` (literal) | ConsensusState JSON | Single consensus state entry |
| `snapshots` | Height (u64 big-endian) | AppSnapshot JSON | Periodic app state snapshots |
| `meta` | `"committed_height"` / `"committed_hash"` | String values | Quick-access metadata |

## ConsensusState Fields

```
high_qc:               QuorumCertificate  // Highest QC seen
locked_qc:             QuorumCertificate  // Locked QC (safety rule)
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

1. Load `ConsensusState` from `consensus` CF
2. Load latest `AppSnapshot` from `snapshots` CF
3. Replay blocks from snapshot height to committed height
4. Resume consensus from `current_view`

## WriteBatch Atomic Commit

Each block commit writes atomically: block data → `blocks` CF, height mapping → `height_index`, updated `committed_height`/`committed_hash` → `meta`.

## Pruning

- `prune_old_blocks(keep_recent: u64)` — removes blocks older than `committed_height - keep_recent`
- `prune_old_snapshots(keep_recent: u64)` — removes snapshots older than latest height - keep_recent

## Key Config

- `DATA_DIR` env var — RocksDB data directory (default: `data/`)
- `SNAPSHOT_INTERVAL` — blocks between snapshots (default: 100)
