//! Persistent Storage
//!
//! RocksDB-based persistence for blocks, consensus state, and app snapshots.
//!
//! ## Architecture
//!
//! Uses a durable finalized-chain approach:
//! - Always persist: finalized blocks, consensus safety state (QCs, voted views)
//! - Persist proposal targets by hash before recording a vote
//! - Snapshots remain available for sync/operations, but canonical restart
//!   recovery begins with genesis and replays every finalized block because
//!   snapshots omit orderbook state

pub mod recovery;
pub mod rocks;
pub mod snapshot;
pub mod verify;

pub use recovery::{recover_from_storage, RecoveryResult};
pub use rocks::RocksDbStore;
pub use snapshot::AppSnapshot;
pub use verify::{
    compute_snapshot_hash, verify_block_chain, verify_block_chain_internal, verify_snapshot,
    verify_snapshot_height, ChainVerifyResult,
};

use serde::{Deserialize, Serialize};

use crate::app::candles::Candle;
use crate::consensus::BlockStore;
use crate::consensus::EpochTransitionProof;
use crate::types::{
    Block, Certificate, CommitmentV2, ConsensusContext, Hash, TransactionReceipt, View,
    MAX_RECEIPT_BYTES,
};

/// Schema carried by the durable full-state root record. Under block protocol
/// V4 this schema-v3 root is also the value authenticated as `Block::app_hash`.
pub const STATE_ROOT_SCHEMA_VERSION: u16 = crate::types::CONSENSUS_STATE_ROOT_SCHEMA_VERSION;
const STATE_ROOT_RECORD_BYTES: usize = 2 + 32;

/// Maximum number of durable actionable equivocation proofs retained per
/// store.  The journal is intentionally bounded until consensus wiring can
/// consume and delete records after block inclusion.
pub const MAX_EQUIVOCATION_PROOF_RECORDS: usize = 256;
/// Maximum combined key/value bytes occupied by the proof journal.
pub const MAX_EQUIVOCATION_PROOF_BYTES: usize = 1024 * 1024;

/// Maximum serialized bytes for one staged epoch-transition proof.
pub const MAX_EPOCH_TRANSITION_PROOF_BYTES: usize =
    crate::consensus::MAX_EPOCH_TRANSITION_PROOF_BYTES;

/// Read-only declaration of the durable equivocation journal operations a
/// store implements.  Recovery needs all three operations: a store that can
/// only load evidence cannot safely acknowledge live consensus startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EquivocationJournalCapability {
    pub load: bool,
    pub save: bool,
    pub delete: bool,
}

impl EquivocationJournalCapability {
    pub const fn unsupported() -> Self {
        Self {
            load: false,
            save: false,
            delete: false,
        }
    }

    pub const fn supported() -> Self {
        Self {
            load: true,
            save: true,
            delete: true,
        }
    }

    pub const fn supports_all(self) -> bool {
        self.load && self.save && self.delete
    }
}

/// Explicit on-disk representation of a schema-v3 full-state root.
///
/// The storage API still exposes the root hash to consensus/recovery callers,
/// but RocksDB never stores a bare 32-byte value. Keeping the schema beside
/// the hash makes a future root encoder fail closed instead of being silently
/// interpreted as the current schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateRootRecord {
    pub schema_version: u16,
    pub root: Hash,
}

impl StateRootRecord {
    pub const fn new(root: Hash) -> Self {
        Self {
            schema_version: STATE_ROOT_SCHEMA_VERSION,
            root,
        }
    }

