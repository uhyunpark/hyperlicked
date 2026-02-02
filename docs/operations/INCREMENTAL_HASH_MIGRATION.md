# Incremental Hash Migration Guide

This document describes the incremental hash feature, when to use it, and how to safely migrate a network.

## Overview

Hyperlicked supports two state hashing algorithms:

1. **Full Hash (default)**: O(n) - Hashes all accounts and state every block
2. **Incremental Hash (feature flag)**: O(k) - Only rehashes changed accounts, where k << n

The incremental hash is **disabled by default** because it produces different hash values than the full hash algorithm.

## Algorithm Differences

### Full Hash (`compute_state_hash_full`)

Iterates through all state in a deterministic order:
1. All accounts (sorted by address)
2. All orderbooks (sorted by symbol)
3. All mark prices (sorted)
4. Insurance fund
5. All funding rates (sorted)
6. All staking state
7. All trigger orders
8. Oracle prices

Hash: `H(accounts || orderbooks || prices || insurance || funding || staking || triggers || oracle)`

### Incremental Hash (`incremental_hash` feature)

Uses a bucket-based approach:
1. Accounts divided into 256 buckets by address hash prefix
2. Each bucket maintains its own hash
3. Only dirty buckets are rehashed on state changes
4. Root hash combines all bucket hashes + globals hash

Hash: `H(bucket_00 || bucket_01 || ... || bucket_ff || globals_hash)`

### Why Different Values?

The algorithms produce different hash values because:
- Full hash includes more detailed state (individual order fields, staking status enums)
- Incremental hash uses a summary-based approach for global state
- The bucket structure adds an extra layer of hashing

Both are **deterministic** - the same state always produces the same hash with the same algorithm.

## When to Enable Incremental Hash

Consider enabling incremental hash when:
- Account count exceeds ~100,000
- Block execution time is dominated by hashing (~10ms+ for hashing)
- Network latency allows for coordinated upgrade

Do NOT enable incremental hash:
- During initial network bootstrap (overhead not worth it)
- If you cannot coordinate a network-wide upgrade
- If you need to maintain backward compatibility with external state verifiers

## Migration Procedure

### Prerequisites

- All validators must upgrade simultaneously (within a single epoch)
- Coordinate a specific block height for the switchover
- Ensure all nodes have the same software version

### Step 1: Prepare All Nodes

Build the new binary with incremental hash support:

```bash
cargo build --release --features incremental_hash
```

Deploy the binary to all validator and RPC nodes but do NOT restart yet.

### Step 2: Announce Upgrade Block Height

Coordinate with all operators to agree on:
1. A specific block height for the upgrade (e.g., height 1,000,000)
2. A time window when all nodes must restart

Choose a block height that:
- Is at least 1 epoch (90 minutes) in the future
- Falls during low-activity periods if possible
- Gives operators buffer time for unexpected issues

### Step 3: Upgrade Validators (Coordinated Restart)

At the agreed time, all validators must:

1. Stop the validator process
2. Update environment (if using feature flags at runtime)
3. Restart with the new binary

```bash
# Stop current validator
systemctl stop hl-validator

# Start with incremental hash
INCREMENTAL_HASH=true systemctl start hl-validator
```

The first block after restart will:
1. Initialize all 256 buckets as dirty
2. Compute the first incremental hash
3. This hash will differ from the previous full hash

Because all validators switch simultaneously, they will all compute the same new hash, and consensus continues normally.

### Step 4: Upgrade RPC Nodes

After validators have successfully upgraded (verify by checking block production continues):

1. Stop each RPC node
2. Delete local state or start from fresh snapshot
3. Restart with the new binary

RPC nodes must resync from validators because their local state hash won't match.

### Step 5: Verify

Check that:
- Blocks are being produced
- All validators are participating
- App hashes match across nodes (check logs for "app hash mismatch" errors)

```bash
# Check validator health
curl http://localhost:8080/api/v1/chain/health | jq

# Compare hashes across nodes
for node in node1 node2 node3; do
  curl http://$node:8080/api/v1/sync/status | jq '.stateHash'
done
```

## Rollback Procedure

If issues occur during migration:

### Option 1: Coordinated Rollback (Before Epoch Transition)

If caught early (same epoch as upgrade):

1. Stop all validators
2. Restore previous binary (or unset incremental hash flag)
3. Restore state from pre-upgrade snapshot
4. Restart all validators simultaneously

### Option 2: Full Resync (After Epoch Transition)

If the network has progressed significantly:

1. Stop affected nodes
2. Delete all persistent data (`DATA_DIR`)
3. Restore previous binary
4. Resync from genesis or trusted snapshot

This is more disruptive but ensures clean state.

### Option 3: Continue Forward (Recommended)

If the network is stable with incremental hash:

1. Keep running with incremental hash
2. Address any performance/correctness issues in code
3. Only rollback if consensus fails

## Monitoring During Migration

Watch for these log messages:

**Healthy migration:**
```
INFO Epoch transition: updating validator set
INFO Block committed height=1000000 hash=abcd...
```

**Problems:**
```
ERROR App hash mismatch - expected abc..., got def...
WARN State corruption detected - operator intervention required
```

If you see hash mismatches:
1. Check all validators are running the same binary
2. Check all validators have the same feature flags
3. If only some nodes have issues, they may need to resync

## Performance Comparison

| Metric | Full Hash | Incremental Hash |
|--------|-----------|------------------|
| Time (1K accounts) | ~1ms | ~1ms |
| Time (100K accounts) | ~100ms | ~5ms* |
| Time (1M accounts) | ~1s | ~10ms* |
| Memory overhead | None | ~8KB (bucket hashes) |

*Assumes typical trading activity (1-10% of accounts modified per block)

## Configuration

The incremental hash is controlled by a Cargo feature flag:

```toml
# Cargo.toml
[features]
incremental_hash = []
```

Build with: `cargo build --features incremental_hash`

There is currently no runtime toggle - the algorithm is chosen at compile time.

## Future Work

Potential improvements being considered:

1. **Shadow mode verification**: In debug builds, compute both hashes and verify they're tracking the same state changes (not the same value, but same behavior)

2. **Gradual migration**: Protocol-level support for hash algorithm versioning, allowing mixed networks during transition

3. **Runtime toggle**: Environment variable to switch algorithms without recompilation

## References

- `src/app/state/incremental_hash.rs` - Incremental hash implementation
- `src/app/state/consensus.rs` - Hash selection logic
- `docs/blockchain/ROADMAP.md` - Feature status
