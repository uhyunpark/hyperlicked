# Multi-Node Consensus Guide

Running multiple validator nodes to test HotStuff-2 consensus.

## Quick Start

### Local 3-Node Cluster

Open three terminal windows and run:

```bash
# Terminal 1 - Node 0 (leader for view 0)
cargo run --bin multinode -- --node 0

# Terminal 2 - Node 1 (leader for view 1)
cargo run --bin multinode -- --node 1

# Terminal 3 - Node 2 (leader for view 2)
cargo run --bin multinode -- --node 2
```

Nodes will automatically connect to each other via TCP on ports 9000-9002.

### With BLS Signatures

Enable BLS signature aggregation for production-like testing:

```bash
ENABLE_BLS=1 cargo run --bin multinode -- --node 0
ENABLE_BLS=1 cargo run --bin multinode -- --node 1
ENABLE_BLS=1 cargo run --bin multinode -- --node 2
```

## What to Expect

After starting all 3 nodes, you should see:

1. **Connection establishment** (first ~2 seconds)
   ```
   Network listening
   Peer connected
   ```

2. **Consensus progress**
   ```
   Running as LEADER
   Proposing block height=1
   Collected quorum, forming QC
   COMMITTED block height=0
   ```

3. **View rotation** - Leadership rotates every block:
   - View 0, 3, 6... → Node 0 is leader
   - View 1, 4, 7... → Node 1 is leader
   - View 2, 5, 8... → Node 2 is leader

## Configuration

### Default Ports

| Node | Listen Address | Node ID |
|------|----------------|---------|
| 0    | 127.0.0.1:9000 | `[1u8; 32]` |
| 1    | 127.0.0.1:9001 | `[2u8; 32]` |
| 2    | 127.0.0.1:9002 | `[3u8; 32]` |

### Consensus Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| Validators | 3 | Number of validator nodes |
| Quorum | 2 | Votes needed for QC (2f+1) |
| Byzantine fault tolerance | 0 | Max Byzantine nodes (f = (n-1)/3) |
| View timeout | 3000ms | Time before view change |

## Architecture

### Network Layer

```
┌─────────────────────────────────────────────────────┐
│                  TcpNetwork                         │
│  ┌─────────────┐      ┌─────────────────────┐      │
│  │   Listener  │      │   Peer Connections  │      │
│  │  (accept)   │      │  HashMap<NodeId,    │      │
│  │             │      │    TcpStream>       │      │
│  └─────────────┘      └─────────────────────┘      │
└─────────────────────────────────────────────────────┘
```

### Consensus Flow

```
┌──────────────┐   Propose    ┌──────────────┐
│    Leader    │──────────────▶│  Followers   │
│   (Node N)   │              │              │
│              │◀─────────────│              │
│              │    Votes     │              │
│              │──────────────▶│              │
│              │   Prepare    │              │
│              │   (with QC)  │              │
└──────────────┘              └──────────────┘
```

### Transaction Propagation

Transactions are propagated via block payloads:

1. **Leader prepares payload** - Serializes pending transactions
2. **Leader proposes block** - Includes serialized transactions in payload
3. **Followers execute** - Deserialize and execute transactions from payload
4. **All nodes agree** - Same transactions = same state hash

## Testing

### Automated Tests

Run the multi-node test suite:

```bash
cargo test --test multinode -- --nocapture
```

Tests include:
- `test_three_nodes_reach_consensus` - Basic consensus
- `test_transactions_included_in_blocks` - Transaction propagation
- `test_leader_rotation` - Round-robin leadership
- `test_quorum_calculation` - BFT quorum math

### Manual Testing

1. **Start all nodes** - See Quick Start above

2. **Verify consensus**
   ```
   # Look for "COMMITTED block" messages on all nodes
   # Block heights should increase
   # Block hashes should match across nodes
   ```

3. **Test view change** (kill the leader)
   ```
   # Kill the current leader (Ctrl+C)
   # Remaining nodes should timeout and change views
   # New leader should take over
   ```

## Hardware Requirements

### Local Testing (3 nodes on 1 machine)

| Resource | Minimum |
|----------|---------|
| CPU | 4+ cores |
| RAM | 8 GB |
| Storage | In-memory (no persistence) |

### Production (per node)

| Resource | Recommended |
|----------|-------------|
| CPU | 8+ cores |
| RAM | 16-32 GB |
| Storage | 500 GB+ NVMe SSD |
| Network | 1 Gbps, low latency |

## Troubleshooting

### "Connection refused" errors

Ensure all nodes are running and ports are available:

```bash
# Check if ports are in use
lsof -i :9000
lsof -i :9001
lsof -i :9002
```

### "Timeout waiting for proposal"

- Check network connectivity between nodes
- Verify all nodes have the same validator list
- Increase `view_timeout_ms` if network is slow

### Nodes not reaching consensus

1. Check logs for "Failed to collect quorum"
2. Verify at least 2 of 3 nodes are running
3. Check for app_hash mismatches (Byzantine detection)

### Different app_hash on nodes

This indicates non-deterministic execution:

- Check for floating-point operations (use integer math)
- Verify all nodes process the same transactions
- Check transaction ordering in mempool

## Future Work

- [ ] Byzantine fault testing
- [ ] Performance benchmarks
- [ ] Testnet deployment
- [ ] Multi-machine testing
- [ ] Persistent storage for validators
