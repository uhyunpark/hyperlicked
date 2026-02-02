# Storage and Persistence

Hyperlicked uses RocksDB for persistent storage with a hybrid approach:
- **Always persist**: Blocks, consensus safety state
- **Periodic snapshots**: Application state every N blocks
- **Recovery**: Load snapshot + replay blocks since snapshot

## Enabling Persistence

Set `DATA_DIR` environment variable to enable persistence:

```bash
DATA_DIR=/path/to/data cargo run --bin hl-server
```

Without `DATA_DIR`, the node runs in-memory only (data lost on restart).

---

## RocksDB Column Families

| Column Family | Key | Value | Description |
|--------------|-----|-------|-------------|
| `blocks` | Block hash (32 bytes) | Block JSON | All blocks ever seen |
| `height_index` | Height (u64 big-endian) | Block hash | Height → hash lookup |
| `consensus` | `"state"` | ConsensusState JSON | Safety state |
| `snapshots` | Height (u64 big-endian) | AppSnapshot JSON | App state snapshots |
| `meta` | `"committed_height"` / `"committed_hash"` | Height / Hash | Latest committed block |

---

## Consensus State

The consensus state is persisted on every block commit to ensure safety after crashes:

```rust
pub struct ConsensusState {
    pub high_qc: Option<Certificate>,      // Highest QC seen
    pub locked_qc: Option<Certificate>,    // Locked QC (HotStuff-2 safety)
    pub voted_views: Vec<View>,            // Views we've voted in
    pub current_view: View,                // Current view number
    pub committed_height: u64,             // Last committed block height
    pub committed_hash: Hash,              // Last committed block hash
    pub consecutive_timeouts: u32,         // Pacemaker timeout count
    pub vc_sent_for_view: Option<View>,    // ViewChange deduplication
}
```

**Safety Guarantee**: Voted views are persisted to prevent double-voting after crash. A validator that crashes and restarts will refuse to vote for the same view twice.

---

## App Snapshots

Application state is snapshotted periodically (configurable via `SNAPSHOT_INTERVAL`):

```rust
pub struct AppSnapshot {
    pub height: u64,                       // Block height of snapshot
    pub timestamp: u64,                    // Timestamp of snapshot
    pub accounts: Vec<Account>,            // All accounts with positions
    pub market_configs: Vec<MarketConfig>, // Market configurations
    pub mark_prices: Vec<(Symbol, i64)>,   // Mark prices
    pub insurance_fund: i64,               // Insurance fund balance
    pub funding_rates: Vec<(Symbol, i64)>, // Current funding rates
    pub last_funding_times: Vec<(Symbol, u64)>, // Last funding timestamps
    pub staking: Option<StakingState>,     // Staking state
    pub oracle: Option<OracleState>,       // Oracle state
    pub trigger_orders: Vec<TriggerOrder>, // Pending trigger orders
    pub premium_samples: Vec<(Symbol, Vec<i64>)>, // Funding premium samples
    pub trigger_seq: u64,                  // Trigger order sequence number
}
```

**Default**: Snapshot every 1000 blocks (`SNAPSHOT_INTERVAL=1000`).

---

## Recovery Process

On startup with persistence enabled:

1. **Load consensus state** from `consensus` column family
   - If not found, use genesis state

2. **Load latest snapshot** from `snapshots` column family
   - Find snapshot at or before committed height
   - If not found, use genesis snapshot

3. **Replay blocks** from snapshot height to committed height
   - Execute each block through `AppState::execute()`
   - Rebuilds orderbook state, positions, etc.

4. **Resume consensus** at recovered view
   - Safety module initialized with voted_views
   - Pacemaker starts at current_view

```
┌─────────────────────────────────────────────────────────┐
│                    Recovery Timeline                      │
├─────────────────────────────────────────────────────────┤
│                                                          │
│  Block 0        Block 1000      Block 1500              │
│    │               │               │                    │
│    ▼               ▼               ▼                    │
│  Genesis       Snapshot        Crash/Restart            │
│    └──────────────┘               │                    │
│    Snapshot contains              │                    │
│    all state at 1000              │                    │
│                    └──────────────┘                    │
│                    Replay blocks                        │
│                    1001 → 1500                          │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

---

## Write-Ahead Logging

RocksDB provides write-ahead logging (WAL) automatically. Critical writes use `WriteBatch` for atomic commits:

```rust
fn commit_block(&self, block: &Block, state: &ConsensusState) -> Result<()> {
    let mut batch = WriteBatch::default();

    // Block + height index + consensus state + meta
    // All written atomically
    batch.put_cf(cf_blocks, hash, &block_bytes);
    batch.put_cf(cf_height, height, hash);
    batch.put_cf(cf_consensus, "state", &state_bytes);
    batch.put_cf(cf_meta, "committed_height", height);
    batch.put_cf(cf_meta, "committed_hash", hash);

    self.db.write(batch)?; // Atomic!
}
```

---

## Snapshot Verification

Snapshots can be verified for integrity:

```rust
// Compute hash of snapshot
let hash = compute_snapshot_hash(&snapshot);

// Verify snapshot matches expected height
verify_snapshot_height(&snapshot, expected_height)?;

// Verify snapshot data integrity
verify_snapshot(&snapshot)?;
```

---

## Chain Verification

Block chain integrity can be verified:

```rust
// Verify blocks form valid chain
let result = verify_block_chain(&blocks)?;

// Result contains:
// - first_height / last_height
// - total_blocks
// - total_transactions
// - missing_heights (gaps)
// - hash_mismatches (invalid blocks)
```

---

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `DATA_DIR` | None | Path to RocksDB data directory |
| `SNAPSHOT_INTERVAL` | 1000 | Snapshot every N blocks (0 = disabled) |

---

## Disk Space

Disk usage depends on:
- **Block count**: ~1-2 KB per block (varies with transactions)
- **Snapshot count**: ~10-100 KB per snapshot (varies with account count)
- **Retention**: All blocks and snapshots are retained indefinitely

For high-volume deployments, consider:
- Periodic pruning of old blocks (not yet implemented)
- External archival of old snapshots
- Separate snapshot storage with compression

---

## Backup

To backup a running node:

```bash
# Stop the node first (or use RocksDB's backup API)
cp -r /path/to/data /path/to/backup

# Verify backup integrity
cargo run --bin hl-server -- verify-storage /path/to/backup
```

**Warning**: Copying a RocksDB database while the node is running may result in an inconsistent backup. Use proper backup procedures or stop the node first.

---

## Recovery Scenarios

### Normal Restart
1. Node reads consensus state and snapshot
2. Replays blocks since snapshot
3. Resumes at previous view

### Corruption Recovery
If corruption is detected:
1. Node logs error and halts
2. Operator must restore from backup or resync

### Fresh Start
To start fresh:
```bash
rm -rf /path/to/data
cargo run --bin hl-server
```

### Sync from Peers
RPC nodes can sync from validators:
```bash
NODE_ROLE=rpc PEERS=http://validator1:8080 cargo run --bin hl-server
```
