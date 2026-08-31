//! RocksDB Storage Implementation
//!
//! Uses column families for logical separation:
//! - blocks: Hash -> Block
//! - height_index: u64 -> Hash
//! - consensus: safety state (high_qc, locked_qc, voted_views, current_view)
//! - snapshots: height -> AppSnapshot
//! - meta: committed_height, committed_hash

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rocksdb::{ColumnFamilyDescriptor, Options, WriteBatch, WriteOptions, DB};
use serde::{Deserialize, Serialize};

use super::{
    AppSnapshot, ConsensusState, EquivocationJournalCapability, PersistentStore, StateRootRecord,
    TransactionReceiptLookup, MAX_EQUIVOCATION_PROOF_BYTES, MAX_EQUIVOCATION_PROOF_RECORDS,
    MAX_TRANSACTION_RECEIPT_INDEX_BYTES, TRANSACTION_RECEIPT_INDEX_SCHEMA_VERSION,
};
use crate::app::{candles::Candle, ConsensusTransaction};
use crate::consensus::{
    BlockStore, EpochTransitionProof, EquivocationProof, MAX_SPECULATIVE_BLOCK_BYTES,
    MAX_SPECULATIVE_STORE_BLOCKS, MAX_SPECULATIVE_STORE_BYTES,
};
use crate::types::{Block, CommitmentV2, Hash, TransactionReceipt};

/// Column family names
const CF_BLOCKS: &str = "blocks";
const CF_HEIGHT_INDEX: &str = "height_index";
const CF_CONSENSUS: &str = "consensus";
const CF_SNAPSHOTS: &str = "snapshots";
const CF_META: &str = "meta";
const CF_CANDLES: &str = "candles";
const CF_BLOCK_ARTIFACTS: &str = "block_artifacts";
const CF_TRANSACTION_RECEIPTS: &str = "transaction_receipts";
const CF_STATE_ROOTS: &str = "state_roots";
const CF_EQUIVOCATION_PROOFS: &str = "equivocation_proofs";
const CF_EPOCH_TRANSITIONS: &str = "epoch_transitions";
const EPOCH_TRANSITION_MARKER_KEY: &[u8] = b"current";
const SPECULATIVE_MANIFEST_PREFIX: &[u8] = b"speculative/";
const EQUIVOCATION_JOURNAL_KEY_BYTES: usize = 1 + 8 + 32 + 32 + 32;

/// Canonical durable value behind the transaction-ID index.
///
/// The row is deliberately redundant with the Commitment v2 artifact.  The
/// redundancy lets reads detect an index row that was modified independently;
/// the artifact/header verification below remains the authentication source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TransactionReceiptIndexRecord {
    schema_version: u16,
    tx_id: Hash,
    block_hash: Hash,
    block_height: u64,
    tx_index: u32,
    receipt: TransactionReceipt,
}

impl TransactionReceiptIndexRecord {
    fn new(block: &Block, receipt: &TransactionReceipt) -> Self {
        Self {
            schema_version: TRANSACTION_RECEIPT_INDEX_SCHEMA_VERSION,
            tx_id: receipt.tx_id,
            block_hash: block.hash(),
            block_height: block.height,
            tx_index: receipt.tx_index,
            receipt: receipt.clone(),
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != TRANSACTION_RECEIPT_INDEX_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported transaction receipt index schema version {}",
                self.schema_version
            );
        }
        if self.tx_id != self.receipt.tx_id {
            anyhow::bail!("transaction receipt index tx ID does not match its receipt");
        }
        if self.tx_index != self.receipt.tx_index {
            anyhow::bail!(
                "transaction receipt index position {} does not match receipt position {}",
                self.tx_index,
                self.receipt.tx_index
            );
        }
        self.receipt
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid indexed transaction receipt: {error}"))?;
        Ok(())
    }

    fn canonical_bytes(&self) -> anyhow::Result<Vec<u8>> {
        self.validate()?;
        let bytes = bincode::serialize(self).map_err(|error| {
            anyhow::anyhow!("transaction receipt index encoding failed: {error}")
        })?;
        if bytes.len() > MAX_TRANSACTION_RECEIPT_INDEX_BYTES {
            anyhow::bail!(
                "transaction receipt index row {} bytes exceeds {}",
                bytes.len(),
                MAX_TRANSACTION_RECEIPT_INDEX_BYTES
            );
        }
        Ok(bytes)
    }

    fn from_canonical_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() > MAX_TRANSACTION_RECEIPT_INDEX_BYTES {
            anyhow::bail!(
                "transaction receipt index row {} bytes exceeds {}",
                bytes.len(),
                MAX_TRANSACTION_RECEIPT_INDEX_BYTES
            );
        }
        let record: Self = bincode::deserialize(bytes).map_err(|error| {
            anyhow::anyhow!("transaction receipt index decoding failed: {error}")
        })?;
        let canonical = record.canonical_bytes()?;
        if canonical != bytes {
            anyhow::bail!("transaction receipt index row is not canonical");
        }
        Ok(record)
    }

    fn lookup(self) -> TransactionReceiptLookup {
        TransactionReceiptLookup {
            tx_id: self.tx_id,
            block_hash: self.block_hash,
            block_height: self.block_height,
            tx_index: self.tx_index,
            receipt: self.receipt,
        }
    }
}

fn speculative_manifest_key(hash: &Hash) -> Vec<u8> {
    let mut key = SPECULATIVE_MANIFEST_PREFIX.to_vec();
    key.extend_from_slice(hash);
    key
}

fn reaches_ancestor(blocks: &HashMap<Hash, Block>, descendant: Hash, ancestor: Hash) -> bool {
    let mut current = descendant;
    let mut visited = HashSet::new();
    loop {
        if current == ancestor {
            return true;
        }
        if !visited.insert(current) {
            return false;
        }
        let Some(block) = blocks.get(&current) else {
            return false;
        };
        current = block.parent;
    }
}

fn speculative_ancestor_closure(blocks: &HashMap<Hash, Block>, roots: &[Hash]) -> HashSet<Hash> {
    let mut protected = HashSet::new();
    for root in roots {
        let mut current = *root;
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(current) {
                break;
            }
            let Some(block) = blocks.get(&current) else {
                break;
            };
            protected.insert(current);
            current = block.parent;
        }
    }
    protected
}

/// RocksDB-backed persistent store
pub struct RocksDbStore {
    db: DB,
    /// Serialize the speculative journal check/account/write transition with
    /// pruning and canonical promotion.  RocksDB batches are atomic, but the
    /// capacity check happens before the batch is assembled, so the database
    /// alone cannot close that TOCTOU window.
    speculative_journal_lock: Mutex<()>,
    /// Serialize bounded proof-journal validation and writes/deletes.
    equivocation_journal_lock: Mutex<()>,
}

impl RocksDbStore {
    /// Open or create a RocksDB store at the given path
    pub fn open<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cfs = vec![
            ColumnFamilyDescriptor::new(CF_BLOCKS, Options::default()),
            ColumnFamilyDescriptor::new(CF_HEIGHT_INDEX, Options::default()),
            ColumnFamilyDescriptor::new(CF_CONSENSUS, Options::default()),
            ColumnFamilyDescriptor::new(CF_SNAPSHOTS, Options::default()),
            ColumnFamilyDescriptor::new(CF_META, Options::default()),
            ColumnFamilyDescriptor::new(CF_CANDLES, Options::default()),
            ColumnFamilyDescriptor::new(CF_BLOCK_ARTIFACTS, Options::default()),
            ColumnFamilyDescriptor::new(CF_TRANSACTION_RECEIPTS, Options::default()),
            ColumnFamilyDescriptor::new(CF_STATE_ROOTS, Options::default()),
            ColumnFamilyDescriptor::new(CF_EQUIVOCATION_PROOFS, Options::default()),
            ColumnFamilyDescriptor::new(CF_EPOCH_TRANSITIONS, Options::default()),
        ];

