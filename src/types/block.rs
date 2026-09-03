//! Block Type
//!
//! A block in the chain.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    Certificate, ConsensusContext, Hash, Height, NodeId, View, BLOCK_HASH_VERSION,
    MAX_BLOCK_PAYLOAD_SIZE,
};

/// Maximum amount a live proposal timestamp may lead a validator's wall
/// clock. Historical replay only needs parent monotonicity; this bound is
/// enforced by live consensus before voting.
pub const MAX_BLOCK_FUTURE_DRIFT_MS: u64 = 30_000;

/// Maximum amount a live height-one proposal timestamp may lag a validator's
/// wall clock. Historical replay only needs parent monotonicity; this anchor
/// bound is enforced by live consensus before voting.
pub const MAX_BLOCK_PAST_DRIFT_MS: u64 = 30_000;

/// Maximum timestamp advance over the exact parent after height one.
///
/// Height one anchors the deterministic genesis timestamp (`0`) to wall
/// time. Later blocks may not turn a single proposal into an arbitrarily
/// large application-time/reward jump.
pub const MAX_BLOCK_TIMESTAMP_STEP_MS: u64 = 30_000;

/// A block in the chain.
///
/// Blocks form a chain via `parent` hash. Each block has:
/// - `view`: The consensus round it was proposed in
/// - `height`: Position in committed chain (0 = genesis)
/// - `payload`: Serialized transactions
/// - `app_hash`: State root after executing this block
/// - `commitment_root`: Combined Commitment v2 receipt/event root
/// - `justify`: QC that justifies this block (proves parent was certified)
///
/// The `justify` field is NOT included in the block hash (it's metadata
/// about how we received the block, not part of the block identity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    /// Consensus epoch that authenticated this block.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    pub view: View,
    pub height: Height,
    pub parent: Hash,
    pub payload: Vec<u8>,
    pub proposer: NodeId,
    /// Combined Commitment v2 receipts/events root for this block.
    ///
    /// This field is included in `hash()`, so proposer signatures, votes, and
    /// QCs authenticate the execution artifact without folding it into the
    /// schema-v5 state root in `app_hash`.
    pub commitment_root: Hash,
    pub app_hash: Hash,
    pub timestamp: u64,
    /// QC that justifies this block (proves parent was certified).
    /// Not included in block hash - it's consensus metadata.
    #[serde(default)]
    pub justify: Option<Certificate>,
}

impl Block {
    /// Compute the hash of this block
    pub fn hash(&self) -> Hash {
        let mut hasher = Sha256::new();
        hasher.update(b"HYPERLICKED_BLOCK_V5_STATE_ROOT_COMMITMENT");
        hasher.update(BLOCK_HASH_VERSION.to_le_bytes());
        hasher.update(self.epoch.to_le_bytes());
        hasher.update(self.committee_hash);
        hasher.update(self.genesis_hash);
        hasher.update(self.view.to_le_bytes());
        hasher.update(self.height.to_le_bytes());
        hasher.update(self.parent);
        hasher.update(&self.payload);
        hasher.update(self.proposer);
        hasher.update(self.commitment_root);
        hasher.update(self.app_hash);
        hasher.update(self.timestamp.to_le_bytes());
        hasher.finalize().into()
    }

