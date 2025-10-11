# Hash Architecture: Complete Summary & Roadmap

## What We Debated & Decided

This document summarizes all discussions about hash structures, state roots, and block commitments in HyperLicked.

---

## Part 1: The Core Questions

### Q1: Should BlockHash include AppHash?
**Answer: NO** ❌

**Decided:** Separate consensus hash from application state hash (Tendermint/HotStuff model)

**Reason:**
- Blocks are proposed BEFORE execution
- AppHash is unknown at proposal time
- Validators execute and vote with AppHash separately
- Allows pipelined execution

**Status:** ✅ Implemented

---

### Q2: Should AppHash be in Vote/Certificate?
**Answer: YES** ✅

**Decided:** Votes and Certificates MUST include AppHash

**Reason:**
- Cryptographically bind state to consensus
- Detect Byzantine validators with divergent state
- Enable light client state verification
- Match HotStuff/Tendermint/Hyperliquid architecture

**Status:** ✅ Implemented (2025-10-11)

---

### Q3: Should we have separate PayloadHash?
**Answer: ADD SOON** ⚠️

**Decided:** Add PayloadHash field, use hash instead of raw bytes in BlockHash

**Reason:**
- Compact headers for efficient header-first sync
- Light client support (verify headers without full payload)
- Foundation for Merkle proofs
- Standard in all production chains

**Status:** 🔜 Recommended to add now

---

### Q4: Do we need Parent hash in Block?
**Answer: YES** ✅

**Decided:** Block.Parent = Hash of parent block (consensus hash)

**Reason:**
- Links blocks into chain structure
- Detects forks and reorgs
- Standard in all blockchains
- Already implemented correctly

**Status:** ✅ Implemented (already there)

---

### Q5: Do we need parent's StateRoot (like Ethereum)?
**Answer: NO** ❌

**Decided:** Don't include parent's AppHash in Block header

**Reason:**
- Ethereum does this for PoW mining (know state before proposing)
- We use BFT with fast finality (no need)
- Would require optimistic execution (complex)
- Tendermint/HotStuff don't do this either

**Status:** ✅ Not needed

---

## Part 2: Current Architecture

### Block Structure
```go
type Block struct {
    Height   Height      // Block number
    View     View        // Consensus round
    Parent   Hash        // Parent's BlockHash (consensus hash)
    AppHash  Hash        // Application state hash (after execution)
    Payload  []byte      // Raw transaction bytes
    Proposer NodeID      // Who proposed this block
    Time     time.Time   // Block timestamp
}
```

### Three Hashes We Have

#### 1. BlockHash (Consensus Hash)
```go
BlockHash = SHA256(
    Height       // 8 bytes
    View         // 8 bytes
    Parent       // 32 bytes - parent's BlockHash
    Payload      // Variable - raw transaction bytes
    Proposer     // Variable - validator ID
    Time         // 8 bytes - timestamp
)
```

**Used for:**
- Voting (validators vote on BlockHash)
- Certificates (QC commits to BlockHash)
- Chain structure (Parent field points to BlockHash)

**Does NOT include:**
- ❌ AppHash (state commitment is separate)
- ❌ PayloadHash (raw bytes included directly)

#### 2. AppHash (Application State Hash)
```go
AppHash = SHA256(
    Height       // 8 bytes - ensures uniqueness per block
    Timestamp    // 8 bytes - additional entropy
    OrderbookState // Deterministic hash of all orderbook levels
)
```

**Used for:**
- Voting (validators vote on AppHash)
- Certificates (proves 2f+1 validators agree on state)
- State verification (light clients, fraud proofs)

**Will extend to include:**
- [ ] Account balances
- [ ] Open positions
- [ ] Funding rates
- [ ] Oracle prices
- [ ] Validator stakes
- [ ] Bridge state

#### 3. Parent Hash (Chain Link)
```
Block.Parent = BlockHash of previous block
```