        let db = DB::open_cf_descriptors(&opts, path, cfs)?;
        Ok(Self {
            db,
            speculative_journal_lock: Mutex::new(()),
            equivocation_journal_lock: Mutex::new(()),
        })
    }

    /// Get column family handle (panics if not found - programming error)
    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(name).expect("column family must exist")
    }

    /// Durability boundary for consensus safety state.
    ///
    /// RocksDB's default write options acknowledge after the WAL write has
    /// reached the OS.  Vote safety and finalized metadata must survive a
    /// power loss before the runner records the corresponding in-memory
    /// state, so these writes explicitly request WAL fsync.
    fn sync_write_options() -> WriteOptions {
        let mut options = WriteOptions::default();
        options.set_sync(true);
        options
    }

    fn lock_speculative_journal(&self) -> anyhow::Result<MutexGuard<'_, ()>> {
        self.speculative_journal_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("speculative journal lock is poisoned"))
    }

    fn lock_equivocation_journal(&self) -> anyhow::Result<MutexGuard<'_, ()>> {
        self.equivocation_journal_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("equivocation journal lock is poisoned"))
    }

    /// Scan the dedicated proof column and reject any malformed, noncanonical,
    /// mis-keyed, or over-bounded row before returning data to consensus.
    fn load_equivocation_proof_rows(
        &self,
    ) -> anyhow::Result<Vec<(Vec<u8>, EquivocationProof, usize)>> {
        let mut rows = Vec::new();
        let mut total_bytes = 0usize;
        for item in self.db.iterator_cf(
            self.cf(CF_EQUIVOCATION_PROOFS),
            rocksdb::IteratorMode::Start,
        ) {
            let (key, value) = item?;
            if key.len() != EQUIVOCATION_JOURNAL_KEY_BYTES {
                anyhow::bail!(
                    "equivocation proof journal key has invalid length: {}",
                    key.len()
                );
            }
            let row_bytes = key
                .len()
                .checked_add(value.len())
                .ok_or_else(|| anyhow::anyhow!("equivocation proof journal size overflow"))?;
            if row_bytes > MAX_EQUIVOCATION_PROOF_BYTES {
                anyhow::bail!(
                    "equivocation proof journal row exceeds {} bytes",
                    MAX_EQUIVOCATION_PROOF_BYTES
                );
            }
            let next_total = total_bytes
                .checked_add(row_bytes)
                .ok_or_else(|| anyhow::anyhow!("equivocation proof journal size overflow"))?;
            if next_total > MAX_EQUIVOCATION_PROOF_BYTES {
                anyhow::bail!(
                    "equivocation proof journal exceeds {} bytes",
                    MAX_EQUIVOCATION_PROOF_BYTES
                );
            }
            let proof: EquivocationProof = serde_json::from_slice(&value).map_err(|error| {
                anyhow::anyhow!("invalid equivocation proof journal row: {error}")
            })?;
            let canonical_bytes = serde_json::to_vec(&proof).map_err(|error| {
                anyhow::anyhow!("cannot canonicalize equivocation proof journal row: {error}")
            })?;
            if canonical_bytes.as_slice() != value.as_ref() {
                anyhow::bail!("equivocation proof journal row is not canonical");
            }
            if proof.journal_key() != key.as_ref() {
                anyhow::bail!("equivocation proof journal key does not match proof");
            }

            total_bytes = next_total;
            if rows.len() >= MAX_EQUIVOCATION_PROOF_RECORDS {
                anyhow::bail!(
                    "equivocation proof journal exceeds {} records",
                    MAX_EQUIVOCATION_PROOF_RECORDS
                );
            }
            rows.push((key.to_vec(), proof, row_bytes));
        }
        Ok(rows)
    }

    fn save_equivocation_proof_row(&self, proof: &EquivocationProof) -> anyhow::Result<()> {
        let proof = proof
            .canonicalized()
            .map_err(|error| anyhow::anyhow!("invalid equivocation proof: {error}"))?;
        let key = proof.journal_key();
        let value = serde_json::to_vec(&proof)
            .map_err(|error| anyhow::anyhow!("cannot encode equivocation proof: {error}"))?;

        let _journal_guard = self.lock_equivocation_journal()?;
        let rows = self.load_equivocation_proof_rows()?;
        // The authenticated context/offender key is first-write-wins. An
        // existing valid row is also the idempotent success path.
        if rows.iter().any(|(stored_key, _, _)| *stored_key == key) {
            return Ok(());
        }

        let row_bytes = key
            .len()
            .checked_add(value.len())
            .ok_or_else(|| anyhow::anyhow!("equivocation proof journal size overflow"))?;
        let total_bytes: usize = rows
            .iter()
            .map(|(_, _, bytes)| *bytes)
            .sum::<usize>()
            .checked_add(row_bytes)
            .ok_or_else(|| anyhow::anyhow!("equivocation proof journal size overflow"))?;
        if rows.len() >= MAX_EQUIVOCATION_PROOF_RECORDS {
            anyhow::bail!(
                "equivocation proof journal is full: maximum {} records",
                MAX_EQUIVOCATION_PROOF_RECORDS
            );
        }
        if total_bytes > MAX_EQUIVOCATION_PROOF_BYTES {
            anyhow::bail!(
                "equivocation proof journal is full: maximum {} bytes",
                MAX_EQUIVOCATION_PROOF_BYTES
            );
        }

        let mut batch = WriteBatch::default();
        batch.put_cf(self.cf(CF_EQUIVOCATION_PROOFS), key, value);
        self.db.write_opt(batch, &Self::sync_write_options())?;
        Ok(())
    }

    fn load_equivocation_proofs_rows(&self) -> anyhow::Result<Vec<EquivocationProof>> {
        let _journal_guard = self.lock_equivocation_journal()?;
        Ok(self
            .load_equivocation_proof_rows()?
            .into_iter()
            .map(|(_, proof, _)| proof)
            .collect())
    }

    fn delete_equivocation_proof_row(&self, proof: &EquivocationProof) -> anyhow::Result<()> {
        let proof = proof
            .canonicalized()
            .map_err(|error| anyhow::anyhow!("invalid equivocation proof: {error}"))?;
        let key = proof.journal_key();
        let _journal_guard = self.lock_equivocation_journal()?;
        let rows = self.load_equivocation_proof_rows()?;
        if !rows.iter().any(|(stored_key, _, _)| *stored_key == key) {
            return Ok(());
        }

        let mut batch = WriteBatch::default();
        batch.delete_cf(self.cf(CF_EQUIVOCATION_PROOFS), key);
        self.db.write_opt(batch, &Self::sync_write_options())?;
        Ok(())
    }

    fn save_epoch_transition_proof_row(&self, proof: &EpochTransitionProof) -> anyhow::Result<()> {
        let value = proof
            .canonical_bytes()
            .map_err(|error| anyhow::anyhow!("invalid epoch-transition proof: {error}"))?;
        if let Some(existing) = self
            .db
            .get_cf(self.cf(CF_EPOCH_TRANSITIONS), EPOCH_TRANSITION_MARKER_KEY)?
        {
            let existing =
                EpochTransitionProof::from_canonical_bytes(&existing).map_err(|error| {
                    anyhow::anyhow!("invalid stored epoch-transition proof: {error}")
                })?;
            if existing != *proof {
                anyhow::bail!(
                    "a different epoch-transition proof is already staged; activation must resolve it first"
                );
            }
            return Ok(());
        }

        let mut batch = WriteBatch::default();
        batch.put_cf(
            self.cf(CF_EPOCH_TRANSITIONS),
            EPOCH_TRANSITION_MARKER_KEY,
            value,
        );
        self.db.write_opt(batch, &Self::sync_write_options())?;
        Ok(())
    }

    fn load_epoch_transition_proof_row(&self) -> anyhow::Result<Option<EpochTransitionProof>> {
        let Some(value) = self
            .db
            .get_cf(self.cf(CF_EPOCH_TRANSITIONS), EPOCH_TRANSITION_MARKER_KEY)?
        else {
            return Ok(None);
        };
        EpochTransitionProof::from_canonical_bytes(&value)
            .map(Some)
            .map_err(|error| anyhow::anyhow!("invalid stored epoch-transition proof: {error}"))
    }

    fn clear_epoch_transition_proof_row(&self) -> anyhow::Result<()> {
        let mut batch = WriteBatch::default();
        batch.delete_cf(self.cf(CF_EPOCH_TRANSITIONS), EPOCH_TRANSITION_MARKER_KEY);
        self.db.write_opt(batch, &Self::sync_write_options())?;
        Ok(())
    }

    fn validate_authenticated_commitment(
        block: &Block,
        commitment: Option<&CommitmentV2>,
    ) -> anyhow::Result<()> {
        if block.height == 0 {
            return Ok(());
        }
        let Some(commitment) = commitment else {
            anyhow::bail!(
                "non-genesis block {} is missing its execution commitment",
                block.height
            );
        };
        let root = commitment.root()?;
        if block.commitment_root == [0u8; 32] || block.commitment_root != root {
            anyhow::bail!(
                "execution commitment root does not match block {}",
                block.height
            );
        }
        Ok(())
    }

    fn save_speculative_row(&self, block: &Block) -> anyhow::Result<()> {
        let _journal_guard = self.lock_speculative_journal()?;
        if self.speculative_row_exists(block)? {
            return Ok(());
        }
        <Self as BlockStore>::ensure_speculative_capacity(
            self,
            block,
            MAX_SPECULATIVE_STORE_BLOCKS,
            MAX_SPECULATIVE_STORE_BYTES,
        )?;
        let hash = block.hash();
        let block_bytes = serde_json::to_vec(block)?;
        let manifest = serde_json::to_vec(&(block.height, block.parent, block_bytes.len()))?;
        let mut batch = WriteBatch::default();
        batch.put_cf(self.cf(CF_BLOCKS), hash, &block_bytes);
        batch.put_cf(self.cf(CF_META), speculative_manifest_key(&hash), manifest);
        self.db.write_opt(batch, &Self::sync_write_options())?;
        Ok(())
    }

    /// Return true for an existing, internally consistent row.  Block hashes
    /// exclude `justify`, so speculative writes must be first-write-wins: a
    /// later certificate must not replace the body or grow the journal.
    fn speculative_row_exists(&self, block: &Block) -> anyhow::Result<bool> {
        let hash = block.hash();
        let existing = self.db.get_cf(self.cf(CF_BLOCKS), hash)?;
        let manifest_key = speculative_manifest_key(&hash);
        let manifest = self.db.get_cf(self.cf(CF_META), &manifest_key)?;
        match (existing, manifest) {
            (None, None) => Ok(false),
            (None, Some(_)) => anyhow::bail!(
                "speculative manifest exists without block row {}",
                hex::encode(hash)
            ),
            (Some(bytes), manifest) => {
                let stored: Block = serde_json::from_slice(&bytes).map_err(|error| {
                    anyhow::anyhow!(
                        "stored block row {} is not valid: {error}",
                        hex::encode(hash)
                    )
                })?;
                if stored.hash() != hash {
                    anyhow::bail!(
                        "stored block row {} has a mismatched body hash",
                        hex::encode(hash)
                    );
                }
                if let Some(manifest_bytes) = manifest {
                    self.validate_speculative_manifest(&hash, &manifest_bytes)?;
                } else {
                    let canonical = self
                        .db
                        .get_cf(self.cf(CF_HEIGHT_INDEX), stored.height.to_be_bytes())?
                        .is_some_and(|canonical_hash| canonical_hash.as_slice() == hash.as_slice());
                    if !canonical {
                        anyhow::bail!(
                            "existing unfinalized block {} has no speculative manifest",
                            hex::encode(hash)
                        );
                    }
                }
                Ok(true)
            }
        }
    }

    fn decode_speculative_manifest(
        hash: &Hash,
        manifest_bytes: &[u8],
    ) -> anyhow::Result<(u64, Hash, usize)> {
        serde_json::from_slice(manifest_bytes).map_err(|error| {
            anyhow::anyhow!(
                "speculative manifest {} is not valid: {error}",
                hex::encode(hash)
            )
        })
    }

    fn validate_speculative_row(
        hash: &Hash,
        bytes: &[u8],
        manifest_bytes: &[u8],
    ) -> anyhow::Result<(Block, usize)> {
        let block: Block = serde_json::from_slice(&bytes).map_err(|error| {
            anyhow::anyhow!(
                "stored block row {} is not valid: {error}",
                hex::encode(hash)
            )
        })?;
        if block.hash() != *hash {
            anyhow::bail!(
                "stored block row {} has a mismatched body hash",
                hex::encode(hash)
            );
        }
        let (height, parent, row_bytes) = Self::decode_speculative_manifest(hash, manifest_bytes)?;
        if height != block.height || parent != block.parent || row_bytes != bytes.len() {
            anyhow::bail!(
                "speculative manifest {} does not match its block row",
                hex::encode(hash)
            );
        }
        // The manifest's row_bytes field is only an integrity claim.  Always
        // account using the exact raw value stored in CF_BLOCKS so a future
        // manifest format change cannot make capacity accounting trust an
        // unvalidated payload size.
        Ok((block, bytes.len()))
    }

    fn validate_speculative_manifest(
        &self,
        hash: &Hash,
        manifest_bytes: &[u8],
    ) -> anyhow::Result<(Block, usize)> {
        let bytes = self.db.get_cf(self.cf(CF_BLOCKS), hash)?.ok_or_else(|| {
            anyhow::anyhow!(
                "speculative manifest exists without block row {}",
                hex::encode(hash)
            )
        })?;
        Self::validate_speculative_row(hash, &bytes, manifest_bytes)
    }

    /// Build candle key: `{symbol}\0{interval_str}\0{timestamp_be_u64}`
    pub fn candle_key(symbol: &str, interval_str: &str, timestamp: u64) -> Vec<u8> {
        let mut key = Vec::with_capacity(symbol.len() + 1 + interval_str.len() + 1 + 8);
        key.extend_from_slice(symbol.as_bytes());
        key.push(0);
        key.extend_from_slice(interval_str.as_bytes());
        key.push(0);
        key.extend_from_slice(&timestamp.to_be_bytes());
        key
    }

    /// Save a batch of candles atomically
    pub fn save_candles_batch(&self, entries: &[(Vec<u8>, Vec<u8>)]) -> anyhow::Result<()> {
        let mut batch = WriteBatch::default();
        let cf = self.cf(CF_CANDLES);
        for (key, value) in entries {
            batch.put_cf(cf, key, value);
        }
        self.db.write(batch)?;
        Ok(())
    }

    /// Load candles for a (symbol, interval) pair, returning the latest `limit` candles
    pub fn load_candles(
        &self,
        symbol: &str,
        interval_str: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Candle>> {
        let cf = self.cf(CF_CANDLES);
        // Build prefix for this (symbol, interval)
        let mut prefix = Vec::with_capacity(symbol.len() + 1 + interval_str.len() + 1);
        prefix.extend_from_slice(symbol.as_bytes());
        prefix.push(0);
        prefix.extend_from_slice(interval_str.as_bytes());
        prefix.push(0);

        // Scan forward from prefix, collect all matching entries
        let iter = self.db.iterator_cf(
            cf,
            rocksdb::IteratorMode::From(&prefix, rocksdb::Direction::Forward),
        );

        let mut candles = Vec::new();
        for item in iter {
            let (key, value) = item?;
            // Check key still has our prefix
            if !key.starts_with(&prefix) {
                break;
            }
            if let Ok(candle) = serde_json::from_slice::<Candle>(&value) {
                candles.push(candle);
            }
        }

        // Keep only the latest `limit` candles
        if candles.len() > limit {
            candles = candles.split_off(candles.len() - limit);
        }

        Ok(candles)
    }

    /// Load the canonical execution artifact bytes for a block hash.
    ///
    /// Artifacts are written only by `commit_block_with_artifacts`, in the
    /// same synced WriteBatch as the finalized block and commit metadata.
    /// Therefore a missing value is meaningful: it denotes genesis, a block
    /// that has not crossed finality, or corrupt non-genesis storage.
    pub fn load_block_artifacts(&self, hash: &Hash) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.db.get_cf(self.cf(CF_BLOCK_ARTIFACTS), hash)?)
    }

    /// Load and validate a canonical Commitment v2 artifact by block hash.
    pub fn load_commitment(&self, hash: &Hash) -> anyhow::Result<Option<CommitmentV2>> {
        self.load_block_artifacts(hash)?
            .map(|bytes| CommitmentV2::from_canonical_bytes(&bytes))
            .transpose()
            .map_err(Into::into)
    }

    /// Load canonical execution artifacts through the finalized height index.
    pub fn load_block_artifacts_by_height(&self, height: u64) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(hash_bytes) = self
            .db
            .get_cf(self.cf(CF_HEIGHT_INDEX), height.to_be_bytes())?
        else {
            return Ok(None);
        };
        if hash_bytes.len() != 32 {
            return Ok(None);
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hash_bytes);
        self.load_block_artifacts(&hash)
    }

    /// Load and validate a canonical Commitment v2 artifact by finalized
    /// height.
    pub fn load_commitment_by_height(&self, height: u64) -> anyhow::Result<Option<CommitmentV2>> {
        let Some(hash_bytes) = self
            .db
            .get_cf(self.cf(CF_HEIGHT_INDEX), height.to_be_bytes())?
        else {
            return Ok(None);
        };
        if hash_bytes.len() != 32 {
            return Ok(None);
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hash_bytes);
        self.load_commitment(&hash)
    }

    /// Decode and cross-check the exact block payload against its Commitment
    /// v2 receipts. System entries are receipt-bearing protocol entries, but
    /// only signed envelopes get a global transaction-ID index row.
    fn validate_commitment_payload<'a>(
        block: &Block,
        commitment: &'a CommitmentV2,
    ) -> anyhow::Result<Vec<(Hash, &'a TransactionReceipt, bool)>> {
        let entries = crate::app::AppState::decode_consensus_payload(&block.payload)
            .map_err(|error| anyhow::anyhow!("invalid consensus transaction payload: {error}"))?;
        if entries.len() != commitment.receipts.len() {
            anyhow::bail!(
                "block {} payload contains {} transactions but its commitment contains {} receipts",
                block.height,
                entries.len(),
                commitment.receipts.len()
            );
        }

        let mut validated = Vec::with_capacity(entries.len());
        for (index, (entry, receipt)) in entries.iter().zip(&commitment.receipts).enumerate() {
            let expected_index = index as u32;
            if receipt.tx_index != expected_index {
                anyhow::bail!(
                    "block {} receipt index {} does not match payload index {}",
                    block.height,
                    receipt.tx_index,
                    expected_index
                );
            }
            let entry_hash = entry.hash().map_err(|error| {
                anyhow::anyhow!("cannot hash block transaction {index}: {error}")
            })?;
            if receipt.tx_id != entry_hash {
                anyhow::bail!(
                    "block {} receipt transaction ID at index {} does not match its payload entry",
                    block.height,
                    expected_index
                );
            }
            validated.push((
                entry_hash,
                receipt,
                matches!(entry, ConsensusTransaction::Signed(_)),
            ));
        }
        Ok(validated)
    }

    /// Build the transaction-ID index rows for a finalized Commitment v2.
    ///
    /// The rows are derived only after the commitment has been checked against
    /// the block header and payload. Duplicate signed IDs and a pre-existing
    /// different mapping are rejected before the caller assembles its
    /// finalized WriteBatch.
    fn transaction_receipt_index_rows(
        &self,
        block: &Block,
        commitment: Option<&CommitmentV2>,
    ) -> anyhow::Result<Vec<(Hash, Vec<u8>)>> {
        // Genesis has no authenticated commitment root in the current block
        // protocol.  Never expose a receipt from an unauthenticated genesis
        // artifact through the finalized transaction index.
        if block.height == 0 {
            return Ok(Vec::new());
        }
        let Some(commitment) = commitment else {
            anyhow::bail!(
                "non-genesis block {} is missing its transaction receipt commitment",
                block.height
            );
        };
        Self::validate_authenticated_commitment(block, Some(commitment))?;

        let mut seen = HashSet::with_capacity(commitment.receipts.len());
        let mut rows = Vec::with_capacity(commitment.receipts.len());
        for (tx_id, receipt, is_signed) in Self::validate_commitment_payload(block, commitment)? {
            if !is_signed {
                continue;
            }
            if !seen.insert(tx_id) {
                anyhow::bail!(
                    "transaction ID {} occurs more than once in block {}",
                    hex::encode(tx_id),
                    block.height
                );
            }
            let record = TransactionReceiptIndexRecord::new(block, receipt);
            let bytes = record.canonical_bytes()?;
            if let Some(existing) = self.db.get_cf(self.cf(CF_TRANSACTION_RECEIPTS), tx_id)? {
                let existing_record = TransactionReceiptIndexRecord::from_canonical_bytes(
                    &existing,
                )
                .map_err(|error| {
                    anyhow::anyhow!(
                        "existing transaction receipt index row for {} is invalid: {error}",
                        hex::encode(tx_id)
                    )
                })?;
                if existing_record != record {
                    anyhow::bail!(
                        "transaction ID {} is already indexed at a different finalized location",
                        hex::encode(tx_id)
                    );
                }
            }
            rows.push((tx_id, bytes));
        }
        Ok(rows)
    }

    /// Load one transaction receipt and re-authenticate the indexed location
    /// against the finalized block and its Commitment v2 artifact.
    pub fn load_transaction_receipt(
        &self,
        tx_id: &Hash,
    ) -> anyhow::Result<Option<TransactionReceiptLookup>> {
        let Some(bytes) = self.db.get_cf(self.cf(CF_TRANSACTION_RECEIPTS), tx_id)? else {
            return Ok(None);
        };
        let record = TransactionReceiptIndexRecord::from_canonical_bytes(&bytes)?;
        if record.tx_id != *tx_id {
            anyhow::bail!(
                "transaction receipt index key {} does not match row ID {}",
                hex::encode(tx_id),
                hex::encode(record.tx_id)
            );
        }

        let block = self.get(&record.block_hash).ok_or_else(|| {
            anyhow::anyhow!(
                "transaction receipt index {} points to a missing or invalid block {}",
                hex::encode(tx_id),
                hex::encode(record.block_hash)
            )
        })?;
        if block.hash() != record.block_hash {
            anyhow::bail!(
                "transaction receipt index {} points to a block with a mismatched hash",
                hex::encode(tx_id)
            );
        }
        if block.height != record.block_height {
            anyhow::bail!(
                "transaction receipt index {} has block height {}, actual block height {}",
                hex::encode(tx_id),
                record.block_height,
                block.height
            );
        }
        let canonical = self.get_by_height(record.block_height).ok_or_else(|| {
            anyhow::anyhow!(
                "transaction receipt index {} points to a missing finalized height {}",
                hex::encode(tx_id),
                record.block_height
            )
        })?;
        if canonical.hash() != record.block_hash {
            anyhow::bail!(
                "transaction receipt index {} points outside the canonical finalized chain",
                hex::encode(tx_id)
            );
        }
        let Some(committed_height) = <Self as PersistentStore>::load_committed_height(self)? else {
            anyhow::bail!(
                "transaction receipt index {} exists without committed-height metadata",
                hex::encode(tx_id)
            );
        };
        if record.block_height > committed_height {
            anyhow::bail!(
                "transaction receipt index {} points above committed height {}",
                hex::encode(tx_id),
                committed_height
            );
        }

        let commitment = self.load_commitment(&record.block_hash)?.ok_or_else(|| {
            anyhow::anyhow!(
                "transaction receipt index {} points to a block without a Commitment v2 artifact",
                hex::encode(tx_id)
            )
        })?;
        Self::validate_authenticated_commitment(&block, Some(&commitment))?;
        let payload_entries = Self::validate_commitment_payload(&block, &commitment)?;
        let (_, _, is_signed) = payload_entries
            .get(record.tx_index as usize)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "transaction receipt index {} points outside block {} payload",
                    hex::encode(tx_id),
                    record.block_height
                )
            })?;
        if !is_signed {
            anyhow::bail!(
                "transaction receipt index {} points to a system transaction, not a signed envelope",
                hex::encode(tx_id)
            );
        }
        let receipt = commitment
            .receipts
            .get(record.tx_index as usize)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "transaction receipt index {} points outside block {} receipt list",
                    hex::encode(tx_id),
                    record.block_height
                )
            })?;
        if receipt != &record.receipt || receipt.tx_id != *tx_id {
            anyhow::bail!(
                "transaction receipt index {} does not match the authenticated commitment",
                hex::encode(tx_id)
            );
        }

        Ok(Some(record.lookup()))
    }

    /// Load the canonical full-state shadow root by block hash.
    pub fn load_state_root(&self, hash: &Hash) -> anyhow::Result<Option<Hash>> {
        let Some(bytes) = self.db.get_cf(self.cf(CF_STATE_ROOTS), hash)? else {
            return Ok(None);
        };
        let record = StateRootRecord::from_canonical_bytes(&bytes)
            .map_err(|error| anyhow::anyhow!("invalid stored state-root record: {error}"))?;
        Ok(Some(record.root))
    }

    /// Load the canonical full-state shadow root by finalized height.
    pub fn load_state_root_by_height(&self, height: u64) -> anyhow::Result<Option<Hash>> {
        let Some(hash_bytes) = self
            .db
            .get_cf(self.cf(CF_HEIGHT_INDEX), height.to_be_bytes())?
        else {
            return Ok(None);
        };
        let hash: Hash = hash_bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("height index hash has invalid length"))?;
        self.load_state_root(&hash)
    }
}

