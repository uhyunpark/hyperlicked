//! RocksDB Storage Implementation
//!
//! Uses column families for logical separation:
//! - blocks: Hash -> Block
//! - height_index: u64 -> Hash
//! - consensus: safety state (high_qc, locked_qc, voted_views, current_view)
//! - snapshots: height -> AppSnapshot
//! - meta: committed_height, committed_hash

use std::path::Path;

use rocksdb::{ColumnFamilyDescriptor, Options, WriteBatch, DB};

use super::{AppSnapshot, ConsensusState, PersistentStore};
use crate::app::candles::Candle;
use crate::consensus::BlockStore;
use crate::types::{Block, Hash};

/// Column family names
const CF_BLOCKS: &str = "blocks";
const CF_HEIGHT_INDEX: &str = "height_index";
const CF_CONSENSUS: &str = "consensus";
const CF_SNAPSHOTS: &str = "snapshots";
const CF_META: &str = "meta";
const CF_CANDLES: &str = "candles";

/// RocksDB-backed persistent store
pub struct RocksDbStore {
    db: DB,
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
        ];

        let db = DB::open_cf_descriptors(&opts, path, cfs)?;
        Ok(Self { db })
    }

    /// Get column family handle (panics if not found - programming error)
    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        self.db.cf_handle(name).expect("column family must exist")
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
}

impl BlockStore for RocksDbStore {
    fn save(&self, block: &Block) {
        let hash = block.hash();
        let bytes = serde_json::to_vec(block).expect("block serialization failed");

        // Save block by hash
        self.db
            .put_cf(self.cf(CF_BLOCKS), hash, &bytes)
            .expect("block save failed");

        // Save height -> hash index
        self.db
            .put_cf(self.cf(CF_HEIGHT_INDEX), block.height.to_be_bytes(), hash)
            .expect("height index save failed");
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
        let _ = self.db.put_cf(self.cf(CF_META), b"committed_hash", hash);
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
    fn save_consensus_state(&self, state: &ConsensusState) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(state)?;
        self.db.put_cf(self.cf(CF_CONSENSUS), b"state", &bytes)?;
        Ok(())
    }

    fn load_consensus_state(&self) -> anyhow::Result<Option<ConsensusState>> {
        match self.db.get_cf(self.cf(CF_CONSENSUS), b"state")? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    fn save_snapshot(&self, height: u64, snapshot: &AppSnapshot) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(snapshot)?;
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
                    let snapshot: AppSnapshot = serde_json::from_slice(&value)?;
                    return Ok(Some((height, snapshot)));
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
        let mut batch = WriteBatch::default();

        let hash = block.hash();
        let block_bytes = serde_json::to_vec(block)?;
        let state_bytes = serde_json::to_vec(state)?;

        // Block by hash
        batch.put_cf(self.cf(CF_BLOCKS), hash, &block_bytes);

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

        // Atomic write
        self.db.write(batch)?;

        Ok(())
    }

    fn save_candles_batch(&self, entries: &[(Vec<u8>, Vec<u8>)]) -> anyhow::Result<()> {
        RocksDbStore::save_candles_batch(self, entries)
    }

    fn load_candles(&self, symbol: &str, interval_str: &str, limit: usize) -> anyhow::Result<Vec<Candle>> {
        RocksDbStore::load_candles(self, symbol, interval_str, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_store() -> (RocksDbStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();
        (store, dir)
    }

    #[test]
    fn test_block_save_and_get() {
        let (store, _dir) = create_test_store();

        let block = Block::genesis();
        let hash = block.hash();

        store.save(&block);

        let loaded = store.get(&hash).unwrap();
        assert_eq!(loaded.height, block.height);
        assert_eq!(loaded.view, block.view);
    }

    #[test]
    fn test_get_by_height() {
        let (store, _dir) = create_test_store();

        let block = Block::genesis();
        store.save(&block);

        let loaded = store.get_by_height(0).unwrap();
        assert_eq!(loaded.height, 0);
    }

    #[test]
    fn test_consensus_state() {
        let (store, _dir) = create_test_store();

        let state = ConsensusState {
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
    }
}