**Used for:**
- Chain structure (links blocks)
- 2-chain commit rule (parent-child relationship)
- Fork detection

**Points to:**
- Parent's CONSENSUS hash (BlockHash)
- NOT parent's state hash (AppHash)

---

## Part 3: What We DON'T Have (and why)

### ⚠️ PayloadHash (Transaction Root) - Should Add

**What it would be:**
```go
PayloadHash = Hash(Payload)  // Or MerkleRoot(transactions)
```

**Why we should add it:**
- Standard in all production chains
- Enables light clients
- Compact headers for efficient sync
- Foundation for Merkle proofs

**When to add:** Now or very soon

---

### ❌ Parent's AppHash in Block

**What Ethereum does:**
```go
Block N header includes:
  - parentHash (Block N-1's consensus hash)
  - stateRoot (State AFTER Block N-1 execution) ← Ethereum includes this
```

**Why we don't do this:**
- Ethereum needs this for PoW (miners know state before mining)
- BFT doesn't need it (validators execute after voting)
- Adds complexity (optimistic execution)
- Tendermint/HotStuff don't do this

---

### ❌ ReceiptsRoot (Execution Results)

**What Ethereum has:**
```go
ReceiptsRoot = MerkleRoot(all transaction receipts)
```

**Why we don't have it:**
- No EVM (no complex receipts)
- Order execution results are simple (fills)
- Can add later if needed for EVM surface

---

## Part 4: Comparison with Other Chains

| Hash Type | HyperLicked | Ethereum | Tendermint | Flow | Purpose |
|-----------|-------------|----------|------------|------|---------|
| **BlockHash** | ✅ Yes (incl. raw Payload) | ✅ Yes (Header only) | ✅ Yes (Header only) | ✅ Yes (Header only) | Chain structure |
| **AppHash** | ✅ Yes (in Vote/Cert) | ✅ Yes (stateRoot) | ✅ Yes (separate) | ✅ Yes (StateCommit) | State commitment |
| **PayloadHash** | ❌ No (raw bytes in hash) | ✅ Yes (txRoot) | ✅ Yes (DataHash) | ✅ Yes (PayloadHash) | Tx commitment |
| **ReceiptsRoot** | ❌ No | ✅ Yes | ❌ No | ❌ No | Execution results |
| **Parent Hash** | ✅ Yes (consensus hash) | ✅ Yes (prev blockHash) | ✅ Yes (LastBlockID) | ✅ Yes (ParentID) | Chain link |
| **Parent State** | ❌ No | ✅ Yes (prev stateRoot) | ❌ No | ❌ No | Mining optimization |

### Key Insight
Our architecture matches **Tendermint/Flow** (BFT) more than **Ethereum** (PoW/PoS).

---

## Part 5: Evolution Path & Recommendations

### Current Implementation ✅ DONE

**What we have:**
```
Block:
  - Parent: Hash (consensus hash of parent)
  - AppHash: Hash (state after execution) ✅
  - Payload: []byte (raw transactions)

BlockHash = Hash(height, view, parent, payload, proposer, time)
Vote includes: (BlockHash, AppHash) ✅
Certificate includes: (BlockHash, AppHash) ✅
```

**Status:** AppHash verification working, Byzantine detection ready

---

### Next Step: Add PayloadHash ⚠️ RECOMMENDED NOW

**Add PayloadHash:**
```go
type Block struct {
    Height      Height
    View        View
    Parent      Hash
    PayloadHash Hash      // ← NEW: Hash(Payload)
    AppHash     Hash
    Payload     []byte
    Proposer    NodeID
    Time        time.Time
}

BlockHash = Hash(height, view, parent, payloadHash, proposer, time)
                                       ↑ 32 bytes instead of raw MB
```

**Benefits:**
- Fixed-size headers (~200 bytes)
- Light client support
- Faster header propagation
- Foundation for Merkle proofs
- Standard in all production chains

**Effort:** ~200 lines of code, 2-3 hours

---