impl RocksDbStore {
    /// Prune blocks and height index entries below the given height.
    /// Returns number of blocks deleted.
    pub fn prune_blocks_before(&self, height: u64) -> anyhow::Result<u64> {
        let cf_blocks = self.cf(CF_BLOCKS);
        let cf_height = self.cf(CF_HEIGHT_INDEX);
        let cf_artifacts = self.cf(CF_BLOCK_ARTIFACTS);
        let cf_transaction_receipts = self.cf(CF_TRANSACTION_RECEIPTS);
        let cf_state_roots = self.cf(CF_STATE_ROOTS);
        let mut batch = WriteBatch::default();
        let mut count = 0u64;

        let iter = self.db.iterator_cf(cf_height, rocksdb::IteratorMode::Start);

        for item in iter {
            let (key, hash_bytes) = item?;
            if key.len() == 8 {
                let h = u64::from_be_bytes(key[..8].try_into().unwrap());
                if h >= height {
                    break;
                }
                if h > 0 {
                    let block_hash: Hash = hash_bytes.as_ref().try_into().map_err(|_| {
                        anyhow::anyhow!("height index hash at height {} has invalid length", h)
                    })?;
                    let block = self.get(&block_hash).ok_or_else(|| {
                        anyhow::anyhow!("cannot prune block {} with a missing or invalid body", h)
                    })?;
                    if block.hash() != block_hash || block.height != h {
                        anyhow::bail!("block at height {} has an inconsistent hash or height", h);
                    }
                    let artifact_bytes =
                        self.db.get_cf(cf_artifacts, block_hash)?.ok_or_else(|| {
                            anyhow::anyhow!(
                            "cannot prune non-genesis block {} without a Commitment v2 artifact",
                            h
                        )
                        })?;
                    let commitment = CommitmentV2::from_canonical_bytes(&artifact_bytes)
                        .map_err(|error| {
                            anyhow::anyhow!(
                                "cannot prune block {} with an invalid Commitment v2 artifact: {error}",
                                h
                            )
                        })?;
                    Self::validate_authenticated_commitment(&block, Some(&commitment))?;
                    for (tx_id, _, is_signed) in
                        Self::validate_commitment_payload(&block, &commitment)?
                    {
                        if is_signed {
                            batch.delete_cf(cf_transaction_receipts, tx_id);
                        }
                    }
                }
                batch.delete_cf(cf_height, &key);
                batch.delete_cf(cf_blocks, &*hash_bytes);
                batch.delete_cf(cf_artifacts, &*hash_bytes);
                batch.delete_cf(cf_state_roots, &*hash_bytes);
                count += 1;
            }
        }

        if count > 0 {
            self.db.write(batch)?;
            tracing::debug!(count, below_height = height, "Pruned old blocks");
        }
        Ok(count)
    }

    /// Prune snapshots below the given height, keeping the latest one for recovery.
    pub fn prune_snapshots_before(&self, height: u64) -> anyhow::Result<u64> {
        let cf = self.cf(CF_SNAPSHOTS);
        let mut batch = WriteBatch::default();
        let mut count = 0u64;
        let mut snapshot_heights: Vec<u64> = Vec::new();

        let iter = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        for item in iter {
            let (key, _) = item?;
            if key.len() == 8 {
                let h = u64::from_be_bytes(key[..8].try_into().unwrap());
                if h >= height {
                    break;
                }
                snapshot_heights.push(h);
            }
        }

        // Keep the latest snapshot below threshold for recovery
        if snapshot_heights.len() > 1 {
            snapshot_heights.pop(); // keep latest
            for h in &snapshot_heights {
                batch.delete_cf(cf, h.to_be_bytes());
                count += 1;
            }
        }

        if count > 0 {
            self.db.write(batch)?;
            tracing::debug!(count, "Pruned old snapshots");
        }
        Ok(count)
    }
}

impl BlockStore for RocksDbStore {
    fn save(&self, block: &Block) {
        let _journal_guard = self
            .lock_speculative_journal()
            .expect("speculative journal lock must not be poisoned");
        let hash = block.hash();
        let bytes = serde_json::to_vec(block).expect("block serialization failed");
        let mut batch = WriteBatch::default();
        batch.put_cf(self.cf(CF_BLOCKS), hash, &bytes);
        batch.put_cf(self.cf(CF_HEIGHT_INDEX), block.height.to_be_bytes(), hash);
        batch.delete_cf(self.cf(CF_META), speculative_manifest_key(&hash));
        self.db.write(batch).expect("block save failed");
    }

    fn save_speculative(&self, block: &Block) -> anyhow::Result<()> {
        self.save_speculative_row(block)
    }

