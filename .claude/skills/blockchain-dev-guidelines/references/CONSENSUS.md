# HotStuff-2 Consensus Implementation

Deep dive into the HotStuff-2 BFT consensus implementation.

## Table of Contents

- [Overview](#overview)
- [Block Structure](#block-structure)
- [Vote & Certificate](#vote--certificate)
- [Engine Tick Pattern](#engine-tick-pattern)
- [Leader/Follower Logic](#leaderfollower-logic)
- [2-Chain Commit Rule](#2-chain-commit-rule)

---

## Overview

HotStuff-2 is a Byzantine Fault Tolerant (BFT) consensus protocol that provides:
- **Safety**: No two honest validators commit different blocks at the same height
- **Liveness**: Progress continues if ≥2f+1 validators are honest
- **2-Chain Commit**: Simpler than HotStuff (3-chain), one round less latency

### Key Components

```
src/consensus/
├── mod.rs          # Traits: AppHook, BlockStore
├── engine.rs       # Main consensus loop
├── pacemaker.rs    # View advancement
├── safety.rs       # Voting rules
├── aggregator.rs   # BLS signature aggregation
└── runner.rs       # Async multi-node orchestration
```

---

## Block Structure

```rust
pub struct Block {
    pub view: View,           // Consensus round number
    pub height: Height,       // Block number (0 = genesis)
    pub parent: Hash,         // SHA-256 hash of parent block
    pub payload: Vec<u8>,     // Serialized transactions
    pub proposer: NodeId,     // Leader who proposed this block
    pub app_hash: Hash,       // State root AFTER executing this block
    pub timestamp: u64,       // Unix timestamp
}
```

### Hash Computation

**CRITICAL**: BlockHash does NOT include app_hash!

```rust
impl Block {
    pub fn hash(&self) -> Hash {
        let mut hasher = Sha256::new();
        hasher.update(self.view.to_le_bytes());
        hasher.update(self.height.to_le_bytes());
        hasher.update(self.parent);
        hasher.update(&self.payload);
        hasher.update(self.proposer);
        hasher.update(self.app_hash);
        hasher.update(self.timestamp.to_le_bytes());
        hasher.finalize().into()
    }
}
```

**Why?** Execution happens AFTER proposal. The proposer:
1. Creates block with empty app_hash
2. Executes transactions to get app_hash
3. Fills in app_hash
4. Broadcasts proposal

### Genesis Block

```rust
impl Block {
    pub fn genesis() -> Self {
        Self {
            view: 0,
            height: 0,
            parent: [0u8; 32],
            payload: vec![],
            proposer: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 0,
        }
    }
}
```

---

## Vote & Certificate

### Vote Structure

```rust
pub struct Vote {
    pub view: View,
    pub block_hash: Hash,
    pub app_hash: Hash,              // For Byzantine detection
    pub voter: NodeId,
    pub signature: Signature,
    pub bls_pubkey: Option<Vec<u8>>, // Optional BLS key (48 bytes)
}
```

**app_hash in Vote**: Enables Byzantine detection. If validators execute differently, their app_hash will differ.

### Signing Data

```rust
impl Vote {
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.view.to_le_bytes());
        data.extend_from_slice(&self.block_hash);
        data.extend_from_slice(&self.app_hash);
        data.extend_from_slice(&self.voter);
        data
    }
}
```

### Certificate (QC)

```rust
pub struct Certificate {
    pub view: View,
    pub block_hash: Hash,
    pub votes: Vec<Vote>,               // Individual votes (legacy)
    pub voters: Vec<NodeId>,            // Voter IDs
    pub bls_pubkeys: Vec<Vec<u8>>,     // BLS public keys
    pub agg_signature: Vec<u8>,         // Aggregated BLS signature
    pub app_hash: Option<Hash>,         // For BLS verification
}
```

**Two modes:**
- **Legacy**: Stores individual votes, concatenates signatures
- **BLS**: Stores aggregated signature (96 bytes total), requires app_hash for verification

**BLS Verification**: All voters sign the same message `(view, block_hash, app_hash)` (without voter ID). This enables efficient aggregate verification via a single pairing check.

---

## Engine Tick Pattern

```rust
pub struct Engine<A, S>
where
    A: AppHook,
    S: BlockStore,
{
    config: ConsensusConfig,
    safety: Safety,
    pacemaker: Pacemaker,
    app: A,
    store: S,
    pending: HashMap<Hash, Block>,
    committed_height: u64,
}

impl<A, S> Engine<A, S> {
    pub fn tick(&mut self) -> Option<Block> {
        let view = self.pacemaker.current_view();

        if self.config.is_leader(view) {
            self.run_leader(view)
        } else {
            self.run_follower(view)
        }
    }
}
```

**tick() must return quickly** - no blocking I/O!

---

## Leader/Follower Logic

### Leader Flow

```rust
fn run_leader(&mut self, view: View) -> Option<Block> {
    // 1. Get parent from high_qc
    let parent = self.get_proposal_parent();

    // 2. Prepare payload from app
    let payload = self.app.prepare_payload(&parent);

    // 3. Create block
    let mut block = Block {
        view,
        height: parent.height + 1,
        parent: parent.hash(),
        payload,
        proposer: self.config.my_id(),
        app_hash: [0u8; 32],  // Fill after execution
        timestamp: current_time(),
    };

    // 4. Execute to get app_hash
    let app_hash = self.app.execute(&block);
    block.app_hash = app_hash;

    // 5. Store and self-vote
    self.store.save(&block);
    let vote = Vote::new_bls(view, block.hash(), app_hash, ...);
    let qc = Certificate::new(view, block.hash(), vec![vote]);

    // 6. Process QC (may commit previous block)
    let committed = self.process_qc(qc);

    // 7. Advance view
    if let Some(ref qc) = self.safety.high_qc() {
        self.pacemaker.advance_view(qc);
    }

    committed
}
```

### Follower Flow

```rust
fn run_follower(&mut self, view: View) -> Option<Block> {
    // 1. Wait for proposal (non-blocking check)
    let proposal = self.pending.get(&view)?;

    // 2. Validate proposal
    if !self.safety.can_vote(proposal) {
        return None;
    }

    // 3. Execute to get our app_hash
    let app_hash = self.app.execute(proposal);

    // 4. Create and send vote
    let vote = Vote::new_bls(view, proposal.hash(), app_hash, ...);
    self.send_vote(vote);

    // 5. Advance view
    self.pacemaker.advance_view_follower();

    None  // Followers don't return committed blocks directly
}
```

---

## 2-Chain Commit Rule

### How It Works

```
View N:     Block A proposed, gets QC
View N+1:   Block B proposed (extends A), gets QC
            → Block A is COMMITTED (2-chain)
```

**Commit happens when:**
1. Block at view N has a QC
2. Block at view N+1 extends N and has a QC
3. N+1's QC commits block N

### Implementation

```rust
fn process_qc(&mut self, qc: Certificate) -> Option<Block> {
    // Get the block this QC certifies
    let certified_block = self.store.get(&qc.block_hash)?;

    // Get parent (the block to potentially commit)
    let parent = self.store.get(&certified_block.parent)?;

    // Check 2-chain: if parent has a QC and this extends it
    if self.has_qc_for(&parent) && !self.is_committed(&parent) {
        self.store.set_committed(&parent.hash());
        self.committed_height = parent.height;
        return Some(parent);
    }

    None
}
```

---

## Safety Rules

```rust
pub struct Safety {
    last_voted_view: View,
    locked_qc: Option<Certificate>,
    high_qc: Option<Certificate>,
    voted_views: HashSet<View>,  // PERSISTED - prevents double-voting after crash
}

impl Safety {
    pub fn can_vote(&self, block: &Block) -> bool {
        // 1. Haven't voted in this view
        if block.view <= self.last_voted_view {
            return false;
        }

        // 2. Block extends locked_qc or is in higher view
        if let Some(ref locked) = self.locked_qc {
            if block.parent != locked.block_hash
               && block.view <= locked.view {
                return false;
            }
        }

        true
    }
}
```

---

## Security Patterns

### Vote Rate Limiting

Prevents DoS via vote spam:

```rust
// In aggregator.rs
pub struct VoteRateLimiter {
    windows: HashMap<NodeId, VecDeque<Instant>>,
    max_per_second: usize,  // Default: 10
}

impl VoteAggregator {
    pub fn add_vote(&mut self, vote: Vote) -> Option<Certificate> {
        // Check rate limit BEFORE processing
        if let Err(e) = self.rate_limiter.check_and_record(&vote.voter) {
            tracing::warn!("Rate limited vote from {}", hash_short(&vote.voter));
            return None;
        }
        // ... process vote
    }
}
```

### Safety Persistence

**CRITICAL**: voted_views MUST be persisted to prevent double-voting after crash:

```rust
// In runner.rs - after every vote
fn on_vote(&mut self, vote: Vote) {
    self.safety.record_vote(vote.view);

    // Persist immediately - panic on failure to prevent double-vote
    if let Err(e) = self.persist_consensus_state() {
        panic!("CRITICAL: Failed to persist voted_views: {}", e);
    }
}
```

### Network Authentication

TCP connections require BLS-authenticated handshakes in production:

```rust
// In network/mod.rs
pub struct NetworkConfig {
    pub require_authenticated_peers: bool,  // false in dev, true otherwise
    pub bls_secret_key: Option<BlsSecretKey>,
    pub validator_pubkeys: HashMap<NodeId, BlsPublicKey>,
}

// In transport.rs - reject unauthenticated connections
if handshake_config.require_auth && !handshake_result.authenticated {
    warn!("Rejecting unauthenticated peer");
    return;
}
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [TYPES.md](TYPES.md) - Core type definitions
- [CRYPTO.md](CRYPTO.md) - BLS signatures