### Later: Upgrade to Merkle Roots 🎯 FOR ADVANCED FEATURES

**Upgrade to Merkle roots:**
```go
PayloadHash = MerkleRoot(transactions)  // Not just Hash(Payload)
AppHash = MerkleRoot(stateTries)        // Not just Hash(state)
```

**Benefits:**
- Merkle inclusion/exclusion proofs
- Light clients can verify "tx X in block Y"
- State proofs for specific accounts
- Fraud proofs for rollups/bridges

**Libraries:**
- `cosmos/iavl` - IAVL+ tree (Cosmos SDK uses)
- Simple Merkle tree implementation

**Effort:** ~1000 lines, 1-2 weeks
**When needed:** Light clients requiring state proofs, cross-chain bridges

---

### Optional Advanced Features 🔮 AS NEEDED

**Consider adding:**

1. **ReceiptsRoot** (if EVM added)
   - Merkle root of execution results
   - Proves "tx X succeeded with result Y"

2. **Parent state** (if needed for bridges)
   - Include parent's AppHash in block
   - Enables atomic cross-chain proofs

3. **Validator signatures** (for light clients)
   - Include validator set and signatures
   - Light clients verify without full node

---

## Part 6: Decision Matrix

### When to Add PayloadHash?

| Condition | Add PayloadHash? | Reason |
|-----------|-----------------|--------|
| Building production blockchain | ✅ Yes | Industry standard |
| Multi-validator network | ✅ Yes | Header sync benefits |
| Light client support | ✅ Yes | Required for compact headers |
| Any deployment | ✅ Recommended | Simple addition, major benefits |
| Cross-chain bridges | ✅ Yes | Merkle proofs needed |

### When to Add Merkle Trees?

| Condition | Add Merkle? | Reason |
|-----------|------------|--------|
| Basic blockchain | ❌ No | Simple hash sufficient initially |
| Light clients needing proofs | ✅ Yes | State proofs required |
| Bridges/rollups | ✅ Yes | Fraud proofs required |
| Large-scale deployment | ✅ Yes | Scalability and efficiency |

---

## Part 7: Recommended Architecture

### Current Implementation ✅ WORKING

```
Block Structure:
  Height, View, Parent, AppHash, Payload, Proposer, Time

Hashes:
  1. BlockHash = Hash(Height || View || Parent || Payload || Proposer || Time)
  2. AppHash = Hash(Height || Timestamp || AppState)
  3. Parent = BlockHash of previous block

Commitments:
  - Vote includes (BlockHash, AppHash) ✅
  - Certificate includes (BlockHash, AppHash) ✅
```

**Status:** ✅ AppHash verification working, Byzantine detection ready

---

### Next: Add PayloadHash ⚠️ RECOMMENDED

```
Block Structure:
  Height, View, Parent, PayloadHash, AppHash, Payload, Proposer, Time
                        ↑ ADD THIS

Hashes:
  1. BlockHash = Hash(Height || View || Parent || PayloadHash || Proposer || Time)
  2. PayloadHash = Hash(Payload)  // Simple hash
  3. AppHash = Hash(Height || Timestamp || AppState)
  4. Parent = BlockHash of previous block

Commitments:
  - Vote includes (BlockHash, AppHash)
  - Certificate includes (BlockHash, AppHash)
```

**When:** Soon - standard feature in production chains
**Effort:** 2-3 hours
**Benefit:** Compact headers, light client support, industry standard

---

### Later: Merkle Roots 🎯 FOR ADVANCED FEATURES

```
Block Structure:
  Height, View, Parent, PayloadHash, AppHash, Payload, Proposer, Time

Hashes:
  1. BlockHash = Hash(Height || View || Parent || PayloadHash || Proposer || Time)
  2. PayloadHash = MerkleRoot(transactions)  // ← Upgraded to Merkle
  3. AppHash = MerkleRoot(stateTries)        // ← Upgraded to Merkle
  4. Parent = BlockHash of previous block

Commitments:
  - Vote includes (BlockHash, AppHash)
  - Certificate includes (BlockHash, AppHash)

New capabilities:
  - Merkle proofs for tx inclusion
  - State proofs for account balances
  - Fraud proofs for bridges
  - Light client state verification
```