    /// Create genesis block (height 0, no parent)
    pub fn genesis(context: ConsensusContext) -> Self {
        Self {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 0,
            height: 0,
            parent: [0u8; 32],
            payload: vec![],
            proposer: [0u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 0,
            justify: None,
        }
    }

    /// Return the authentication context carried by this block.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this block belongs to the expected consensus context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err(format!(
                "block context mismatch: expected epoch {} / committee {} / genesis {}, got epoch {} / committee {} / genesis {}",
                expected.epoch,
                hex::encode(expected.committee_hash),
                hex::encode(expected.genesis_hash),
                self.epoch,
                hex::encode(self.committee_hash),
                hex::encode(self.genesis_hash),
            ));
        }
        Ok(())
    }

    /// Validate block structure (DoS protection)
    ///
    /// Returns error if block violates size limits.
    /// Call this before voting on or executing blocks from other validators.
    pub fn validate(&self) -> Result<(), String> {
        if self.payload.len() > MAX_BLOCK_PAYLOAD_SIZE {
            return Err(format!(
                "Block payload too large: {} bytes > {} bytes max",
                self.payload.len(),
                MAX_BLOCK_PAYLOAD_SIZE
            ));
        }
        if self.height > 0 && self.commitment_root == [0u8; 32] {
            return Err("non-genesis block is missing its execution commitment root".to_string());
        }
        Ok(())
    }

    /// Validate the consensus timestamp against the exact parent using only
    /// deterministic block data. Height one establishes the wall-time anchor;
    /// every later block is additionally bounded by the parent-relative step.
    pub fn validate_parent_timestamp(&self, parent_timestamp: u64) -> Result<(), String> {
        if self.timestamp < parent_timestamp {
            return Err(format!(
                "block timestamp {} precedes parent timestamp {}",
                self.timestamp, parent_timestamp
            ));
        }
        if self.height > 1 {
            let latest = parent_timestamp.saturating_add(MAX_BLOCK_TIMESTAMP_STEP_MS);
            if self.timestamp > latest {
                return Err(format!(
                    "block timestamp {} exceeds parent-relative bound {}",
                    self.timestamp, latest
                ));
            }
        }
        Ok(())
    }

    /// Apply the deterministic parent rule plus live wall-clock bounds.
    /// Only height one is past-bounded: later blocks must remain resumable
    /// after a long outage. Historical replay uses
    /// [`Self::validate_parent_timestamp`] so a valid persisted chain never
    /// depends on the recovering node's wall clock.
    pub fn validate_live_timestamp(
        &self,
        parent_timestamp: u64,
        now_ms: u64,
    ) -> Result<(), String> {
        self.validate_parent_timestamp(parent_timestamp)?;
        let latest = now_ms.saturating_add(MAX_BLOCK_FUTURE_DRIFT_MS);
        if self.timestamp > latest {
            return Err(format!(
                "block timestamp {} exceeds live future bound {}",
                self.timestamp, latest
            ));
        }
        if self.height == 1 {
            let earliest = now_ms.saturating_sub(MAX_BLOCK_PAST_DRIFT_MS);
            if self.timestamp < earliest {
                return Err(format!(
                    "block timestamp {} precedes live past bound {}",
                    self.timestamp, earliest
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ConsensusContext {
        ConsensusContext::new(0, [7u8; 32])
    }

    #[test]
    fn genesis_carries_context() {
        let block = Block::genesis(context());

        assert_eq!(block.context(), context());
        assert_eq!(block.height, 0);
        assert_eq!(block.view, 0);
    }

    #[test]
    fn block_hash_binds_epoch_committee_and_genesis_domain() {
        let block = Block::genesis(context());

        let mut different_epoch = block.clone();
        different_epoch.epoch = 1;
        assert_ne!(block.hash(), different_epoch.hash());

        let mut different_committee = block.clone();
        different_committee.committee_hash[0] ^= 1;
        assert_ne!(block.hash(), different_committee.hash());

        let mut different_genesis = block;
        different_genesis.genesis_hash[0] ^= 1;
        assert_ne!(different_genesis.hash(), Block::genesis(context()).hash());
    }

    #[test]
    fn block_hash_binds_the_authenticated_state_root() {
        let block = Block {
            height: 1,
            parent: [3u8; 32],
            app_hash: [4u8; 32],
            ..Block::genesis(context())
        };
        let mut changed = block.clone();
        changed.app_hash[0] ^= 1;
        assert_ne!(block.hash(), changed.hash());
    }

    #[test]
    fn block_hash_binds_the_execution_commitment_root() {
        let block = Block {
            height: 1,
            parent: [3u8; 32],
            commitment_root: [4u8; 32],
            ..Block::genesis(context())
        };
        let mut changed = block.clone();
        changed.commitment_root[0] ^= 1;
        assert_ne!(block.hash(), changed.hash());
    }

    #[test]
    fn non_genesis_block_requires_an_execution_commitment_root() {
        let block = Block {
            height: 1,
            parent: [3u8; 32],
            ..Block::genesis(context())
        };

        assert!(block.validate().is_err());
    }

    #[test]
    fn block_from_another_genesis_is_rejected() {
        let local = ConsensusContext::with_genesis(0, [7u8; 32], [1u8; 32]);
        let remote = ConsensusContext::with_genesis(0, [7u8; 32], [2u8; 32]);
        let block = Block::genesis(remote);

        assert!(block.validate_context(local).is_err());
    }

    #[test]
    fn live_timestamp_is_parent_monotonic_and_future_bounded() {
        let mut block = Block::genesis(context());
        block.timestamp = 100;
        assert!(block.validate_live_timestamp(100, 100).is_ok());

        block.timestamp = 99;
        assert!(block.validate_live_timestamp(100, 100).is_err());

        block.timestamp = 100 + MAX_BLOCK_FUTURE_DRIFT_MS;
        assert!(block.validate_live_timestamp(100, 100).is_ok());
        block.timestamp += 1;
        assert!(block.validate_live_timestamp(100, 100).is_err());
    }

    #[test]
    fn live_timestamp_rejects_stale_height_one_but_allows_old_parent_after_downtime() {
        let now_ms = 100_000;
        let stale_timestamp = now_ms - MAX_BLOCK_PAST_DRIFT_MS - 1;

        let mut height_one = Block::genesis(context());
        height_one.height = 1;
        height_one.timestamp = stale_timestamp;
        assert!(height_one.validate_live_timestamp(0, now_ms).is_err());
        assert!(height_one.validate_parent_timestamp(0).is_ok());

        let mut later = height_one.clone();
        later.height = 2;
        later.timestamp = stale_timestamp;
        assert!(later
            .validate_live_timestamp(stale_timestamp, now_ms)
            .is_ok());
        assert!(later.validate_parent_timestamp(stale_timestamp).is_ok());
    }

    #[test]
    fn timestamp_step_is_deterministically_bounded_after_height_one() {
        let mut block = Block::genesis(context());
        block.height = 2;
        block.timestamp = 100 + MAX_BLOCK_TIMESTAMP_STEP_MS;
        assert!(block.validate_parent_timestamp(100).is_ok());

        block.timestamp += 1;
        assert!(block.validate_parent_timestamp(100).is_err());

        block.height = 1;
        assert!(block.validate_parent_timestamp(0).is_ok());
    }
}
