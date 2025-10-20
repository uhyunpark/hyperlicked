# Consensus Layer (HotStuff BFT)

## Overview

HotStuff-style Byzantine Fault Tolerant (BFT) consensus implementation.
- **2-chain commit rule**: Block N commits when we have Certificate(N) and Certificate(N+1)
- **Leader/Follower model**: Leader actively proposes, followers reactively respond
- **Pipelined execution**: Execute block at view N, commit at view N+2
- **AppHash verification**: Validators agree on both transactions AND resulting state

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Engine (Run Loop)                     │
│  - Leader: propose → collect votes → make QC → broadcast    │
│  - Follower: wait for propose → vote → wait for prepare     │
└────────────┬─────────────┬─────────────┬────────────────────┘
             │             │             │
      ┌──────▼──────┐ ┌───▼────┐ ┌─────▼────────┐
      │   Safety    │ │  Pace  │ │   Network    │
      │  (voting)   │ │ maker  │ │  (libp2p)    │
      └─────────────┘ └────────┘ └──────────────┘
             │
      ┌──────▼──────┐
      │    State    │
      │ (HighCert,  │
      │  Locked)    │
      └─────────────┘
```

## Core Types (`types.go`)

### Block
```go
type Block struct {
    Height   Height   // Monotonic block number (commits only)
    View     View     // Monotonic view number (every round)
    Parent   Hash     // Hash of parent block (consensus hash)
    AppHash  Hash     // Application state hash (set after execution)
    Payload  []byte   // Transactions
    Proposer NodeID   // Leader who proposed this block
    Time     time.Time
}
```

**Key point**: `Parent` links to parent's **BlockHash**, NOT parent's AppHash.

### Certificate (QC)
```go
type Certificate struct {
    View    View     // View number when this QC was formed
    H       Hash     // Consensus hash (HashOfBlock)
    AppHash Hash     // Application state hash (agreed by 2f+1)
    Sig     []byte   // Aggregated signature
}
```

**Key point**: Certificate proves 2f+1 validators agree on BOTH block transactions AND resulting state.

### Vote
```go
type Vote struct {
    View     View
    H        Hash     // Block hash (consensus)
    AppHash  Hash     // State hash after execution
    SigShare []byte   // BLS signature share
    From     NodeID
}
```

**Key point**: Validators execute block BEFORE voting, include AppHash in vote.

### Hash Separation

**Two separate hashes**:
1. **BlockHash (consensus)** - `HashOfBlock(b)` - commits to transactions
   - Includes: height, view, parent, payload, proposer, time
   - **Excludes**: AppHash (unknown at proposal time)

2. **AppHash (state)** - computed by app after execution
   - Includes: application state after executing transactions
   - Example: orderbook state, account balances, positions

**Why separate?**
- Blocks proposed BEFORE execution (AppHash unknown)
- Certificate commits to consensus data (transactions)
- AppHash verified separately after execution (Byzantine detection)

## State Machine (`state.go`)

```go
type State struct {
    Q        Quorum        // N validators, need 2f+1 votes
    SelfID   NodeID        // This node's identity
    Height   Height        // Last committed block height
    View     View          // Current view number
    Locked   *Locked       // Locked block (2-chain rule)
    HighCert *Certificate  // Highest QC seen
    Blocks   map[Hash]Block
    Genesis  Block
}
```

**Invariants**:
- `Height` only increments on commit (2-chain rule satisfied)
- `View` increments every round (even if no commit)
- `View ≥ Height` always (can propose blocks faster than committing)
- Genesis: height=0, view=0, parent=zero, AppHash=zero

## Safety Module (`safety.go`)

Enforces voting rules to prevent conflicting commits.

### Key Functions

**`CanVote(p Propose) bool`**
- Returns true if safe to vote for this proposal
- Check: `p.HighCert.View >= Locked.Cert.View` (HotStuff locking rule)

**`OnPrepare(cert Certificate, b Block)`**
- Called when receiving prepare message (QC broadcast)
- Updates HighestCert if newer
- Stores block for future reference

**`UpdateLock(cert Certificate, b Block)`**
- Called on commit (2-chain rule satisfied)
- Updates locked block and certificate
- Prevents voting for conflicting chains

**`BlockByHash(h Hash) (Block, bool)`**
- Retrieves block by consensus hash
- Used by Leader to find parent block

## Pacemaker (`pacemaker.go`)

Controls view advancement and timing.

### Timers
```go
type PacemakerTimers struct {
    Ppc   time.Duration  // Case-2 wait time (e.g., 150ms)
    Delta time.Duration  // Network upper bound (e.g., 50ms)
}
```

**HotStuff-2 timing**:
- **Case-1 (fast path)**: If HighCert.View = View-1, skip wait
- **Case-2 (slow path)**: Wait Ppc+Δ for stragglers

### Reactive View Advancement

**`WaitForViewAdvance(ctx, targetView)`**
- Follower waits for prepare message
- Returns when prepare received OR timeout
- Advances state.View when unblocked

**`SignalViewAdvance(v View)`**
- Called by `onPrepare` when receiving prepare
- Wakes waiting followers via channel

**Why reactive?**
- Followers don't know when leader will propose
- Avoids busy-waiting loops
- Efficient CPU usage in idle periods

## Engine (`engine.go`)

Main consensus loop implementing HotStuff standard.

### Run Loop

```go
func (e *Engine) Run(ctx context.Context) error {
    for {
        v := e.State.View + 1
        leader := e.Elector.LeaderOf(v)

        if leader == e.ID {
            // Leader: actively propose
            e.leaderRound(ctx, v)
            e.State.View = v
        } else {
            // Follower: reactively wait
            e.PM.WaitForViewAdvance(ctx, v)
        }
    }
}
```

### Leader Round

**`leaderRound(ctx, v View)`**

1. **Propose**: Create block with HighCert as parent
   ```go
   block := Block{
       Height: height+1,
       View: v,
       Parent: HighCert.H,  // Link to parent block hash
       Payload: app.PreparePayload(),
       Proposer: e.ID,
   }
   ```

2. **Broadcast**: Send propose message to all validators

3. **Collect Votes**: Wait for 2f+1 votes
   - Votes sent via **unicast stream** to leader (not broadcast)
   - Each vote includes AppHash after execution

4. **Verify AppHash Agreement**:
   ```go
   agreedAppHash := votes[0].AppHash
   for _, vote := range votes {
       if vote.AppHash != agreedAppHash {
           return fmt.Errorf("Byzantine fault: AppHash mismatch")
       }
   }
   ```

5. **Form QC**: Aggregate signatures into certificate
   ```go
   cert := Certificate{
       View: v,
       H: HashOfBlock(block),
       AppHash: agreedAppHash,  // Agreed state commitment
       Sig: aggregate(votes),
   }
   ```

6. **Broadcast Prepare**: Send QC to all validators

### Follower Handlers

**`onPropose(ctx, p Propose)`**

1. **Safety Check**: `if !Safety.CanVote(p) { return }`

2. **Execute Block**: Compute AppHash BEFORE voting
   ```go
   appHash := e.App.OnCommit(p.Block)
   ```

3. **Create Vote**: Include AppHash in vote
   ```go
   vote := Vote{
       View: p.Block.View,
       H: HashOfBlock(p.Block),
       AppHash: appHash,  // Commit to state
       SigShare: sign(H),
       From: e.ID,
   }
   ```

4. **Send Vote**: Unicast to leader (not broadcast)

**`onPrepare(ctx, cert Certificate, blk Block)`**

1. **Update HighCert**: `Safety.OnPrepare(cert, blk)`

2. **Signal Followers**: `PM.SignalViewAdvance(cert.View)`

3. **Check 2-Chain Commit Rule**:
   ```go
   // Need: Certificate(N-1), Certificate(N), Block(N).Parent == Cert(N-1).H
   if cert.View == 0 { return }

   prevCert := Store.GetCert(cert.View - 1)
   if !ok { return }

   childBlk := blk
   if childBlk.Parent != prevCert.H { return }  // Chain mismatch

   prevBlk := Store.GetBlock(prevCert.H)
   if !ok { return }

   // COMMIT prevBlk
   appHash := prevCert.AppHash  // Use agreed AppHash from certificate
   State.Height++
   Safety.UpdateLock(prevCert, prevBlk)
   Store.SetCommitted(HashOfBlock(prevBlk))
   ```

4. **Log Commit**: Show height, committed_view, txs, apphash

## 2-Chain Commit Rule

**Rule**: Block N commits when we have Cert(N) and Cert(N+1) where Block(N+1).Parent == Cert(N).H

**Example Timeline**:
```
View 0: Genesis (height 0)