    pub fn validate(&self) -> Result<(), StateRootRecordError> {
        if self.schema_version != STATE_ROOT_SCHEMA_VERSION {
            return Err(StateRootRecordError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        Ok(())
    }

    /// Return the exact fixed-width bytes persisted in the state-roots column.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, StateRootRecordError> {
        self.validate()?;
        let mut bytes = Vec::with_capacity(STATE_ROOT_RECORD_BYTES);
        bytes.extend_from_slice(&self.schema_version.to_le_bytes());
        bytes.extend_from_slice(&self.root);
        Ok(bytes)
    }

    /// Decode only the current fixed-width encoding. Raw legacy hashes,
    /// unsupported schemas, truncated values, and trailing bytes are errors.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, StateRootRecordError> {
        if bytes.len() == 32 {
            return Err(StateRootRecordError::LegacyRawRoot);
        }
        if bytes.len() != STATE_ROOT_RECORD_BYTES {
            return Err(StateRootRecordError::InvalidLength {
                expected: STATE_ROOT_RECORD_BYTES,
                actual: bytes.len(),
            });
        }

        let schema_version = u16::from_le_bytes([bytes[0], bytes[1]]);
        if schema_version != STATE_ROOT_SCHEMA_VERSION {
            return Err(StateRootRecordError::UnsupportedSchemaVersion(
                schema_version,
            ));
        }
        let mut root = [0u8; 32];
        root.copy_from_slice(&bytes[2..]);
        Ok(Self {
            schema_version,
            root,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateRootRecordError {
    #[error("unsupported full-state root schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("legacy raw 32-byte full-state root is not supported")]
    LegacyRawRoot,
    #[error("full-state root record has invalid length: expected {expected} bytes, got {actual}")]
    InvalidLength { expected: usize, actual: usize },
}

/// Schema version for the durable transaction receipt index rows.
pub const TRANSACTION_RECEIPT_INDEX_SCHEMA_VERSION: u16 = 1;
/// Bound one durable transaction receipt index row before decoding it.
pub const MAX_TRANSACTION_RECEIPT_INDEX_BYTES: usize = MAX_RECEIPT_BYTES + 128;

/// A signed-envelope receipt together with the finalized block location that
/// authenticated it. System-action receipts remain block-local and are not
/// inserted into the global transaction-ID index.
///
/// The transaction ID is repeated from [`TransactionReceipt::tx_id`] so API
/// callers do not need to reconstruct it from a nested value.  RocksDB rows
/// are only returned after the block's canonical Commitment v2 artifact and
/// header root have been checked against this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionReceiptLookup {
    pub tx_id: Hash,
    pub block_hash: Hash,
    pub block_height: u64,
    pub tx_index: u32,
    pub receipt: TransactionReceipt,
}

/// Consensus state that must survive crashes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusState {
    /// Consensus epoch that authenticated this persisted safety state.
    pub epoch: u64,
    /// Canonical committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    /// Highest QC seen (determines valid chain extension)
    pub high_qc: Option<Certificate>,
    /// Locked QC (for HotStuff-2 liveness)
    pub locked_qc: Option<Certificate>,
    /// Views we've voted in (safety - prevent double voting)
    pub voted_views: Vec<View>,
    /// Current view
    pub current_view: View,
    /// Last committed block height
    pub committed_height: u64,
    /// Last committed block hash
    pub committed_hash: Hash,
    /// Consecutive timeout count (for exponential backoff persistence)
    #[serde(default)]
    pub consecutive_timeouts: u32,
    /// View for which we've sent a ViewChange (prevent double-send after crash)
    #[serde(default)]
    pub vc_sent_for_view: Option<View>,
}

impl ConsensusState {
    /// Genesis consensus state (no prior history)
    pub fn genesis() -> Self {
        Self {
            epoch: 0,
            committee_hash: [0u8; 32],
            genesis_hash: [0u8; 32],
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: [0u8; 32],
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        }
    }

    /// Return the authentication context carried by this persisted state.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }
}

/// Extended BlockStore with persistence features
pub trait PersistentStore: BlockStore {
    /// Persist a block that may not be finalized yet.
    ///
    /// Proposal/vote safety state can refer to a block before its commit.  A
    /// separate operation keeps those blocks addressable by hash without
    /// replacing the canonical height index for an already-finalized block.
    fn save_block(&self, block: &Block) -> anyhow::Result<()> {
        self.save(block);
        Ok(())
    }

    /// Persist consensus safety state synchronously.
    ///
    /// Consensus uses this operation for vote intents: the candidate state
    /// containing the new `voted_views` entry is written before the live
    /// safety state is mutated or the vote is sent or counted. Implementations
    /// must not acknowledge until the state is durable enough for crash
    /// recovery to reject a second vote in the same view.
    fn save_consensus_state(&self, state: &ConsensusState) -> anyhow::Result<()>;

    /// Load consensus safety state
    fn load_consensus_state(&self) -> anyhow::Result<Option<ConsensusState>>;

    /// Report journal support without probing or mutating the store.  The
    /// default is intentionally unsupported so custom stores must opt in
    /// explicitly before they can back a live consensus runner.
    fn equivocation_journal_capability(&self) -> EquivocationJournalCapability {
        EquivocationJournalCapability::unsupported()
    }

    /// Persist one already-verified equivocation proof in the durable,
    /// bounded journal. Implementations must use first-write-wins semantics
    /// for the proof's authenticated context/offender key.
    fn save_equivocation_proof(
        &self,
        _proof: &crate::consensus::EquivocationProof,
    ) -> anyhow::Result<()> {
        anyhow::bail!("equivocation proof journal is not supported by this store")
    }

    /// Load and strictly validate every durable equivocation proof.
    fn load_equivocation_proofs(&self) -> anyhow::Result<Vec<crate::consensus::EquivocationProof>> {
        anyhow::bail!("equivocation proof journal is not supported by this store")
    }

    /// Delete the proof identified by its authenticated context/offender key.
    /// Deleting a missing row is idempotent.
    fn delete_equivocation_proof(
        &self,
        _proof: &crate::consensus::EquivocationProof,
    ) -> anyhow::Result<()> {
        anyhow::bail!("equivocation proof journal is not supported by this store")
    }

    /// Durably stage one fully verified epoch-transition proof.
    ///
    /// Staging is intentionally separate from activation. A live runner must
    /// not swap only its local committee; the marker is consumed by a future
    /// atomic app/consensus/network activation path. Stores that do not
    /// implement this boundary fail closed instead of silently dropping it.
    fn save_epoch_transition_proof(&self, _proof: &EpochTransitionProof) -> anyhow::Result<()> {
        anyhow::bail!("epoch-transition staging is not supported by this store")
    }

    /// Load a staged transition marker. `None` means no marker is present.
    fn load_epoch_transition_proof(&self) -> anyhow::Result<Option<EpochTransitionProof>> {
        Ok(None)
    }

    /// Clear a staged transition marker after a future atomic activation.
    fn clear_epoch_transition_proof(&self) -> anyhow::Result<()> {
        anyhow::bail!("epoch-transition staging is not supported by this store")
    }

    /// Load the separately indexed committed height metadata.
    fn load_committed_height(&self) -> anyhow::Result<Option<u64>> {
        Ok(self.get_committed_head().map(|block| block.height))
    }

    /// Save app state snapshot at height
    fn save_snapshot(&self, height: u64, snapshot: &AppSnapshot) -> anyhow::Result<()>;

    /// Load latest snapshot at or before given height
    fn load_latest_snapshot(
        &self,
        before_height: u64,
    ) -> anyhow::Result<Option<(u64, AppSnapshot)>>;

    /// Return only the latest snapshot height at or below the requested
    /// height. Metadata callers use this to avoid decoding the full snapshot.
    fn load_latest_snapshot_height(&self, before_height: u64) -> anyhow::Result<Option<u64>>;

    /// Get all committed blocks from height (inclusive) for replay
    fn blocks_from_height(&self, from_height: u64) -> anyhow::Result<Vec<Block>>;

    /// Atomic commit: block + consensus state together
    fn commit_block(&self, block: &Block, state: &ConsensusState) -> anyhow::Result<()>;

    /// Atomically commit a finalized block, consensus state, and optional
    /// execution artifacts.
    ///
    /// The byte payload is the canonical Commitment v2 encoding. Consensus-
    /// activated stores must decode it and verify its root against the block;
    /// `None` is valid only for genesis in the current protocol.
    /// Implementations that do not support artifact persistence must reject
    /// `Some` rather than acknowledging a commit that cannot be reindexed.
    fn commit_block_with_artifacts(
        &self,
        block: &Block,
        state: &ConsensusState,
        artifacts: Option<&[u8]>,
    ) -> anyhow::Result<()> {
        if artifacts.is_some() {
            anyhow::bail!("persistent store does not support block artifacts");
        }
        self.commit_block(block, state)
    }

    /// Atomically commit a finalized block with a validated Commitment v2
    /// artifact. The storage representation is the artifact's exact canonical
    /// bincode encoding, so an indexer can decode and verify the same bytes
    /// after restart.
    fn commit_block_with_commitment(
        &self,
        block: &Block,
        state: &ConsensusState,
        commitment: Option<&CommitmentV2>,
    ) -> anyhow::Result<()> {
        let bytes = commitment.map(CommitmentV2::canonical_bytes).transpose()?;
        self.commit_block_with_artifacts(block, state, bytes.as_deref())
    }

    /// Atomically commit a finalized block, consensus state, Commitment v2,
    /// and the full-state root. The root is also carried by `Block::app_hash`
    /// and is persisted separately as a schema-v3 [`StateRootRecord`] for
    /// restart validation. It remains distinct from the Commitment v2
    /// receipt/event artifact.
    ///
    /// Stores that have not opted into the state-root column must reject a
    /// non-`None` root rather than acknowledging a commit that cannot be
    /// checked after restart.
    fn commit_block_with_commitment_and_state_root(
        &self,
        block: &Block,
        state: &ConsensusState,
        commitment: Option<&CommitmentV2>,
        state_root: Option<&Hash>,
    ) -> anyhow::Result<()> {
        if state_root.is_some() {
            anyhow::bail!("persistent store does not support full-state roots");
        }
        self.commit_block_with_commitment(block, state, commitment)
    }

    /// Load the canonical execution artifact bytes for a block hash.
    ///
    /// `Ok(None)` is expected for genesis or an unfinalized block. Canonical
    /// recovery treats it as corruption for every non-genesis finalized block.
    fn load_block_artifacts(&self, _hash: &Hash) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Load and validate a canonical Commitment v2 artifact by block hash.
    /// Missing artifacts remain `Ok(None)`; malformed stored bytes are an
    /// error rather than a fabricated empty commitment.
    fn load_commitment(&self, hash: &Hash) -> anyhow::Result<Option<CommitmentV2>> {
        self.load_block_artifacts(hash)?
            .map(|bytes| CommitmentV2::from_canonical_bytes(&bytes))
            .transpose()
            .map_err(Into::into)
    }

    /// Load canonical execution artifacts using the finalized height index.
    fn load_block_artifacts_by_height(&self, height: u64) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(block) = self.get_by_height(height) else {
            return Ok(None);
        };
        self.load_block_artifacts(&block.hash())
    }

    /// Load and validate a canonical Commitment v2 artifact by finalized
    /// height.
    fn load_commitment_by_height(&self, height: u64) -> anyhow::Result<Option<CommitmentV2>> {
        let Some(block) = self.get_by_height(height) else {
            return Ok(None);
        };
        self.load_commitment(&block.hash())
    }

    /// Load one finalized transaction receipt by its canonical transaction ID.
    ///
    /// Stores that do not provide an authenticated durable index must fail
    /// closed instead of turning an absent implementation into a pending or
    /// successful transaction result.
    fn load_transaction_receipt(
        &self,
        _tx_id: &Hash,
    ) -> anyhow::Result<Option<TransactionReceiptLookup>> {
        anyhow::bail!("persistent store does not support transaction receipt lookup")
    }

    /// Load and schema-validate the durable full-state root for a block
    /// hash. Implementations must reject raw legacy rows and malformed records.
    /// `None` means that the block has no state-root artifact; canonical
    /// restart recovery treats that as an error for non-genesis blocks.
    fn load_state_root(&self, _hash: &Hash) -> anyhow::Result<Option<Hash>> {
        Ok(None)
    }

    /// Load the durable full-state root through the finalized height
    /// index.
    fn load_state_root_by_height(&self, height: u64) -> anyhow::Result<Option<Hash>> {
        let Some(block) = self.get_by_height(height) else {
            return Ok(None);
        };
        self.load_state_root(&block.hash())
    }

    /// Save a batch of candles (key-value pairs already serialized)
    fn save_candles_batch(&self, entries: &[(Vec<u8>, Vec<u8>)]) -> anyhow::Result<()>;

    /// Load candles for a (symbol, interval) pair
    fn load_candles(
        &self,
        symbol: &str,
        interval_str: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Candle>>;
}

#[cfg(test)]
mod tests {
    use super::{
        ConsensusContext, ConsensusState, StateRootRecord, StateRootRecordError,
        STATE_ROOT_SCHEMA_VERSION,
    };

    #[test]
    fn recovery_context_rejects_another_genesis_domain() {
        let expected = ConsensusContext::with_genesis(0, [7u8; 32], [1u8; 32]);
        let mut persisted = ConsensusState::genesis();
        persisted.epoch = expected.epoch;
        persisted.committee_hash = expected.committee_hash;
        persisted.genesis_hash = [2u8; 32];

        assert_ne!(persisted.context(), expected);
    }

    #[test]
    fn state_root_record_is_versioned_and_strictly_canonical() {
        let record = StateRootRecord::new([0xabu8; 32]);
        assert_eq!(record.schema_version, STATE_ROOT_SCHEMA_VERSION);
        let bytes = record.canonical_bytes().expect("current record encodes");
        assert_eq!(
            StateRootRecord::from_canonical_bytes(&bytes).expect("current record decodes"),
            record
        );

        let mut unsupported = bytes.clone();
        unsupported[..2].copy_from_slice(&(STATE_ROOT_SCHEMA_VERSION + 1).to_le_bytes());
        assert!(matches!(
            StateRootRecord::from_canonical_bytes(&unsupported),
            Err(StateRootRecordError::UnsupportedSchemaVersion(version))
                if version == STATE_ROOT_SCHEMA_VERSION + 1
        ));

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            StateRootRecord::from_canonical_bytes(&trailing),
            Err(StateRootRecordError::InvalidLength { actual: 35, .. })
        ));

        assert!(matches!(
            StateRootRecord::from_canonical_bytes(&bytes[..33]),
            Err(StateRootRecordError::InvalidLength { actual: 33, .. })
        ));
        assert!(matches!(
            StateRootRecord::from_canonical_bytes(&[0xabu8; 32]),
            Err(StateRootRecordError::LegacyRawRoot)
        ));
    }
}