    fn ensure_speculative_capacity(
        &self,
        block: &Block,
        max_blocks: usize,
        max_bytes: usize,
    ) -> anyhow::Result<()> {
        if self.speculative_row_exists(block)? {
            return Ok(());
        }
        let target_hash = block.hash();
        let prefix = SPECULATIVE_MANIFEST_PREFIX;
        let mut count = 0usize;
        let mut bytes = 0usize;
        let mut target_present = false;
        let mut iter = self.db.iterator_cf(
            self.cf(CF_META),
            rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward),
        );
        while let Some(item) = iter.next() {
            let (key, manifest_bytes) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            let hash_bytes = &key[prefix.len()..];
            if hash_bytes.len() != 32 {
                anyhow::bail!("speculative manifest key has invalid hash length");
            }
            let hash: Hash = hash_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("speculative manifest hash has invalid length"))?;
            let (block, raw_row_bytes) =
                self.validate_speculative_manifest(&hash, &manifest_bytes)?;
            let canonical = self
                .db
                .get_cf(self.cf(CF_HEIGHT_INDEX), block.height.to_be_bytes())?
                .is_some_and(|canonical_hash| canonical_hash.as_slice() == hash.as_slice());
            if canonical {
                continue;
            }
            count = count.saturating_add(1);
            bytes = bytes.saturating_add(
                raw_row_bytes
                    .saturating_add(key.len())
                    .saturating_add(manifest_bytes.len()),
            );
            target_present |= hash == target_hash;
        }
        if target_present {
            return Ok(());
        }
        let target_bytes = serde_json::to_vec(block)?;
        if target_bytes.len() > MAX_SPECULATIVE_BLOCK_BYTES {
            anyhow::bail!(
                "speculative block serialization {} bytes exceeds per-row limit {}",
                target_bytes.len(),
                MAX_SPECULATIVE_BLOCK_BYTES
            );
        }
        let target_manifest =
            serde_json::to_vec(&(block.height, block.parent, target_bytes.len()))?;
        let target_usage = target_bytes
            .len()
            .saturating_add(prefix.len() + target_hash.len())
            .saturating_add(target_manifest.len());
        if count.saturating_add(1) > max_blocks || bytes.saturating_add(target_usage) > max_bytes {
            anyhow::bail!(
                "speculative store capacity {} blocks/{} bytes would be exceeded",
                max_blocks,
                max_bytes
            );
        }
        Ok(())
    }

    fn admit_speculative_with_rolling_victim(
        &self,
        block: &Block,
        protected_roots: &[Hash],
        max_blocks: usize,
        max_bytes: usize,
    ) -> anyhow::Result<()> {
        let _journal_guard = self.lock_speculative_journal()?;
        if self.speculative_row_exists(block)? {
            // Block::hash excludes `justify`; the durable journal is
            // first-write-wins even when a later proposal carries a larger QC.
            return Ok(());
        }

        let target_hash = block.hash();
        let target_bytes = serde_json::to_vec(block)?;
        if target_bytes.len() > MAX_SPECULATIVE_BLOCK_BYTES {
            anyhow::bail!(
                "speculative block serialization {} bytes exceeds per-row limit {}",
                target_bytes.len(),
                MAX_SPECULATIVE_BLOCK_BYTES
            );
        }
        let target_manifest =
            serde_json::to_vec(&(block.height, block.parent, target_bytes.len()))?;
        let target_usage = target_bytes
            .len()
            .saturating_add(SPECULATIVE_MANIFEST_PREFIX.len() + target_hash.len())
            .saturating_add(target_manifest.len());

        let mut rows: HashMap<Hash, (Block, usize)> = HashMap::new();
        let mut orphan_manifests = Vec::new();
        let prefix = SPECULATIVE_MANIFEST_PREFIX;
        let mut iter = self.db.iterator_cf(
            self.cf(CF_META),
            rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward),
        );
        while let Some(item) = iter.next() {
            let (key, manifest_bytes) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            let hash_bytes = &key[prefix.len()..];
            if hash_bytes.len() != 32 {
                anyhow::bail!("speculative manifest key has invalid hash length");
            }
            let hash: Hash = hash_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("speculative manifest hash has invalid length"))?;
            Self::decode_speculative_manifest(&hash, &manifest_bytes)?;
            let Some(raw_bytes) = self.db.get_cf(self.cf(CF_BLOCKS), &hash)? else {
                orphan_manifests.push(hash);
                continue;
            };
            let (candidate, row_bytes) =
                Self::validate_speculative_row(&hash, &raw_bytes, &manifest_bytes)?;
            let canonical = self
                .db
                .get_cf(self.cf(CF_HEIGHT_INDEX), candidate.height.to_be_bytes())?
                .is_some_and(|canonical_hash| canonical_hash.as_slice() == hash.as_slice());
            if canonical {
                continue;
            }
            rows.insert(
                hash,
                (
                    candidate,
                    row_bytes
                        .saturating_add(key.len())
                        .saturating_add(manifest_bytes.len()),
                ),
            );
        }

        let protected = speculative_ancestor_closure(
            &rows
                .iter()
                .map(|(hash, (candidate, _))| (*hash, candidate.clone()))
                .collect(),
            protected_roots,
        );
        let total_bytes: usize = rows.values().map(|(_, bytes)| *bytes).sum();
        let fits = |removed: &HashSet<Hash>| {
            rows.len().saturating_sub(removed.len()).saturating_add(1) <= max_blocks
                && total_bytes
                    .saturating_sub(
                        removed
                            .iter()
                            .map(|hash| rows.get(hash).map(|(_, bytes)| *bytes).unwrap_or_default())
                            .sum(),
                    )
                    .saturating_add(target_usage)
                    <= max_bytes
        };

        let mut victims = HashSet::new();
        if !fits(&victims) {
            let committed_hash = self
                .db
                .get_cf(self.cf(CF_META), b"committed_hash")?
                .and_then(|bytes| bytes.as_slice().try_into().ok());
            let blocks: HashMap<Hash, Block> = rows
                .iter()
                .map(|(hash, (candidate, _))| (*hash, candidate.clone()))
                .collect();
            let mut candidates: Vec<Hash> = rows
                .iter()
                .filter_map(|(hash, (candidate, _))| {
                    let sibling = block.justify.is_some()
                        && candidate.parent == block.parent
                        && candidate.view < block.view;
                    let disconnected = committed_hash
                        .map(|committed| !reaches_ancestor(&blocks, *hash, committed))
                        .unwrap_or(false);
                    (sibling || disconnected).then_some(*hash)
                })
                .filter(|hash| !protected.contains(hash))
                .collect();
            candidates.sort_by(|left, right| {
                let left_block = &blocks[left];
                let right_block = &blocks[right];
                let left_sibling = block.justify.is_some()
                    && left_block.parent == block.parent
                    && left_block.view < block.view;
                let right_sibling = block.justify.is_some()
                    && right_block.parent == block.parent
                    && right_block.view < block.view;
                right_sibling
                    .cmp(&left_sibling)
                    .then_with(|| right_block.view.cmp(&left_block.view))
                    .then_with(|| left.cmp(right))
            });
            for root in candidates {
                let branch: HashSet<Hash> = rows
                    .keys()
                    .copied()
                    .filter(|hash| reaches_ancestor(&blocks, *hash, root))
                    .collect();
                if branch.is_empty() || branch.iter().any(|hash| protected.contains(hash)) {
                    continue;
                }
                victims = branch;
                if fits(&victims) {
                    break;
                }
                victims.clear();
            }
        }
        if !fits(&victims) {
            anyhow::bail!(
                "protected speculative branches exceed {} blocks/{} bytes",
                max_blocks,
                max_bytes
            );
        }

        // Keep canonical height/index/artifact/root metadata untouched.  Only
        // speculative hash rows and their admission manifests participate in
        // this rolling transition; the synced batch is crash-safe and retryable.
        let mut batch = WriteBatch::default();
        for hash in orphan_manifests {
            batch.delete_cf(self.cf(CF_META), speculative_manifest_key(&hash));
        }
        for hash in victims {
            batch.delete_cf(self.cf(CF_BLOCKS), hash);
            batch.delete_cf(self.cf(CF_META), speculative_manifest_key(&hash));
        }
        batch.put_cf(self.cf(CF_BLOCKS), target_hash, &target_bytes);
        batch.put_cf(
            self.cf(CF_META),
            speculative_manifest_key(&target_hash),
            &target_manifest,
        );
        self.db.write_opt(batch, &Self::sync_write_options())?;
        Ok(())
    }

    fn prune_speculative(
        &self,
        protected_roots: &[Hash],
        max_blocks: usize,
        max_bytes: usize,
    ) -> anyhow::Result<()> {
        let _journal_guard = self.lock_speculative_journal()?;
        let mut blocks = HashMap::new();
        let mut block_bytes = HashMap::new();
        let mut canonical = HashSet::new();
        let mut orphan_manifests = Vec::new();
        let prefix = SPECULATIVE_MANIFEST_PREFIX;
        let mut iter = self.db.iterator_cf(
            self.cf(CF_META),
            rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward),
        );
        while let Some(item) = iter.next() {
            let (key, manifest_bytes) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            let hash_bytes = &key[prefix.len()..];
            if hash_bytes.len() != 32 {
                anyhow::bail!("speculative manifest key has invalid hash length");
            }
            let hash: Hash = hash_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("speculative manifest hash has invalid length"))?;
            // Decode the manifest before checking the body.  A malformed
            // manifest must fail closed even when the body is already gone;
            // only a well-formed manifest with a missing body is recoverable
            // as an orphan journal entry.
            Self::decode_speculative_manifest(&hash, &manifest_bytes)?;
            let Some(raw_bytes) = self.db.get_cf(self.cf(CF_BLOCKS), &hash)? else {
                orphan_manifests.push(hash);
                continue;
            };
            let (block, bytes) =
                Self::validate_speculative_row(&hash, &raw_bytes, &manifest_bytes)?;
            let height = block.height;
            blocks.insert(hash, block);
            // Account for both the block row and its manifest row so the
            // fixed budget bounds the actual speculative journal footprint,
            // not only the decoded payload value.
            block_bytes.insert(
                hash,
                bytes
                    .saturating_add(key.len())
                    .saturating_add(manifest_bytes.len()),
            );
            if self
                .db
                .get_cf(self.cf(CF_HEIGHT_INDEX), height.to_be_bytes())?
                .is_some_and(|canonical_hash| canonical_hash.as_slice() == hash.as_slice())
            {
                canonical.insert(hash);
            }
        }

        let committed_hash = self
            .db
            .get_cf(self.cf(CF_META), b"committed_hash")?
            .and_then(|bytes| bytes.as_slice().try_into().ok());
        let speculative: HashSet<Hash> = blocks
            .keys()
            .copied()
            .filter(|hash| !canonical.contains(hash))
            .collect();
        let total_bytes: usize = speculative
            .iter()
            .map(|hash| block_bytes.get(hash).copied().unwrap_or_default())
            .sum();
        let under_budget = speculative.len() <= max_blocks && total_bytes <= max_bytes;

        let protected = speculative_ancestor_closure(&blocks, protected_roots);
        let mut eligible: Vec<Hash> = speculative
            .iter()
            .copied()
            .filter(|hash| {
                Some(*hash) != committed_hash
                    && !protected.contains(hash)
                    && committed_hash
                        .map(|committed| !reaches_ancestor(&blocks, *hash, committed))
                        .unwrap_or(false)
            })
            .collect();
        eligible.sort_by_key(|hash| {
            (
                blocks
                    .get(hash)
                    .map(|block| block.height)
                    .unwrap_or(u64::MAX),
                *hash,
            )
        });

        let mut removed = HashSet::new();
        let mut remaining = speculative.len();
        let mut remaining_bytes = total_bytes;
        for root in eligible {
            if !under_budget && remaining <= max_blocks && remaining_bytes <= max_bytes {
                break;
            }
            let branch: HashSet<Hash> = speculative
                .iter()
                .copied()
                .filter(|hash| {
                    Some(*hash) != committed_hash
                        && !protected.contains(hash)
                        && committed_hash
                            .map(|committed| !reaches_ancestor(&blocks, *hash, committed))
                            .unwrap_or(false)
                        && reaches_ancestor(&blocks, *hash, root)
                })
                .collect();
            if branch.is_empty() {
                continue;
            }
            remaining = remaining.saturating_sub(branch.len());
            remaining_bytes = remaining_bytes.saturating_sub(
                branch
                    .iter()
                    .map(|hash| block_bytes.get(hash).copied().unwrap_or_default())
                    .sum(),
            );
            removed.extend(branch);
        }
        if remaining > max_blocks || remaining_bytes > max_bytes {
            anyhow::bail!(
                "protected speculative branches exceed {} blocks/{} bytes",
                max_blocks,
                max_bytes
            );
        }
        if removed.is_empty() && orphan_manifests.is_empty() {
            return Ok(());
        }

        // This batch deliberately contains no height-index or committed-meta
        // deletes.  A branch is removed only as whole unfinalized hash rows;
        // an orphan removes only its manifest.  The canonical recovery index
        // remains authoritative after a crash, and retrying this batch is
        // idempotent if the process dies before the write is acknowledged.
        let mut batch = WriteBatch::default();
        for hash in orphan_manifests {
            batch.delete_cf(self.cf(CF_META), speculative_manifest_key(&hash));
        }
        for hash in removed {
            batch.delete_cf(self.cf(CF_BLOCKS), hash);
            batch.delete_cf(self.cf(CF_BLOCK_ARTIFACTS), hash);
            batch.delete_cf(self.cf(CF_STATE_ROOTS), hash);
            batch.delete_cf(self.cf(CF_META), speculative_manifest_key(&hash));
        }
        self.db.write(batch)?;
        Ok(())
    }

    fn get(&self, hash: &Hash) -> Option<Block> {
        let bytes = self.db.get_cf(self.cf(CF_BLOCKS), hash).ok()??;
        serde_json::from_slice(&bytes).ok()
    }

    fn get_by_height(&self, height: u64) -> Option<Block> {
        let hash_bytes = self
            .db
            .get_cf(self.cf(CF_HEIGHT_INDEX), height.to_be_bytes())
            .ok()??;

        let mut hash = [0u8; 32];
        if hash_bytes.len() == 32 {
            hash.copy_from_slice(&hash_bytes);
            self.get(&hash)
        } else {
            None
        }
    }

    fn set_committed(&self, hash: &Hash) {
        // This is now handled by commit_block() for atomicity
        // Just update meta for backwards compatibility
        let _journal_guard = self
            .lock_speculative_journal()
            .expect("speculative journal lock must not be poisoned");
        let options = Self::sync_write_options();
        let _ = self
            .db
            .put_cf_opt(self.cf(CF_META), b"committed_hash", hash, &options);
    }

    fn get_committed_head(&self) -> Option<Block> {
        let hash_bytes = self.db.get_cf(self.cf(CF_META), b"committed_hash").ok()??;

        let mut hash = [0u8; 32];
        if hash_bytes.len() == 32 {
            hash.copy_from_slice(&hash_bytes);
            self.get(&hash)
        } else {
            None
        }
    }
}

impl PersistentStore for RocksDbStore {
    fn equivocation_journal_capability(&self) -> EquivocationJournalCapability {
        EquivocationJournalCapability::supported()
    }

    fn save_block(&self, block: &Block) -> anyhow::Result<()> {
        self.save_speculative_row(block)
    }