View 1: Leader proposes Block1 (parent=genesis)
  → Get Certificate1 (2f+1 votes)
  → State.View = 1

View 2: Leader proposes Block2 (parent=Block1)
  → Get Certificate2 (2f+1 votes)
  → State.View = 2
  → onPrepare checks: Block2.Parent == Certificate1.H? YES
  → COMMIT Block1 ✅ (height 1, committed_view=1)

View 3: Leader proposes Block3 (parent=Block2)
  → Get Certificate3 (2f+1 votes)
  → State.View = 3
  → onPrepare checks: Block3.Parent == Certificate2.H? YES
  → COMMIT Block2 ✅ (height 2, committed_view=2)
```

**Why some blocks don't commit**:
- Chain break: `Block(N+1).Parent != Cert(N).H`
- Missing certificate: `GetCert(view-1)` fails
- Missing block: `GetBlock(prevCert.H)` fails

**Result**: `View ≈ Height` in normal operation, `View > Height` when blocks skip commits

## Leader Election (`leader.go`)

```go
type RoundRobinElector struct{ IDs []NodeID }

func (r RoundRobinElector) LeaderOf(v View) NodeID {
    return r.IDs[(v-1) % len(IDs)]
}
```

**Current**: Simple round-robin
**Future**: VRF-based or stake-weighted selection

## Network Layer (`pacemaker.go` interface)

```go
type Network interface {
    BroadcastPropose(ctx, p Propose) error
    BroadcastPrepare(ctx, cert Certificate) error
    SendVote(ctx, to NodeID, v Vote) error
    CollectVotes(ctx, view View, h Hash, need int) ([]Vote, error)
    SetHandlers(h Handlers)
}
```

**Implementation**: `pkg/p2p/libp2pnet.go`
- Propose/Prepare: Broadcast via pubsub
- Vote: Unicast via libp2p stream (HotStuff standard)

## Storage Layer (`types.go` interfaces)

```go
type BlockStore interface {
    SaveBlock(b Block)
    GetBlock(h Hash) (Block, bool)
    SaveCert(c Certificate)
    GetCert(v View) (Certificate, bool)
    SetCommitted(h Hash)
    GetCommitted() (Hash, bool)
}
```

**Current**: `pkg/storage/blockstore.go` (in-memory)
**Future**: Persistent storage (Pebble/Badger)

## Configuration

### Quorum Calculation
```go
// N validators, need 2f+1 where N = 3f+1
n := len(validators)
t := (n-1) / 3
need := 2*t + 1