**When:** When building bridges or advanced light clients
**Effort:** 1-2 weeks
**Benefit:** Advanced cryptographic proofs

---

## Part 8: Summary of All Debates

### Debate 1: AppHash in BlockHash?
**Conclusion:** NO - Keep separate (Tendermint model)
**Reason:** Blocks proposed before execution
**Status:** ✅ Implemented correctly

### Debate 2: AppHash in Votes/Certificates?
**Conclusion:** YES - Must include for state verification
**Reason:** Detect Byzantine validators, enable light clients
**Status:** ✅ Implemented (2025-10-11)

### Debate 3: PayloadHash needed?
**Conclusion:** YES, add soon
**Reason:** Standard in all production chains, enables light clients
**Status:** ⚠️ Recommended to add

### Debate 4: Parent hash needed?
**Conclusion:** YES - Already have it
**Reason:** Chain structure, standard in all blockchains
**Status:** ✅ Implemented (already there)

### Debate 5: Parent's StateRoot?
**Conclusion:** NO - Don't need Ethereum's pattern
**Reason:** BFT doesn't need optimistic execution
**Status:** ✅ Correctly NOT implemented

---

## Part 9: Action Items & Timeline

### ✅ COMPLETED
- [x] Separate BlockHash and AppHash
- [x] Add AppHash to Vote struct
- [x] Add AppHash to Certificate struct
- [x] Validators execute before voting
- [x] Leader verifies AppHash agreement
- [x] Byzantine detection working
- [x] Tests passing
- [x] Production-ready consensus with state verification

### ⏳ RECOMMENDED NEXT
- [ ] Add PayloadHash field to Block
- [ ] Update HashOfBlock to use PayloadHash instead of raw Payload
- [ ] Compute PayloadHash when creating blocks
- [ ] Update tests
- **Effort:** 2-3 hours
- **Benefit:** Compact headers, light client support, industry standard

### 🔮 OPTIONAL (for advanced features)
- [ ] Upgrade PayloadHash to MerkleRoot
- [ ] Upgrade AppHash to IAVL/Merkle tree
- [ ] Implement Merkle proof generation
- [ ] Add light client verification with proofs
- **Effort:** 1-2 weeks
- **When needed:** Bridges, advanced light clients requiring proofs

---

## Part 10: Quick Reference

### What We Have Now ✅
```
2 hashes + 1 link:
  - BlockHash (consensus)
  - AppHash (state)
  - Parent (chain link)

BlockHash does NOT include AppHash ✅
AppHash IS in Vote/Certificate ✅
Payload bytes directly in BlockHash ✅
Parent points to consensus hash ✅
```

### What to Add Next ⚠️
```
Soon:
  + PayloadHash (hash of transactions) - 2-3 hours

Later (optional):
  + Merkle trees (for advanced proofs) - 1-2 weeks
  + IAVL state tree (like Cosmos) - when needed
```

### What We'll NEVER Add ❌
```
- Parent's AppHash in block (Ethereum pattern)
- ReceiptsRoot (unless EVM added)
- Uncle blocks (BFT has finality)
```

---

## Conclusion

**Current architecture is SOLID:**
- ✅ Matches Tendermint/HotStuff model
- ✅ AppHash verification working
- ✅ Byzantine detection working
- ✅ Production-ready consensus

**Recommended next step:**
1. **Add PayloadHash** (2-3 hours) - Industry standard, enables light clients
2. **Merkle trees** (optional) - Only when you need advanced cryptographic proofs
3. **Other features** - As needed for your specific use cases

**You're building it right!** 🎉

The foundation is solid. PayloadHash is the natural next evolution for a production blockchain.