    fn save_consensus_state(&self, state: &ConsensusState) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(state)?;
        let options = Self::sync_write_options();
        self.db
            .put_cf_opt(self.cf(CF_CONSENSUS), b"state", &bytes, &options)?;
        Ok(())
    }

    fn load_consensus_state(&self) -> anyhow::Result<Option<ConsensusState>> {
        match self.db.get_cf(self.cf(CF_CONSENSUS), b"state")? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    fn save_equivocation_proof(&self, proof: &EquivocationProof) -> anyhow::Result<()> {
        self.save_equivocation_proof_row(proof)
    }

    fn load_equivocation_proofs(&self) -> anyhow::Result<Vec<EquivocationProof>> {
        self.load_equivocation_proofs_rows()
    }

    fn delete_equivocation_proof(&self, proof: &EquivocationProof) -> anyhow::Result<()> {
        self.delete_equivocation_proof_row(proof)
    }

    fn save_epoch_transition_proof(&self, proof: &EpochTransitionProof) -> anyhow::Result<()> {
        self.save_epoch_transition_proof_row(proof)
    }

    fn load_epoch_transition_proof(&self) -> anyhow::Result<Option<EpochTransitionProof>> {
        self.load_epoch_transition_proof_row()
    }

    fn clear_epoch_transition_proof(&self) -> anyhow::Result<()> {
        self.clear_epoch_transition_proof_row()
    }

    fn load_committed_height(&self) -> anyhow::Result<Option<u64>> {
        let Some(bytes) = self.db.get_cf(self.cf(CF_META), b"committed_height")? else {
            return Ok(None);
        };
        let bytes: [u8; 8] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("committed height metadata has invalid length"))?;
        Ok(Some(u64::from_be_bytes(bytes)))
    }

    fn save_snapshot(&self, height: u64, snapshot: &AppSnapshot) -> anyhow::Result<()> {
        let bytes = snapshot
            .to_bounded_json()
            .map_err(|error| anyhow::anyhow!("invalid app snapshot: {error}"))?;
        self.db
            .put_cf(self.cf(CF_SNAPSHOTS), height.to_be_bytes(), &bytes)?;
        tracing::info!(height, bytes = bytes.len(), "Saved app snapshot");
        Ok(())
    }

    fn load_latest_snapshot(
        &self,
        before_height: u64,
    ) -> anyhow::Result<Option<(u64, AppSnapshot)>> {
        // Iterate backwards from before_height to find latest snapshot
        let cf = self.cf(CF_SNAPSHOTS);

        // Use iterator in reverse from the target height
        let iter = self.db.iterator_cf(
            cf,
            rocksdb::IteratorMode::From(&before_height.to_be_bytes(), rocksdb::Direction::Reverse),
        );

        for item in iter {
            let (key, value) = item?;
            if key.len() == 8 {
                let height = u64::from_be_bytes(key[..8].try_into().unwrap());
                if height <= before_height {
                    let snapshot = AppSnapshot::from_bounded_json(&value)
                        .map_err(|error| anyhow::anyhow!("invalid stored app snapshot: {error}"))?;
                    return Ok(Some((height, snapshot)));
                }
            }
        }

        Ok(None)
    }

    fn load_latest_snapshot_height(&self, before_height: u64) -> anyhow::Result<Option<u64>> {
        let cf = self.cf(CF_SNAPSHOTS);
        let iter = self.db.iterator_cf(
            cf,
            rocksdb::IteratorMode::From(&before_height.to_be_bytes(), rocksdb::Direction::Reverse),
        );

        for item in iter {
            let (key, _) = item?;
            if key.len() == 8 {
                let height = u64::from_be_bytes(
                    key[..8]
                        .try_into()
                        .expect("snapshot key length checked above"),
                );
                if height <= before_height {
                    return Ok(Some(height));
                }
            }
        }

        Ok(None)
    }

    fn blocks_from_height(&self, from_height: u64) -> anyhow::Result<Vec<Block>> {
        let mut blocks = Vec::new();
        let cf = self.cf(CF_HEIGHT_INDEX);

        let iter = self.db.iterator_cf(
            cf,
            rocksdb::IteratorMode::From(&from_height.to_be_bytes(), rocksdb::Direction::Forward),
        );

        for item in iter {
            let (key, hash_bytes) = item?;
            if key.len() == 8 && hash_bytes.len() == 32 {
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&hash_bytes);
                if let Some(block) = self.get(&hash) {
                    blocks.push(block);
                }
            }
        }

        // Sort by height to ensure order
        blocks.sort_by_key(|b| b.height);
        Ok(blocks)
    }

    fn commit_block(&self, block: &Block, state: &ConsensusState) -> anyhow::Result<()> {
        Self::validate_authenticated_commitment(block, None)?;
        self.commit_block_with_artifacts(block, state, None)
    }

    fn commit_block_with_commitment(
        &self,
        block: &Block,
        state: &ConsensusState,
        commitment: Option<&CommitmentV2>,
    ) -> anyhow::Result<()> {
        if block.height > 0 {
            return self.commit_block_with_commitment_and_state_root(
                block,
                state,
                commitment,
                Some(&block.app_hash),
            );
        }
        Self::validate_authenticated_commitment(block, commitment)?;
        let bytes = commitment.map(CommitmentV2::canonical_bytes).transpose()?;
        self.commit_block_with_artifacts(block, state, bytes.as_deref())
    }

    fn commit_block_with_artifacts(
        &self,
        block: &Block,
        state: &ConsensusState,
        artifacts: Option<&[u8]>,
    ) -> anyhow::Result<()> {
        if block.height > 0 {
            let commitment = artifacts
                .map(CommitmentV2::from_canonical_bytes)
                .transpose()?;
            return self.commit_block_with_commitment_and_state_root(
                block,
                state,
                commitment.as_ref(),
                Some(&block.app_hash),
            );
        }
        let _journal_guard = self.lock_speculative_journal()?;
        let hash = block.hash();

        // This is the storage boundary for finalized state.  Never write a
        // block and metadata that disagree: a caller that has not advanced
        // its committed fields yet must fail closed instead of creating a
        // store that appears committed after restart.
        if state.context() != block.context() {
            anyhow::bail!("cannot commit block with mismatched consensus context");
        }
        if state.committed_height != block.height {
            anyhow::bail!(
                "committed height {} does not match block height {}",
                state.committed_height,
                block.height
            );
        }
        if state.committed_hash != hash {
            anyhow::bail!("committed hash does not match block hash");
        }
        if let Some(qc) = &state.high_qc {
            if qc.context() != block.context() {
                anyhow::bail!("high QC context does not match committed block");
            }
        }
        if let Some(qc) = &state.locked_qc {
            if qc.context() != block.context() {
                anyhow::bail!("locked QC context does not match committed block");
            }
        }

        // A finalized height is anchored by its exact previous committed
        // block, not by height alone.  The genesis commit is the only valid
        // write without an existing committed head; retries of an already
        // durable commit are idempotent.
        if let Some(existing_head) = self.get_committed_head() {
            if existing_head.hash() != hash
                && (block.parent != existing_head.hash()
                    || block.height != existing_head.height.saturating_add(1))
            {
                anyhow::bail!(
                    "committed block does not extend the exact committed head at height {}",
                    existing_head.height
                );
            }
        } else if block.height != 0 || block.parent != [0u8; 32] {
            anyhow::bail!("first committed block must be the canonical genesis block");
        } else if block.hash() != Block::genesis(block.context()).hash() {
            anyhow::bail!("first committed block is not the canonical genesis block");
        }

        // Never replace a canonical height with a different block.  This is
        // also useful when a retry repeats an already successful commit.
        if let Some(existing) = self.get_by_height(block.height) {
            if existing.hash() != hash {
                anyhow::bail!(
                    "height {} is already finalized by another block",
                    block.height
                );
            }
        }

        // A block's canonical artifacts are immutable. A retry may supply the
        // same bytes, but it may not silently replace a bundle that an indexer
        // could already have consumed. Missing bytes are valid only for genesis.
        if let Some(artifacts) = artifacts {
            if let Some(existing) = self.load_block_artifacts(&hash)? {
                if existing != artifacts {
                    anyhow::bail!(
                        "execution artifacts for block {} already exist with different bytes",
                        hex::encode(hash)
                    );
                }
            }
        }
        // Genesis has no authenticated receipt root, so this is empty on the
        // only path that reaches this branch.  Keep the call explicit so a
        // future protocol version cannot accidentally bypass the index check.
        let transaction_receipt_rows = self.transaction_receipt_index_rows(block, None)?;

        let mut batch = WriteBatch::default();
        let block_bytes = serde_json::to_vec(block)?;
        let state_bytes = serde_json::to_vec(state)?;

        // Block by hash
        batch.put_cf(self.cf(CF_BLOCKS), hash, &block_bytes);
        batch.delete_cf(self.cf(CF_META), speculative_manifest_key(&hash));

        // Height -> hash index
        batch.put_cf(self.cf(CF_HEIGHT_INDEX), block.height.to_be_bytes(), hash);

        // Consensus state
        batch.put_cf(self.cf(CF_CONSENSUS), b"state", &state_bytes);

        // Meta: committed height and hash
        batch.put_cf(
            self.cf(CF_META),
            b"committed_height",
            state.committed_height.to_be_bytes(),
        );
        batch.put_cf(self.cf(CF_META), b"committed_hash", &state.committed_hash);

        // Keep artifact persistence inside the same batch. `None` is valid
        // only for genesis; non-genesis callers were rejected above.
        if let Some(artifacts) = artifacts {
            batch.put_cf(self.cf(CF_BLOCK_ARTIFACTS), hash, artifacts);
        }
        for (tx_id, row) in transaction_receipt_rows {
            batch.put_cf(self.cf(CF_TRANSACTION_RECEIPTS), tx_id, row);
        }

        // Atomic write
        let options = Self::sync_write_options();
        self.db.write_opt(batch, &options)?;

        Ok(())
    }

    fn commit_block_with_commitment_and_state_root(
        &self,
        block: &Block,
        state: &ConsensusState,
        commitment: Option<&CommitmentV2>,
        state_root: Option<&Hash>,
    ) -> anyhow::Result<()> {
        let _journal_guard = self.lock_speculative_journal()?;
        if block.height > 0 && state_root.is_none() {
            anyhow::bail!(
                "non-genesis block {} is missing its authenticated full-state root",
                block.height
            );
        }
        let root_record = state_root.map(|root| StateRootRecord::new(*root));
        let root_bytes = root_record
            .as_ref()
            .map(StateRootRecord::canonical_bytes)
            .transpose()?;
        let hash = block.hash();

        // Validate root immutability before delegating to the existing
        // finalized write path.  The actual block/consensus/commitment/root
        // write is assembled below as one synced batch.
        if let Some(root) = state_root {
            if block.height > 0 && *root != block.app_hash {
                anyhow::bail!(
                    "state root does not match the authenticated block app hash at height {}",
                    block.height
                );
            }
            if let Some(existing) = self.db.get_cf(self.cf(CF_STATE_ROOTS), hash)? {
                let existing_record =
                    StateRootRecord::from_canonical_bytes(&existing).map_err(|error| {
                        anyhow::anyhow!("invalid existing state-root record: {error}")
                    })?;
                if existing_record.root != *root {
                    anyhow::bail!(
                        "state root for block {} already exists with different bytes",
                        hex::encode(hash)
                    );
                }
            }
        }
        Self::validate_authenticated_commitment(block, commitment)?;

        // Keep all validation and idempotence checks in the existing method.
        // For a state-root commit we need the root in the same WriteBatch, so
        // repeat the canonical checks and construct the complete batch here.
        if state.context() != block.context() {
            anyhow::bail!("cannot commit block with mismatched consensus context");
        }
        if state.committed_height != block.height {
            anyhow::bail!(
                "committed height {} does not match block height {}",
                state.committed_height,
                block.height
            );
        }
        if state.committed_hash != hash {
            anyhow::bail!("committed hash does not match block hash");
        }
        if let Some(qc) = &state.high_qc {
            if qc.context() != block.context() {
                anyhow::bail!("high QC context does not match committed block");
            }
        }
        if let Some(qc) = &state.locked_qc {
            if qc.context() != block.context() {
                anyhow::bail!("locked QC context does not match committed block");
            }
        }
        if let Some(existing_head) = self.get_committed_head() {
            if existing_head.hash() != hash
                && (block.parent != existing_head.hash()
                    || block.height != existing_head.height.saturating_add(1))
            {
                anyhow::bail!(
                    "committed block does not extend the exact committed head at height {}",
                    existing_head.height
                );
            }
        } else if block.height != 0 || block.parent != [0u8; 32] {
            anyhow::bail!("first committed block must be the canonical genesis block");
        } else if block.hash() != Block::genesis(block.context()).hash() {
            anyhow::bail!("first committed block is not the canonical genesis block");
        }
        if let Some(existing) = self.get_by_height(block.height) {
            if existing.hash() != hash {
                anyhow::bail!(
                    "height {} is already finalized by another block",
                    block.height
                );
            }
        }

        let commitment_bytes = commitment.map(CommitmentV2::canonical_bytes).transpose()?;
        if let Some(bytes) = commitment_bytes.as_deref() {
            if let Some(existing) = self.load_block_artifacts(&hash)? {
                if existing != bytes {
                    anyhow::bail!(
                        "execution artifacts for block {} already exist with different bytes",
                        hex::encode(hash)
                    );
                }
            }
        }
        let transaction_receipt_rows = self.transaction_receipt_index_rows(block, commitment)?;

        let mut batch = WriteBatch::default();
        let block_bytes = serde_json::to_vec(block)?;
        let state_bytes = serde_json::to_vec(state)?;
        batch.put_cf(self.cf(CF_BLOCKS), hash, &block_bytes);
        batch.delete_cf(self.cf(CF_META), speculative_manifest_key(&hash));
        batch.put_cf(self.cf(CF_HEIGHT_INDEX), block.height.to_be_bytes(), hash);
        batch.put_cf(self.cf(CF_CONSENSUS), b"state", &state_bytes);
        batch.put_cf(
            self.cf(CF_META),
            b"committed_height",
            state.committed_height.to_be_bytes(),
        );
        batch.put_cf(self.cf(CF_META), b"committed_hash", &state.committed_hash);
        if let Some(bytes) = commitment_bytes.as_deref() {
            batch.put_cf(self.cf(CF_BLOCK_ARTIFACTS), hash, bytes);
        }
        if let Some(root) = root_bytes.as_deref() {
            batch.put_cf(self.cf(CF_STATE_ROOTS), hash, root);
        }
        for (tx_id, row) in transaction_receipt_rows {
            batch.put_cf(self.cf(CF_TRANSACTION_RECEIPTS), tx_id, row);
        }
        self.db.write_opt(batch, &Self::sync_write_options())?;
        Ok(())
    }

    fn save_candles_batch(&self, entries: &[(Vec<u8>, Vec<u8>)]) -> anyhow::Result<()> {
        RocksDbStore::save_candles_batch(self, entries)
    }

    fn load_candles(
        &self,
        symbol: &str,
        interval_str: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Candle>> {
        RocksDbStore::load_candles(self, symbol, interval_str, limit)
    }

    fn load_block_artifacts(&self, hash: &Hash) -> anyhow::Result<Option<Vec<u8>>> {
        RocksDbStore::load_block_artifacts(self, hash)
    }

    fn load_block_artifacts_by_height(&self, height: u64) -> anyhow::Result<Option<Vec<u8>>> {
        RocksDbStore::load_block_artifacts_by_height(self, height)
    }

    fn load_transaction_receipt(
        &self,
        tx_id: &Hash,
    ) -> anyhow::Result<Option<TransactionReceiptLookup>> {
        RocksDbStore::load_transaction_receipt(self, tx_id)
    }

    fn load_state_root(&self, hash: &Hash) -> anyhow::Result<Option<Hash>> {
        RocksDbStore::load_state_root(self, hash)
    }

    fn load_state_root_by_height(&self, height: u64) -> anyhow::Result<Option<Hash>> {
        RocksDbStore::load_state_root_by_height(self, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{SignatureScheme, SignedEnvelope, Transaction};
    use crate::consensus::{form_certificate, EpochTransitionProof};
    use crate::crypto::bls::BlsSecretKey;
    use crate::storage::snapshot::MAX_APP_SNAPSHOT_BYTES;
    use crate::types::{
        Certificate, ConsensusConfig, ConsensusContext, ResourceUsage, TransactionReceipt,
        TransactionType, Vote,
    };
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    fn create_test_store() -> (RocksDbStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();
        (store, dir)
    }

    fn epoch_transition_proof() -> EpochTransitionProof {
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = [0x11; 32];
        let secret = config
            .bls_secret_key()
            .unwrap_or_else(|| BlsSecretKey::from_seed(&[42u8; 32]));
        let committee = config.committee().unwrap();
        let context = config.context().unwrap();
        let block = Block {
            epoch: 0,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 7,
            height: 42,
            parent: [0x22; 32],
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: [0x33; 32],
            app_hash: [0x44; 32],
            timestamp: 123,
            justify: None,
        };
        let vote = Vote::new_bls(
            context,
            block.view,
            block.hash(),
            block.app_hash,
            config.node_id,
            &secret,
        );
        let qc = form_certificate(&committee, context, vec![vote], true).unwrap();
        let update = crate::app::staking::ValidatorSetUpdate {
            node_ids: vec![config.node_id],
            bls_pubkeys: vec![secret.public_key().to_bytes().to_vec()],
            stakes: vec![(config.node_id, 2)],
        };
        EpochTransitionProof::from_validator_set_update(
            context,
            qc,
            &block,
            block.app_hash,
            &update,
        )
        .unwrap()
    }

    #[test]
    fn epoch_transition_marker_survives_restart_and_is_explicitly_clearable() {
        let (store, dir) = create_test_store();
        let proof = epoch_transition_proof();
        store.save_epoch_transition_proof(&proof).unwrap();
        store.save_epoch_transition_proof(&proof).unwrap();
        assert_eq!(
            store.load_epoch_transition_proof().unwrap(),
            Some(proof.clone())
        );
        drop(store);

        let reopened = RocksDbStore::open(dir.path()).unwrap();
        assert_eq!(reopened.load_epoch_transition_proof().unwrap(), Some(proof));
        reopened.clear_epoch_transition_proof().unwrap();
        assert_eq!(reopened.load_epoch_transition_proof().unwrap(), None);
    }

    #[test]
    fn malformed_epoch_transition_marker_fails_closed() {
        let (store, _dir) = create_test_store();
        store
            .db
            .put_cf(
                store.cf(CF_EPOCH_TRANSITIONS),
                EPOCH_TRANSITION_MARKER_KEY,
                b"not-a-proof",
            )
            .unwrap();
        assert!(store.load_epoch_transition_proof().is_err());
    }

    fn equivocation_proof(
        epoch: u64,
        committee_marker: u8,
        offender_marker: u8,
        hash_a_marker: u8,
        hash_b_marker: u8,
    ) -> EquivocationProof {
        EquivocationProof {
            context: ConsensusContext::with_genesis(epoch, [committee_marker; 32], [0xabu8; 32]),
            offender: [offender_marker; 32],
            view: 7,
            hash_a: [hash_a_marker; 32],
            app_hash_a: [hash_a_marker.wrapping_add(0x10); 32],
            hash_b: [hash_b_marker; 32],
            app_hash_b: [hash_b_marker.wrapping_add(0x10); 32],
            signature_a: vec![
                hash_a_marker;
                crate::consensus::equivocation::EQUIVOCATION_SIGNATURE_BYTES
            ],
            signature_b: vec![
                hash_b_marker;
                crate::consensus::equivocation::EQUIVOCATION_SIGNATURE_BYTES
            ],
        }
    }

    #[test]
    fn equivocation_proof_journal_saves_loads_and_reopens() {
        let dir = TempDir::new().unwrap();
        let proof = equivocation_proof(1, 1, 2, 2, 1);
        let canonical = proof.canonicalized().unwrap();
        {
            let store = RocksDbStore::open(dir.path()).unwrap();
            store.save_equivocation_proof(&proof).unwrap();
            assert_eq!(
                store.load_equivocation_proofs().unwrap(),
                vec![canonical.clone()]
            );
        }

        let reopened = RocksDbStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.load_equivocation_proofs().unwrap(),
            vec![canonical]
        );
    }

    #[test]
    fn equivocation_proof_journal_is_first_write_wins_per_context_and_offender() {
        let (store, _dir) = create_test_store();
        let first = equivocation_proof(1, 1, 2, 1, 2);
        let replacement = equivocation_proof(1, 1, 2, 3, 4);

        store.save_equivocation_proof(&first).unwrap();
        store.save_equivocation_proof(&replacement).unwrap();
        store.save_equivocation_proof(&first).unwrap();

        assert_eq!(
            store.load_equivocation_proofs().unwrap(),
            vec![first.canonicalized().unwrap()]
        );
    }

    #[test]
    fn equivocation_proof_journal_enforces_record_cap() {
        let (store, _dir) = create_test_store();
        for marker in 0..MAX_EQUIVOCATION_PROOF_RECORDS as u16 {
            let proof = equivocation_proof(1, 1, marker as u8, 1, 2);
            store.save_equivocation_proof(&proof).unwrap();
        }

        let extra = equivocation_proof(2, 2, 0, 1, 2);
        assert!(store.save_equivocation_proof(&extra).is_err());
        assert_eq!(
            store.load_equivocation_proofs().unwrap().len(),
            MAX_EQUIVOCATION_PROOF_RECORDS
        );
    }

    #[test]
    fn equivocation_proof_journal_load_fails_closed_on_malformed_or_oversized_rows() {
        let (store, _dir) = create_test_store();
        let proof = equivocation_proof(1, 1, 2, 1, 2);
        let key = proof.journal_key();

        store
            .db
            .put_cf(store.cf(CF_EQUIVOCATION_PROOFS), &key, b"not-json")
            .unwrap();
        assert!(store.load_equivocation_proofs().is_err());

        store
            .db
            .delete_cf(store.cf(CF_EQUIVOCATION_PROOFS), &key)
            .unwrap();
        let mut malformed: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&proof).unwrap()).unwrap();
        malformed["signature_a"] = serde_json::Value::Array(Vec::new());
        store
            .db
            .put_cf(
                store.cf(CF_EQUIVOCATION_PROOFS),
                &key,
                serde_json::to_vec(&malformed).unwrap(),
            )
            .unwrap();
        assert!(store.load_equivocation_proofs().is_err());

        store
            .db
            .delete_cf(store.cf(CF_EQUIVOCATION_PROOFS), &key)
            .unwrap();
        store
            .db
            .put_cf(
                store.cf(CF_EQUIVOCATION_PROOFS),
                &key,
                vec![0u8; MAX_EQUIVOCATION_PROOF_BYTES + 1],
            )
            .unwrap();
        assert!(store.load_equivocation_proofs().is_err());
    }

    #[test]
    fn equivocation_proof_journal_delete_is_idempotent_and_isolated() {
        let (store, _dir) = create_test_store();
        let first = equivocation_proof(1, 1, 2, 1, 2);
        let second = equivocation_proof(1, 1, 3, 1, 2);
        store
            .db
            .put_cf(store.cf(CF_META), b"journal-isolation", b"untouched")
            .unwrap();
        store.save_equivocation_proof(&first).unwrap();
        store.save_equivocation_proof(&second).unwrap();

        store.delete_equivocation_proof(&first).unwrap();
        store.delete_equivocation_proof(&first).unwrap();
        assert_eq!(
            store.load_equivocation_proofs().unwrap(),
            vec![second.canonicalized().unwrap()]
        );
        assert_eq!(
            store
                .db
                .get_cf(store.cf(CF_META), b"journal-isolation")
                .unwrap()
                .as_deref(),
            Some(&b"untouched"[..])
        );
    }

    fn genesis_state(block: &Block) -> ConsensusState {
        let context = block.context();
        ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: block.height,
            committed_hash: block.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        }
    }

    fn receipt_commitment_fixture(genesis: &Block) -> (Block, ConsensusState, CommitmentV2, Hash) {
        let signer = [1u8; 20];
        let envelope = SignedEnvelope::new(
            genesis.context().genesis_hash,
            signer,
            0,
            0,
            100,
            Transaction::Deposit {
                trader: format!("0x{}", hex::encode(signer)),
                amount: 1,
            },
            SignatureScheme::Dev,
            b"dev".to_vec(),
        )
        .unwrap();
        let entry = ConsensusTransaction::Signed(envelope);
        let tx_id = entry.hash().unwrap();
        let payload = bincode::serialize(&vec![entry]).unwrap();
        let receipt = TransactionReceipt::success(
            0,
            tx_id,
            TransactionType::DEPOSIT,
            ResourceUsage::default(),
            Vec::new(),
        )
        .unwrap();
        let commitment = CommitmentV2::new(vec![receipt]).unwrap();
        let context = genesis.context();
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload,
            proposer: [1u8; 32],
            commitment_root: commitment.root().unwrap(),
            app_hash: [2u8; 32],
            timestamp: 1,
            justify: None,
        };
        let state = ConsensusState {
            committed_height: block.height,
            committed_hash: block.hash(),
            ..genesis_state(genesis)
        };
        (block, state, commitment, tx_id)
    }

    fn system_receipt_fixture(
        parent: &Block,
        height: u64,
        view: u64,
        action: &Transaction,
    ) -> (Block, ConsensusState, CommitmentV2, Hash) {
        let entry = ConsensusTransaction::System(action.clone());
        let tx_id = entry.hash().unwrap();
        let payload = bincode::serialize(&vec![entry]).unwrap();
        let receipt = TransactionReceipt::success(
            0,
            tx_id,
            TransactionType::DEPOSIT,
            ResourceUsage::default(),
            Vec::new(),
        )
        .unwrap();
        let commitment = CommitmentV2::new(vec![receipt]).unwrap();
        let context = parent.context();
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            height,
            parent: parent.hash(),
            payload,
            proposer: [height as u8; 32],
            commitment_root: commitment.root().unwrap(),
            app_hash: [height as u8; 32],
            timestamp: view,
            justify: None,
        };
        let state = ConsensusState {
            committed_height: block.height,
            committed_hash: block.hash(),
            ..genesis_state(parent)
        };
        (block, state, commitment, tx_id)
    }

    fn speculative_fixture(
        context: ConsensusContext,
        parent: Hash,
        height: u64,
        view: u64,
        marker: u8,
    ) -> Block {
        Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            height,
            parent,
            payload: vec![marker],
            proposer: [marker; 32],
            commitment_root: [0u8; 32],
            app_hash: [marker; 32],
            timestamp: view,
            justify: None,
        }
    }

    #[test]
    fn test_block_save_and_get() {
        let (store, _dir) = create_test_store();

        let block = Block::genesis(ConsensusContext::new(0, [7u8; 32]));
        let hash = block.hash();

        store.save(&block);

        let loaded = store.get(&hash).unwrap();
        assert_eq!(loaded.height, block.height);
        assert_eq!(loaded.view, block.view);
    }

    #[test]
    fn test_get_by_height() {
        let (store, _dir) = create_test_store();

        let block = Block::genesis(ConsensusContext::new(0, [7u8; 32]));
        store.save(&block);

        let loaded = store.get_by_height(0).unwrap();
        assert_eq!(loaded.height, 0);
    }

    #[test]
    fn speculative_journal_prune_keeps_canonical_height_index() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        store.save(&genesis);
        store.set_committed(&genesis.hash());

        let mut speculative = Vec::new();
        for view in 1..=5 {
            let block = Block {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view,
                height: 1,
                parent: genesis.hash(),
                payload: vec![view as u8; 64],
                proposer: [view as u8; 32],
                commitment_root: [0u8; 32],
                app_hash: [view as u8; 32],
                timestamp: view,
                justify: None,
            };
            store.save_speculative(&block).unwrap();
            speculative.push(block);
        }

        // Every fork is still connected to the committed genesis, so none
        // may be discarded merely to hit the cap; admission must fail closed.
        assert!(store.prune_speculative(&[], 2, 1024 * 1024).is_err());
        assert_eq!(
            speculative
                .iter()
                .filter(|block| store.get(&block.hash()).is_some())
                .count(),
            speculative.len()
        );
        assert_eq!(store.get_by_height(0).unwrap().hash(), genesis.hash());

        let commitment = CommitmentV2::default();
        let committed = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 100,
            height: 1,
            parent: genesis.hash(),
            payload: vec![],
            proposer: [9u8; 32],
            commitment_root: commitment.root().unwrap(),
            app_hash: [9u8; 32],
            timestamp: 100,
            justify: None,
        };
        let mut state = genesis_state(&genesis);
        state.committed_height = committed.height;
        state.committed_hash = committed.hash();
        store
            .commit_block_with_commitment_and_state_root(
                &committed,
                &state,
                Some(&commitment),
                Some(&committed.app_hash),
            )
            .unwrap();
        store.prune_speculative(&[], 2, 1024 * 1024).unwrap();
        assert_eq!(store.get_by_height(1).unwrap().hash(), committed.hash());
        assert!(
            speculative
                .iter()
                .filter(|block| store.get(&block.hash()).is_some())
                .count()
                <= 2
        );
    }

    #[test]
    fn speculative_journal_gc_reclaims_disconnected_branch_at_exact_cap() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        store.save(&genesis);
        store.set_committed(&genesis.hash());

        let stale = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: vec![1],
            proposer: [1u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        store.save_speculative(&stale).unwrap();

        let commitment = CommitmentV2::default();
        let committed = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 2,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [2u8; 32],
            commitment_root: commitment.root().unwrap(),
            app_hash: [2u8; 32],
            timestamp: 2,
            justify: None,
        };
        let mut state = genesis_state(&genesis);
        state.committed_height = committed.height;
        state.committed_hash = committed.hash();
        store
            .commit_block_with_commitment_and_state_root(
                &committed,
                &state,
                Some(&commitment),
                Some(&committed.app_hash),
            )
            .unwrap();

        // The stale row is the only speculative entry and is already within
        // the requested count cap. It is nevertheless disconnected from the
        // durable head and must be reclaimed now, before a later admission
        // can deadlock on a full journal.
        store.prune_speculative(&[], 1, usize::MAX).unwrap();
        assert!(store.get(&stale.hash()).is_none());
        assert_eq!(store.get_by_height(1).unwrap().hash(), committed.hash());
    }

    #[test]
    fn speculative_hash_reuse_is_first_write_wins_and_canonical_resave_is_noop() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        store.save(&genesis);

        let first = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: vec![1],
            proposer: [1u8; 32],
            commitment_root: [1u8; 32],
            app_hash: [2u8; 32],
            timestamp: 1,
            justify: None,
        };
        store.save_speculative(&first).unwrap();
        let mut larger_justify = first.clone();
        larger_justify.justify = Some(Certificate {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            block_hash: genesis.hash(),
            app_hash: Some(genesis.app_hash),
            votes: Vec::new(),
            voters: vec![[9u8; 32]; 8],
            bls_pubkeys: vec![vec![7u8; 48]; 8],
            agg_signature: vec![5u8; 96],
        });
        assert_eq!(larger_justify.hash(), first.hash());
        assert!(
            serde_json::to_vec(&larger_justify).unwrap().len()
                > serde_json::to_vec(&first).unwrap().len()
        );
        store.save_speculative(&larger_justify).unwrap();
        assert!(store.get(&first.hash()).unwrap().justify.is_none());

        // Once the same hash is canonical, a speculative re-save must not
        // recreate a manifest or replace the canonical body.
        store.save(&first);
        store.save_speculative(&larger_justify).unwrap();
        assert!(store.get(&first.hash()).unwrap().justify.is_none());
        assert_eq!(store.get_by_height(1).unwrap().hash(), first.hash());
    }

    #[test]
    fn speculative_journal_overcap_failure_does_not_delete_partial_victims() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        store.save(&genesis);
        store.set_committed(&genesis.hash());

        let stale = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: vec![1],
            proposer: [1u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        store.save_speculative(&stale).unwrap();

        let commitment = CommitmentV2::default();
        let committed = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 2,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [2u8; 32],
            commitment_root: commitment.root().unwrap(),
            app_hash: [2u8; 32],
            timestamp: 2,
            justify: None,
        };
        let mut state = genesis_state(&genesis);
        state.committed_height = 1;
        state.committed_hash = committed.hash();
        store
            .commit_block_with_commitment_and_state_root(
                &committed,
                &state,
                Some(&commitment),
                Some(&committed.app_hash),
            )
            .unwrap();
        let protected = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 3,
            height: 2,
            parent: committed.hash(),
            payload: vec![3],
            proposer: [3u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [3u8; 32],
            timestamp: 3,
            justify: None,
        };
        store.save_speculative(&protected).unwrap();

        assert!(store
            .prune_speculative(&[protected.hash()], 0, usize::MAX)
            .is_err());
        assert!(store.get(&stale.hash()).is_some());
        assert!(store.get(&protected.hash()).is_some());
        assert_eq!(store.get_by_height(1).unwrap().hash(), committed.hash());
    }

    #[test]
    fn speculative_orphan_manifest_is_atomically_removed_without_touching_canonical_metadata() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        store.save(&genesis);
        store.set_committed(&genesis.hash());

        let orphan_hash = [0x55u8; 32];
        let orphan_height = 9u64;
        let manifest = serde_json::to_vec(&(orphan_height, genesis.hash(), 123usize)).unwrap();
        let manifest_key = speculative_manifest_key(&orphan_hash);
        store
            .db
            .put_cf(store.cf(CF_META), &manifest_key, &manifest)
            .unwrap();
        // A stale canonical index is intentionally left in place: orphan
        // recovery may remove only the manifest admission lock.
        store
            .db
            .put_cf(
                store.cf(CF_HEIGHT_INDEX),
                orphan_height.to_be_bytes(),
                orphan_hash,
            )
            .unwrap();
        let committed_before = store
            .db
            .get_cf(store.cf(CF_META), b"committed_hash")
            .unwrap();

        store.prune_speculative(&[], 0, 0).unwrap();
        assert!(store
            .db
            .get_cf(store.cf(CF_META), &manifest_key)
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .db
                .get_cf(store.cf(CF_HEIGHT_INDEX), orphan_height.to_be_bytes())
                .unwrap(),
            Some(orphan_hash.to_vec())
        );
        assert_eq!(
            store
                .db
                .get_cf(store.cf(CF_META), b"committed_hash")
                .unwrap(),
            committed_before
        );

        // A retry after a crash between scans and acknowledgement is a
        // no-op, proving the cleanup is idempotent.
        store.prune_speculative(&[], 0, 0).unwrap();
    }

    #[test]
    fn speculative_manifest_or_body_corruption_fails_closed_without_deletion() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        let block = speculative_fixture(context, genesis.hash(), 1, 1, 1);
        let hash = block.hash();
        let manifest_key = speculative_manifest_key(&hash);

        store
            .db
            .put_cf(store.cf(CF_META), &manifest_key, b"not a manifest")
            .unwrap();
        let error = store
            .prune_speculative(&[], 1, usize::MAX)
            .expect_err("malformed manifest must fail closed");
        assert!(error.to_string().contains("speculative manifest"));
        assert!(store
            .db
            .get_cf(store.cf(CF_META), &manifest_key)
            .unwrap()
            .is_some());

        let (store, _dir) = create_test_store();
        let manifest = serde_json::to_vec(&(block.height, block.parent, 4usize)).unwrap();
        store
            .db
            .put_cf(store.cf(CF_META), &manifest_key, &manifest)
            .unwrap();
        store
            .db
            .put_cf(store.cf(CF_BLOCKS), hash, b"corrupt body")
            .unwrap();
        let error = store
            .prune_speculative(&[], 1, usize::MAX)
            .expect_err("corrupt body must fail closed");
        assert!(error.to_string().contains("stored block row"));
        assert!(store
            .db
            .get_cf(store.cf(CF_META), &manifest_key)
            .unwrap()
            .is_some());
    }

    #[test]
    fn speculative_capacity_accounts_raw_block_row_bytes() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        let block = speculative_fixture(context, genesis.hash(), 1, 1, 1);
        let hash = block.hash();
        let mut raw_bytes = serde_json::to_vec(&block).unwrap();
        raw_bytes.extend_from_slice(b" \n");
        let manifest = serde_json::to_vec(&(block.height, block.parent, raw_bytes.len())).unwrap();
        let manifest_key = speculative_manifest_key(&hash);
        store
            .db
            .put_cf(store.cf(CF_BLOCKS), hash, &raw_bytes)
            .unwrap();
        store
            .db
            .put_cf(store.cf(CF_META), &manifest_key, &manifest)
            .unwrap();

        let (_, accounted_bytes) = store
            .validate_speculative_manifest(&hash, &manifest)
            .unwrap();
        assert_eq!(accounted_bytes, raw_bytes.len());
        let existing_usage = raw_bytes.len() + manifest_key.len() + manifest.len();
        let target = speculative_fixture(context, genesis.hash(), 1, 2, 2);
        let target_bytes = serde_json::to_vec(&target).unwrap();
        let target_manifest =
            serde_json::to_vec(&(target.height, target.parent, target_bytes.len())).unwrap();
        let target_usage = target_bytes.len()
            + speculative_manifest_key(&target.hash()).len()
            + target_manifest.len();

        assert!(store
            .ensure_speculative_capacity(&target, 2, existing_usage + target_usage - 1,)
            .is_err());
        store
            .ensure_speculative_capacity(&target, 2, existing_usage + target_usage)
            .unwrap();
    }

    #[test]
    fn concurrent_speculative_admission_at_count_cap_allows_only_one_writer() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        let genesis_hash = genesis.hash();
        let store = Arc::new(store);
        for view in 1..=63 {
            store
                .save_speculative(&speculative_fixture(
                    context,
                    genesis_hash,
                    1,
                    view,
                    view as u8,
                ))
                .unwrap();
        }

        let barrier = Arc::new(Barrier::new(3));
        let left_store = Arc::clone(&store);
        let left_barrier = Arc::clone(&barrier);
        let left = std::thread::spawn(move || {
            let block = speculative_fixture(context, genesis_hash, 1, 64, 64);
            left_barrier.wait();
            left_store.save_speculative(&block)
        });
        let right_store = Arc::clone(&store);
        let right_barrier = Arc::clone(&barrier);
        let right = std::thread::spawn(move || {
            let block = speculative_fixture(context, genesis_hash, 1, 65, 65);
            right_barrier.wait();
            right_store.save_speculative(&block)
        });
        barrier.wait();

        let left_result = left.join().unwrap();
        let right_result = right.join().unwrap();
        assert_eq!(left_result.is_ok() as u8 + right_result.is_ok() as u8, 1);
        let persisted = [
            speculative_fixture(context, genesis_hash, 1, 64, 64),
            speculative_fixture(context, genesis_hash, 1, 65, 65),
        ]
        .into_iter()
        .filter(|block| store.get(&block.hash()).is_some())
        .count();
        assert_eq!(persisted, 1);
    }

    #[test]
    fn snapshot_storage_round_trip_uses_bounded_json() {
        let (store, _dir) = create_test_store();
        let snapshot = AppSnapshot::genesis();

        store.save_snapshot(7, &snapshot).unwrap();
        assert_eq!(store.load_latest_snapshot_height(7).unwrap(), Some(7));
        let loaded = store
            .load_latest_snapshot(7)
            .unwrap()
            .expect("snapshot should round-trip");
        assert_eq!(loaded.0, 7);
        assert_eq!(loaded.1.height, snapshot.height);
    }

    #[test]
    fn oversized_stored_snapshot_is_rejected_before_deserialization() {
        let (store, _dir) = create_test_store();
        let bytes = vec![b'{'; MAX_APP_SNAPSHOT_BYTES + 1];
        store
            .db
            .put_cf(store.cf(CF_SNAPSHOTS), 7u64.to_be_bytes(), bytes)
            .unwrap();

        assert_eq!(store.load_latest_snapshot_height(7).unwrap(), Some(7));
        let error = store
            .load_latest_snapshot(7)
            .expect_err("oversized stored bytes must fail closed");
        assert!(error
            .to_string()
            .contains("serialized app snapshot is too large"));
    }

    #[test]
    fn snapshot_height_lookup_does_not_decode_malformed_snapshot_value() {
        let (store, _dir) = create_test_store();
        store
            .db
            .put_cf(
                store.cf(CF_SNAPSHOTS),
                9u64.to_be_bytes(),
                b"not valid snapshot JSON",
            )
            .unwrap();

        assert_eq!(store.load_latest_snapshot_height(9).unwrap(), Some(9));
        assert!(store.load_latest_snapshot(9).is_err());
    }

    #[test]
    fn committed_artifacts_round_trip_by_hash_height_and_restart() {
        let dir = TempDir::new().unwrap();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let state = genesis_state(&block);
        let artifacts = br#"{"version":1,"receipts":[],"events":[]}"#;

        {
            let store = RocksDbStore::open(dir.path()).unwrap();
            store
                .commit_block_with_artifacts(&block, &state, Some(artifacts))
                .unwrap();

            assert_eq!(
                store.load_block_artifacts(&block.hash()).unwrap(),
                Some(artifacts.to_vec())
            );
            assert_eq!(
                store.load_block_artifacts_by_height(block.height).unwrap(),
                Some(artifacts.to_vec())
            );
        }

        // Reopening the database must expose the exact same bytes, not a
        // reconstructed or default artifact bundle.
        let reopened = RocksDbStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.load_block_artifacts(&block.hash()).unwrap(),
            Some(artifacts.to_vec())
        );
        assert_eq!(
            reopened
                .load_block_artifacts_by_height(block.height)
                .unwrap(),
            Some(artifacts.to_vec())
        );
    }

    #[test]
    fn commitment_v2_is_stored_as_validated_canonical_bytes() {
        let dir = TempDir::new().unwrap();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let state = genesis_state(&block);
        let commitment = CommitmentV2::new(Vec::new()).unwrap();
        let canonical = commitment.canonical_bytes().unwrap();

        let store = RocksDbStore::open(dir.path()).unwrap();
        store
            .commit_block_with_commitment(&block, &state, Some(&commitment))
            .unwrap();

        assert_eq!(
            store.load_block_artifacts(&block.hash()).unwrap(),
            Some(canonical)
        );
        assert_eq!(
            store.load_commitment(&block.hash()).unwrap(),
            Some(commitment.clone())
        );
        assert_eq!(
            store.load_commitment_by_height(block.height).unwrap(),
            Some(commitment)
        );
    }

    #[test]
    fn state_root_commit_is_atomic_round_trip_and_restart_stable() {
        let dir = TempDir::new().unwrap();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let state = genesis_state(&block);
        let commitment = CommitmentV2::new(Vec::new()).unwrap();
        let root = [0xabu8; 32];

        {
            let store = RocksDbStore::open(dir.path()).unwrap();
            store
                .commit_block_with_commitment_and_state_root(
                    &block,
                    &state,
                    Some(&commitment),
                    Some(&root),
                )
                .unwrap();
            let stored_bytes = store
                .db
                .get_cf(store.cf(CF_STATE_ROOTS), block.hash())
                .unwrap()
                .expect("state-root record should be persisted");
            assert_eq!(
                stored_bytes,
                StateRootRecord::new(root)
                    .canonical_bytes()
                    .expect("state-root record encodes")
            );
            assert_eq!(store.load_state_root(&block.hash()).unwrap(), Some(root));
            assert_eq!(
                store.load_state_root_by_height(block.height).unwrap(),
                Some(root)
            );
            assert_eq!(
                store.load_commitment(&block.hash()).unwrap(),
                Some(commitment)
            );
            assert_eq!(store.get_committed_head().unwrap().hash(), block.hash());
        }

        let reopened = RocksDbStore::open(dir.path()).unwrap();
        assert_eq!(reopened.load_state_root(&block.hash()).unwrap(), Some(root));
        assert_eq!(
            reopened.load_state_root_by_height(block.height).unwrap(),
            Some(root)
        );
    }

    #[test]
    fn transaction_receipt_index_round_trips_by_id_and_restart() {
        let dir = TempDir::new().unwrap();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        let (block, state, commitment, tx_id) = receipt_commitment_fixture(&genesis);

        {
            let store = RocksDbStore::open(dir.path()).unwrap();
            store
                .commit_block_with_commitment_and_state_root(
                    &genesis,
                    &genesis_state(&genesis),
                    None,
                    Some(&genesis.app_hash),
                )
                .unwrap();
            store
                .commit_block_with_commitment_and_state_root(
                    &block,
                    &state,
                    Some(&commitment),
                    Some(&block.app_hash),
                )
                .unwrap();

            let lookup = store
                .load_transaction_receipt(&tx_id)
                .unwrap()
                .expect("committed receipt should be indexed");
            assert_eq!(lookup.tx_id, tx_id);
            assert_eq!(lookup.block_hash, block.hash());
            assert_eq!(lookup.block_height, block.height);
            assert_eq!(lookup.tx_index, 0);
            assert_eq!(lookup.receipt, commitment.receipts[0]);
        }

        let reopened = RocksDbStore::open(dir.path()).unwrap();
        let lookup = reopened
            .load_transaction_receipt(&tx_id)
            .unwrap()
            .expect("receipt index should survive restart");
        assert_eq!(lookup.block_hash, block.hash());
        assert_eq!(lookup.receipt, commitment.receipts[0]);
    }

    #[test]
    fn transaction_receipt_index_commit_retry_is_idempotent() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        store
            .commit_block(&genesis, &genesis_state(&genesis))
            .unwrap();
        let (block, state, commitment, tx_id) = receipt_commitment_fixture(&genesis);

        store
            .commit_block_with_commitment_and_state_root(
                &block,
                &state,
                Some(&commitment),
                Some(&block.app_hash),
            )
            .unwrap();
        let first = store
            .db
            .get_cf(store.cf(CF_TRANSACTION_RECEIPTS), tx_id)
            .unwrap()
            .expect("transaction receipt index row should be written");

        store
            .commit_block_with_commitment_and_state_root(
                &block,
                &state,
                Some(&commitment),
                Some(&block.app_hash),
            )
            .expect("retry with the same receipt mapping is idempotent");
        let second = store
            .db
            .get_cf(store.cf(CF_TRANSACTION_RECEIPTS), tx_id)
            .unwrap()
            .expect("transaction receipt index row should remain");
        assert_eq!(first, second);
        assert_eq!(
            store
                .load_transaction_receipt(&tx_id)
                .unwrap()
                .unwrap()
                .tx_id,
            tx_id
        );
    }

    #[test]
    fn transaction_receipt_index_corruption_fails_closed() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        store
            .commit_block(&genesis, &genesis_state(&genesis))
            .unwrap();
        let (block, state, commitment, tx_id) = receipt_commitment_fixture(&genesis);
        store
            .commit_block_with_commitment_and_state_root(
                &block,
                &state,
                Some(&commitment),
                Some(&block.app_hash),
            )
            .unwrap();

        store
            .db
            .put_cf(store.cf(CF_TRANSACTION_RECEIPTS), tx_id, b"corrupt")
            .unwrap();
        assert!(store.load_transaction_receipt(&tx_id).is_err());
    }

    #[test]
    fn failed_transaction_receipt_commit_writes_no_index_row() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        store
            .commit_block(&genesis, &genesis_state(&genesis))
            .unwrap();
        let (block, mut state, commitment, tx_id) = receipt_commitment_fixture(&genesis);
        state.committed_hash = [0xabu8; 32];

        assert!(store
            .commit_block_with_commitment_and_state_root(
                &block,
                &state,
                Some(&commitment),
                Some(&block.app_hash),
            )
            .is_err());
        assert!(store
            .db
            .get_cf(store.cf(CF_TRANSACTION_RECEIPTS), tx_id)
            .unwrap()
            .is_none());
        assert!(store.load_transaction_receipt(&tx_id).unwrap().is_none());
        assert!(store.get(&block.hash()).is_none());
    }

    #[test]
    fn repeated_system_receipt_hashes_do_not_conflict_with_global_index() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        store
            .commit_block(&genesis, &genesis_state(&genesis))
            .unwrap();
        let action = Transaction::Deposit {
            trader: "system:funding".to_string(),
            amount: 1,
        };
        let (block1, state1, commitment1, system_tx_id) =
            system_receipt_fixture(&genesis, 1, 1, &action);
        store
            .commit_block_with_commitment_and_state_root(
                &block1,
                &state1,
                Some(&commitment1),
                Some(&block1.app_hash),
            )
            .unwrap();
        let (block2, state2, commitment2, repeated_tx_id) =
            system_receipt_fixture(&block1, 2, 2, &action);
        assert_eq!(system_tx_id, repeated_tx_id);
        store
            .commit_block_with_commitment_and_state_root(
                &block2,
                &state2,
                Some(&commitment2),
                Some(&block2.app_hash),
            )
            .unwrap();

        assert!(store
            .load_transaction_receipt(&system_tx_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn only_signed_receipts_are_queryable_by_global_transaction_id() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        store
            .commit_block(&genesis, &genesis_state(&genesis))
            .unwrap();
        let (signed_block, signed_state, signed_commitment, signed_tx_id) =
            receipt_commitment_fixture(&genesis);
        store
            .commit_block_with_commitment_and_state_root(
                &signed_block,
                &signed_state,
                Some(&signed_commitment),
                Some(&signed_block.app_hash),
            )
            .unwrap();
        let action = Transaction::Deposit {
            trader: "system:funding".to_string(),
            amount: 1,
        };
        let (system_block, system_state, system_commitment, system_tx_id) =
            system_receipt_fixture(&signed_block, 2, 2, &action);
        store
            .commit_block_with_commitment_and_state_root(
                &system_block,
                &system_state,
                Some(&system_commitment),
                Some(&system_block.app_hash),
            )
            .unwrap();

        assert!(store
            .load_transaction_receipt(&signed_tx_id)
            .unwrap()
            .is_some());
        assert!(store
            .load_transaction_receipt(&system_tx_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn corrupted_index_cannot_expose_a_system_receipt() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        store
            .commit_block(&genesis, &genesis_state(&genesis))
            .unwrap();
        let action = Transaction::Deposit {
            trader: "system:funding".to_string(),
            amount: 1,
        };
        let (system_block, system_state, system_commitment, system_tx_id) =
            system_receipt_fixture(&genesis, 1, 1, &action);
        store
            .commit_block_with_commitment_and_state_root(
                &system_block,
                &system_state,
                Some(&system_commitment),
                Some(&system_block.app_hash),
            )
            .unwrap();

        let forged_row =
            TransactionReceiptIndexRecord::new(&system_block, &system_commitment.receipts[0])
                .canonical_bytes()
                .unwrap();
        store
            .db
            .put_cf(store.cf(CF_TRANSACTION_RECEIPTS), system_tx_id, forged_row)
            .unwrap();

        let error = store
            .load_transaction_receipt(&system_tx_id)
            .expect_err("system entries must not be exposed by the global lookup");
        assert!(error.to_string().contains("system transaction"));
    }

    #[test]
    fn receipt_payload_transaction_id_mismatch_writes_nothing() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        store
            .commit_block(&genesis, &genesis_state(&genesis))
            .unwrap();
        let (mut block, mut state, mut commitment, signed_tx_id) =
            receipt_commitment_fixture(&genesis);
        commitment.receipts[0].tx_id = [0xabu8; 32];
        block.commitment_root = commitment.root().unwrap();
        state.committed_hash = block.hash();

        assert!(store
            .commit_block_with_commitment_and_state_root(
                &block,
                &state,
                Some(&commitment),
                Some(&block.app_hash),
            )
            .is_err());
        assert!(store.get(&block.hash()).is_none());
        assert!(store
            .db
            .get_cf(store.cf(CF_TRANSACTION_RECEIPTS), signed_tx_id)
            .unwrap()
            .is_none());
        assert!(store
            .db
            .get_cf(store.cf(CF_TRANSACTION_RECEIPTS), [0xabu8; 32])
            .unwrap()
            .is_none());
    }

    #[test]
    fn state_root_is_immutable_across_commit_retries() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let state = genesis_state(&block);
        let first = [1u8; 32];
        let second = [2u8; 32];

        store
            .commit_block_with_commitment_and_state_root(&block, &state, None, Some(&first))
            .unwrap();
        store
            .commit_block_with_commitment_and_state_root(&block, &state, None, Some(&first))
            .expect("retry with the identical root is idempotent");
        assert!(store
            .commit_block_with_commitment_and_state_root(&block, &state, None, Some(&second))
            .is_err());
        assert_eq!(store.load_state_root(&block.hash()).unwrap(), Some(first));
        assert_eq!(
            store
                .load_consensus_state()
                .unwrap()
                .unwrap()
                .committed_hash,
            block.hash()
        );
    }

    #[test]
    fn non_genesis_state_root_must_match_authenticated_app_hash() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        store
            .commit_block(&genesis, &genesis_state(&genesis))
            .unwrap();

        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [1u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        let state = genesis_state(&block);
        let error = store
            .commit_block_with_commitment_and_state_root(&block, &state, None, Some(&[2u8; 32]))
            .expect_err("storage must reject a root not authenticated by the block");

        assert!(error.to_string().contains("authenticated block app hash"));
        assert!(store.get(&block.hash()).is_none());
        assert!(store.load_state_root(&block.hash()).unwrap().is_none());
    }

    #[test]
    fn non_genesis_commit_without_state_root_writes_nothing() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        let genesis_state = genesis_state(&genesis);
        store.commit_block(&genesis, &genesis_state).unwrap();

        let commitment = CommitmentV2::default();
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [1u8; 32],
            commitment_root: commitment.root().unwrap(),
            app_hash: [2u8; 32],
            timestamp: 1,
            justify: None,
        };
        let state = ConsensusState {
            committed_height: block.height,
            committed_hash: block.hash(),
            ..genesis_state
        };

        let error = store
            .commit_block_with_commitment_and_state_root(&block, &state, Some(&commitment), None)
            .expect_err("non-genesis commits must include an authenticated state root");
        assert!(error
            .to_string()
            .contains("missing its authenticated full-state root"));
        assert!(store.get(&block.hash()).is_none());
        assert!(store.get_by_height(block.height).is_none());
        assert!(store.load_commitment(&block.hash()).unwrap().is_none());
        assert!(store.load_state_root(&block.hash()).unwrap().is_none());
        assert_eq!(
            store
                .load_consensus_state()
                .unwrap()
                .unwrap()
                .committed_hash,
            genesis.hash()
        );
    }

    #[test]
    fn non_genesis_commitment_must_match_authenticated_header_root() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        let genesis_state = genesis_state(&genesis);
        store.commit_block(&genesis, &genesis_state).unwrap();

        let commitment = CommitmentV2::default();
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [1u8; 32],
            commitment_root: [9u8; 32],
            app_hash: [2u8; 32],
            timestamp: 1,
            justify: None,
        };
        let state = ConsensusState {
            committed_height: 1,
            committed_hash: block.hash(),
            ..genesis_state
        };

        let error = store
            .commit_block_with_commitment_and_state_root(
                &block,
                &state,
                Some(&commitment),
                Some(&block.app_hash),
            )
            .expect_err("artifact/header root mismatch must fail before persistence");
        assert!(error
            .to_string()
            .contains("execution commitment root does not match block"));
        assert!(store.get(&block.hash()).is_none());
        assert!(store.get_by_height(1).is_none());
    }

    #[test]
    fn corrupted_legacy_state_root_is_rejected_after_reopen() {
        let dir = TempDir::new().unwrap();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let state = genesis_state(&block);
        let root = [0xabu8; 32];

        {
            let store = RocksDbStore::open(dir.path()).unwrap();
            store
                .commit_block_with_commitment_and_state_root(&block, &state, None, Some(&root))
                .unwrap();
        }
        {
            let store = RocksDbStore::open(dir.path()).unwrap();
            store
                .db
                .put_cf(store.cf(CF_STATE_ROOTS), block.hash(), root)
                .unwrap();
        }

        let reopened = RocksDbStore::open(dir.path()).unwrap();
        let error = reopened
            .load_state_root(&block.hash())
            .expect_err("legacy raw root must fail closed after restart");
        assert!(error.to_string().contains("legacy raw 32-byte"));
        let retry_error = reopened
            .commit_block_with_commitment_and_state_root(&block, &state, None, Some(&root))
            .expect_err("commit must not overwrite a corrupt legacy row");
        assert!(retry_error
            .to_string()
            .contains("invalid existing state-root record"));
    }

    #[test]
    fn invalid_commitment_cannot_cross_the_storage_boundary() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let state = genesis_state(&block);
        let mut invalid = CommitmentV2::new(Vec::new()).unwrap();
        invalid.schema_version = 99;

        assert!(store
            .commit_block_with_commitment(&block, &state, Some(&invalid))
            .is_err());
        assert!(store.get(&block.hash()).is_none());
        assert!(store.load_commitment(&block.hash()).unwrap().is_none());
    }

    #[test]
    fn genesis_commit_reports_missing_artifacts_explicitly() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let state = genesis_state(&block);

        store.commit_block(&block, &state).unwrap();

        assert_eq!(
            store.load_block_artifacts(&block.hash()).unwrap(),
            None,
            "genesis must not fabricate an empty bundle"
        );
        assert_eq!(
            store.load_block_artifacts_by_height(block.height).unwrap(),
            None
        );
    }

    #[test]
    fn unfinalized_block_never_has_queryable_artifacts() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);

        store.save_block(&block).unwrap();

        assert!(store.get(&block.hash()).is_some());
        assert_eq!(store.load_block_artifacts(&block.hash()).unwrap(), None);
        // save_block intentionally does not update the canonical height index.
        assert_eq!(
            store.load_block_artifacts_by_height(block.height).unwrap(),
            None
        );
    }

    #[test]
    fn artifact_commit_validation_failure_writes_nothing() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let mut invalid_state = genesis_state(&block);
        invalid_state.committed_hash = [9u8; 32];

        assert!(store
            .commit_block_with_artifacts(&block, &invalid_state, Some(b"artifact"))
            .is_err());
        assert!(store.get(&block.hash()).is_none());
        assert!(store.load_consensus_state().unwrap().is_none());
        assert_eq!(store.load_block_artifacts(&block.hash()).unwrap(), None);
        assert_eq!(
            store.load_block_artifacts_by_height(block.height).unwrap(),
            None
        );
    }

    #[test]
    fn state_root_commit_validation_failure_writes_nothing() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let mut invalid_state = genesis_state(&block);
        invalid_state.committed_hash = [9u8; 32];
        let root = [3u8; 32];

        assert!(store
            .commit_block_with_commitment_and_state_root(
                &block,
                &invalid_state,
                Some(&CommitmentV2::new(Vec::new()).unwrap()),
                Some(&root),
            )
            .is_err());
        assert!(store.get(&block.hash()).is_none());
        assert!(store.load_consensus_state().unwrap().is_none());
        assert!(store.load_commitment(&block.hash()).unwrap().is_none());
        assert!(store.load_state_root(&block.hash()).unwrap().is_none());
    }

    #[test]
    fn artifact_bytes_are_immutable_across_commit_retries() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let state = genesis_state(&block);

        store
            .commit_block_with_artifacts(&block, &state, Some(b"first"))
            .unwrap();
        assert!(store
            .commit_block_with_artifacts(&block, &state, Some(b"different"))
            .is_err());

        assert_eq!(
            store.load_block_artifacts(&block.hash()).unwrap(),
            Some(b"first".to_vec())
        );
        assert_eq!(
            store
                .load_consensus_state()
                .unwrap()
                .unwrap()
                .committed_hash,
            block.hash()
        );
    }

    #[test]
    fn test_consensus_state() {
        let (store, _dir) = create_test_store();

        let state = ConsensusState {
            epoch: 0,
            committee_hash: [7u8; 32],
            genesis_hash: [8u8; 32],
            high_qc: None,
            locked_qc: None,
            voted_views: vec![1, 2, 3],
            current_view: 5,
            committed_height: 10,
            committed_hash: [1u8; 32],
            consecutive_timeouts: 2,
            vc_sent_for_view: Some(4),
        };

        store.save_consensus_state(&state).unwrap();

        let loaded = store.load_consensus_state().unwrap().unwrap();
        assert_eq!(loaded.current_view, 5);
        assert_eq!(loaded.committed_height, 10);
        assert_eq!(loaded.voted_views, vec![1, 2, 3]);
        assert_eq!(loaded.genesis_hash, [8u8; 32]);
    }

    #[test]
    fn commit_block_rejects_mismatched_metadata_before_writing() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let block = Block::genesis(context);
        let state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: block.height,
            committed_hash: [9u8; 32],
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };

        assert!(store.commit_block(&block, &state).is_err());
        assert!(store.get(&block.hash()).is_none());
        assert!(store.load_consensus_state().unwrap().is_none());
    }

    #[test]
    fn unfinalized_save_does_not_replace_finalized_height_index() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let finalized = Block::genesis(context);
        let mut competing = finalized.clone();
        competing.timestamp = 1;

        let state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: finalized.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        store.commit_block(&finalized, &state).unwrap();
        store.save_block(&competing).unwrap();

        assert_eq!(store.get_by_height(0).unwrap().hash(), finalized.hash());
        assert_eq!(
            store.get(&competing.hash()).unwrap().hash(),
            competing.hash()
        );
    }

    #[test]
    fn commit_block_rejects_a_fork_at_the_next_height_without_writing() {
        let (store, _dir) = create_test_store();
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [8u8; 32]);
        let genesis = Block::genesis(context);
        let genesis_state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        store.commit_block(&genesis, &genesis_state).unwrap();

        let block1 = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [1u8; 32],
            commitment_root: CommitmentV2::default().root().unwrap(),
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        let state1 = ConsensusState {
            committed_height: 1,
            committed_hash: block1.hash(),
            ..genesis_state.clone()
        };
        let commitment = CommitmentV2::default();
        store
            .commit_block_with_commitment_and_state_root(
                &block1,
                &state1,
                Some(&commitment),
                Some(&block1.app_hash),
            )
            .unwrap();

        let fork = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 2,
            height: 2,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [1u8; 32],
            commitment_root: commitment.root().unwrap(),
            app_hash: [2u8; 32],
            timestamp: 2,
            justify: None,
        };
        let fork_state = ConsensusState {
            committed_height: 2,
            committed_hash: fork.hash(),
            ..state1
        };

        assert!(store
            .commit_block_with_commitment_and_state_root(
                &fork,
                &fork_state,
                Some(&commitment),
                Some(&fork.app_hash),
            )
            .is_err());
        assert_eq!(store.get_committed_head().unwrap().hash(), block1.hash());
        assert_eq!(store.load_committed_height().unwrap(), Some(1));
        assert!(store.get_by_height(2).is_none());
        assert!(store.get(&fork.hash()).is_none());
    }
}