// Examples:
// N=1: t=0, need=1 (single-node dev)
// N=4: t=1, need=3
// N=7: t=2, need=5
```

### Timers
```go
Ppc: 150ms    // Case-2 wait time
Delta: 50ms   // Network propagation bound
MinBlockTime: 100ms  // Throttle block production (dev only)
```

## Performance Characteristics

**Latency**:
- Best case (Case-1): 1 RTT (propose → vote → prepare)
- Worst case (Case-2): Ppc + Δ + 1 RTT
- Typical: ~200ms per commit (2-chain means 2 views)

**Throughput**:
- Limited by: block size, execution time, network bandwidth
- NOT limited by: consensus protocol overhead (pipelined)

**Scalability**:
- O(N²) communication (all-to-all broadcast)
- Mitigated by: libp2p gossip, signature aggregation
- Typical: 4-100 validators

## Key Differences from Paper

1. **AppHash in Certificate**: Added for state verification (Byzantine detection)
2. **Reactive Followers**: Channel-based signaling instead of timer loops
3. **Vote Unicast**: Standard HotStuff (paper shows broadcast for simplicity)
4. **MinBlockTime**: Dev throttle to prevent empty block spam

## Debugging

### Common Issues

**"committed_view ≈ 2× height"**
- Expected: ~50% of blocks don't commit
- Reason: Chain breaks (Parent != prevCert.H)
- Enable verbose logging: `VERBOSE=true` to see skipped commits

**"No commits happening"**
- Check: BlockStore is set (`engine.Store != nil`)
- Check: Certificates being saved
- Check: Parent links are correct

**"Byzantine AppHash mismatch"**
- Indicates: Validators computed different state from same transactions
- Cause: Non-deterministic execution (time, random, map iteration)
- Fix: Ensure deterministic app logic

## Files

- `types.go` (121 lines): Core types, HashOfBlock, interfaces
- `state.go` (21 lines): State struct, genesis block
- `safety.go` (72 lines): Voting safety, locking rules
- `pacemaker.go` (86 lines): Timing, view advancement
- `engine.go` (324 lines): Main consensus loop, leader/follower logic
- `leader.go` (43 lines): Block proposal, leader election
- `messages.go` (7 lines): Propose message type

**Total**: ~674 lines
