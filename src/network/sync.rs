//! P2P Sync Protocol
//!
//! Handles block catchup and snapshot transfer between nodes.
//!
//! ## Components
//!
//! - `SyncHandler` - Responds to incoming sync/snapshot requests
//! - `SyncClient` - Creates requests and processes responses
//!
//! ## Protocol Flow
//!
//! ### Block Catchup
//! ```text
//! New Node                        Peer
//!    │                             │
//!    │──SyncRequest(from=100)──────│
//!    │                             │
//!    │◄─SyncResponse(blocks=[...])─│
//!    │                             │
//!    │   (verify + apply blocks)   │
//!    │                             │
//!    │──SyncRequest(from=200)──────│
//!    │         ...                 │
//! ```
//!
//! ### Snapshot Sync
//! ```text
//! New Node                        Peer
//!    │                             │
//!    │──SnapshotRequest(latest)────│
//!    │                             │
//!    │◄─SnapshotResponse(data=...)─│
//!    │                             │
//!    │   (verify + load snapshot)  │
//!    │                             │
//!    │──SyncRequest(from=snap_h)───│
//!    │         ...                 │
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::storage::PersistentStore;
use crate::types::{
    Message, SnapshotRequest, SnapshotResponse, SyncRequest, SyncResponse, MAX_SYNC_RESPONSE_BYTES,
};

/// Maximum blocks per sync response
const MAX_BLOCKS_PER_RESPONSE: u64 = 100;

/// Check a snapshot payload against the exact bincode response envelope and
/// one-byte TCP format prefix.  A zero-length `Vec` has the same bincode
/// length-prefix representation as a non-empty one, so the payload length can
/// be added without cloning the snapshot bytes.
fn snapshot_data_fits_wire(request_id: u64, height: u64, data_len: usize) -> bool {
    let envelope = bincode::serialized_size(&Message::SnapshotResponse(SnapshotResponse {
        request_id,
        height: Some(height),
        data: Some(Vec::new()),
        compressed: false,
    }))
    .ok()
    .and_then(|size| size.checked_add(u64::try_from(data_len).ok()?))
    .and_then(|size| size.checked_add(1))
    .and_then(|size| usize::try_from(size).ok());
    envelope.is_some_and(|size| size <= MAX_SYNC_RESPONSE_BYTES)
}

// =============================================================================
// Sync Handler (Server Side)
// =============================================================================

/// Handles incoming sync requests from peers
pub struct SyncHandler {
    store: Arc<dyn PersistentStore + Send + Sync>,
    current_height: Arc<AtomicU64>,
}

impl SyncHandler {
    /// Create a new sync handler with a persistent store
    pub fn new(
        store: Arc<dyn PersistentStore + Send + Sync>,
        current_height: Arc<AtomicU64>,
    ) -> Self {
        Self {
            store,
            current_height,
        }
    }

    /// Publish a newly committed height to sync responders.
    ///
    /// Callers must invoke this only after the corresponding durable commit
    /// succeeds and their in-memory committed head has advanced.  Keeping the
    /// tracker separate from storage avoids advertising a height that peers
    /// cannot yet retrieve after a crash.
    pub fn update_height(&self, new_height: u64) {
        self.current_height.store(new_height, Ordering::SeqCst);
    }

    /// Handle a SyncRequest and return a SyncResponse
    pub fn handle_sync_request(&self, req: SyncRequest) -> SyncResponse {
        self.handle_sync_request_with_limit(req, MAX_SYNC_RESPONSE_BYTES)
    }

    fn handle_sync_request_with_limit(
        &self,
        req: SyncRequest,
        max_response_bytes: usize,
    ) -> SyncResponse {
        let peer_height = self.current_height.load(Ordering::SeqCst);

        // Limit blocks per request
        let max_blocks = req.max_blocks.min(MAX_BLOCKS_PER_RESPONSE);
        let to_height = req.to_height.unwrap_or(peer_height).min(peer_height);

        // Read only the bounded canonical height window.  Loading the entire
        // chain tail before applying `max_blocks` would let a small request
        // allocate all historical blocks.
        let scan_to = if max_blocks == 0 {
            req.from_height.saturating_sub(1)
        } else {
            req.from_height
                .saturating_add(max_blocks.saturating_sub(1))
                .min(to_height)
        };
        let mut response = SyncResponse {
            request_id: req.request_id,
            blocks: Vec::new(),
            peer_height,
            has_more: false,
        };
        let mut height = req.from_height;
        let mut stopped_for_size = false;
        let mut stopped_for_missing_block = false;
        let base_wire_bytes = bincode::serialized_size(&Message::SyncResponse(SyncResponse {
            request_id: 0,
            blocks: Vec::new(),
            peer_height: 0,
            has_more: false,
        }))
        .ok()
        .and_then(|size| size.checked_add(1))
        .unwrap_or(u64::MAX);
        let mut wire_bytes = base_wire_bytes;

        while max_blocks != 0 && response.blocks.len() < max_blocks as usize && height <= scan_to {
            let Some(block) = self.store.get_by_height(height) else {
                stopped_for_missing_block = true;
                break;
            };
            if block.height != height || block.height > peer_height {
                stopped_for_missing_block = true;
                break;
            }

            let block_wire_bytes = match bincode::serialized_size(&block) {
                Ok(size) => size,
                Err(_) => break,
            };
            if wire_bytes
                .checked_add(block_wire_bytes)
                .and_then(|size| usize::try_from(size).ok())
                .is_none_or(|size| size > max_response_bytes)
            {
                stopped_for_size = true;
                break;
            }

            wire_bytes += block_wire_bytes;
            response.blocks.push(block);
            height = match height.checked_add(1) {
                Some(next) => next,
                None => break,
            };
        }

        // A size-limited page is valid and advertises progress. A missing or
        // mismatched committed height is storage corruption/incompleteness;
        // stop at the gap and never skip forward to serve a false chain.
        response.has_more = !stopped_for_missing_block
            && (stopped_for_size
                || response
                    .blocks
                    .last()
                    .map(|last| last.height < to_height)
                    .unwrap_or(false));
        debug_assert!(
            bincode::serialized_size(&Message::SyncResponse(response.clone()))
                .ok()
                .and_then(|size| size.checked_add(1))
                .and_then(|size| usize::try_from(size).ok())
                .is_some_and(|size| size <= max_response_bytes),
            "sync response must fit the wire byte limit"
        );
        response
    }

    /// Handle a SnapshotRequest and return a SnapshotResponse
    pub fn handle_snapshot_request(&self, req: SnapshotRequest) -> SnapshotResponse {
        let height = req.height.unwrap_or(u64::MAX);

        match self.store.load_latest_snapshot(height) {
            Ok(Some((snap_height, snapshot))) => {
                let data = snapshot.to_bounded_json().ok().filter(|bytes| {
                    snapshot_data_fits_wire(req.request_id, snap_height, bytes.len())
                });

                SnapshotResponse {
                    request_id: req.request_id,
                    height: Some(snap_height),
                    data,
                    compressed: false,
                }
            }
            _ => SnapshotResponse {
                request_id: req.request_id,
                height: None,
                data: None,
                compressed: false,
            },
        }
    }
}

// =============================================================================
// Sync Client (Client Side)
// =============================================================================

/// Creates sync requests and tracks progress
pub struct SyncClient {
    /// Next request ID for correlation
    next_request_id: AtomicU64,
    /// Our current synced height
    current_height: AtomicU64,
}

impl SyncClient {
    pub fn new(current_height: u64) -> Self {
        Self {
            next_request_id: AtomicU64::new(0),
            current_height: AtomicU64::new(current_height),
        }
    }

    /// Create a sync request starting from our current height
    pub fn create_sync_request(&self) -> SyncRequest {
        let from_height = self.current_height.load(Ordering::SeqCst) + 1;
        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);

        SyncRequest {
            from_height,
            to_height: None, // Request all available
            max_blocks: MAX_BLOCKS_PER_RESPONSE,
            request_id,
        }
    }

    /// Create a sync request for a specific range
    pub fn create_range_request(&self, from: u64, to: u64) -> SyncRequest {
        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);

        SyncRequest {
            from_height: from,
            to_height: Some(to),
            max_blocks: MAX_BLOCKS_PER_RESPONSE,
            request_id,
        }
    }

    /// Create a snapshot request
    pub fn create_snapshot_request(&self, height: Option<u64>) -> SnapshotRequest {
        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);

        SnapshotRequest { height, request_id }
    }

    /// Update our height after processing a sync response
    pub fn update_height(&self, new_height: u64) {
        self.current_height.store(new_height, Ordering::SeqCst);
    }

    /// Get our current synced height
    pub fn current_height(&self) -> u64 {
        self.current_height.load(Ordering::SeqCst)
    }

    /// Check if we need more blocks based on a sync response
    pub fn needs_more(&self, response: &SyncResponse) -> bool {
        response.has_more || self.current_height.load(Ordering::SeqCst) < response.peer_height
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::app::candles::Candle;
    use crate::consensus::BlockStore;
    use crate::storage::{AppSnapshot, ConsensusState, PersistentStore};
    use crate::types::Hash;
    use crate::types::{Block, ConsensusContext};
    use tempfile::TempDir;

    #[derive(Default)]
    struct MemorySyncStore {
        blocks: Mutex<BTreeMap<u64, Block>>,
        snapshot: Mutex<Option<(u64, AppSnapshot)>>,
    }

    impl BlockStore for MemorySyncStore {
        fn save(&self, block: &Block) {
            self.blocks
                .lock()
                .unwrap()
                .insert(block.height, block.clone());
        }

        fn get(&self, hash: &Hash) -> Option<Block> {
            self.blocks
                .lock()
                .unwrap()
                .values()
                .find(|block| block.hash() == *hash)
                .cloned()
        }

        fn get_by_height(&self, height: u64) -> Option<Block> {
            self.blocks.lock().unwrap().get(&height).cloned()
        }

        fn set_committed(&self, _hash: &Hash) {}

        fn get_committed_head(&self) -> Option<Block> {
            self.blocks.lock().unwrap().values().next_back().cloned()
        }
    }

    impl PersistentStore for MemorySyncStore {
        fn save_consensus_state(&self, _state: &ConsensusState) -> anyhow::Result<()> {
            Ok(())
        }

        fn load_consensus_state(&self) -> anyhow::Result<Option<ConsensusState>> {
            Ok(None)
        }

        fn save_snapshot(&self, height: u64, snapshot: &AppSnapshot) -> anyhow::Result<()> {
            *self.snapshot.lock().unwrap() = Some((height, snapshot.clone()));
            Ok(())
        }

        fn load_latest_snapshot(
            &self,
            before_height: u64,
        ) -> anyhow::Result<Option<(u64, AppSnapshot)>> {
            Ok(self
                .snapshot
                .lock()
                .unwrap()
                .as_ref()
                .filter(|(height, _)| *height <= before_height)
                .cloned())
        }

        fn load_latest_snapshot_height(&self, before_height: u64) -> anyhow::Result<Option<u64>> {
            Ok(self
                .snapshot
                .lock()
                .unwrap()
                .as_ref()
                .filter(|(height, _)| *height <= before_height)
                .map(|(height, _)| *height))
        }

        fn blocks_from_height(&self, from_height: u64) -> anyhow::Result<Vec<Block>> {
            Ok(self
                .blocks
                .lock()
                .unwrap()
                .range(from_height..)
                .map(|(_, block)| block.clone())
                .collect())
        }

        fn commit_block(&self, block: &Block, _state: &ConsensusState) -> anyhow::Result<()> {
            self.save(block);
            Ok(())
        }

        fn save_candles_batch(&self, _entries: &[(Vec<u8>, Vec<u8>)]) -> anyhow::Result<()> {
            Ok(())
        }

        fn load_candles(
            &self,
            _symbol: &str,
            _interval_str: &str,
            _limit: usize,
        ) -> anyhow::Result<Vec<Candle>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn test_sync_client_request_ids() {
        let client = SyncClient::new(0);

        let req1 = client.create_sync_request();
        let req2 = client.create_sync_request();

        assert_eq!(req1.request_id, 0);
        assert_eq!(req2.request_id, 1);
        assert_eq!(req1.from_height, 1);
        assert_eq!(req2.from_height, 1); // Height not updated yet
    }

    #[test]
    fn test_sync_client_height_tracking() {
        let client = SyncClient::new(100);

        assert_eq!(client.current_height(), 100);

        let req = client.create_sync_request();
        assert_eq!(req.from_height, 101);

        client.update_height(200);
        assert_eq!(client.current_height(), 200);

        let req2 = client.create_sync_request();
        assert_eq!(req2.from_height, 201);
    }

    #[test]
    fn test_sync_client_range_request() {
        let client = SyncClient::new(0);

        let req = client.create_range_request(50, 100);
        assert_eq!(req.from_height, 50);
        assert_eq!(req.to_height, Some(100));
    }

    #[test]
    fn test_sync_client_needs_more() {
        let client = SyncClient::new(100);

        let response = SyncResponse {
            request_id: 0,
            blocks: Vec::new(),
            peer_height: 200,
            has_more: false,
        };

        assert!(client.needs_more(&response)); // Behind peer

        client.update_height(200);
        assert!(!client.needs_more(&response)); // Caught up

        let response_with_more = SyncResponse {
            request_id: 1,
            blocks: Vec::new(),
            peer_height: 200,
            has_more: true,
        };

        assert!(client.needs_more(&response_with_more)); // has_more = true
    }

    #[test]
    fn sync_handler_height_update_changes_advertised_range() {
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let store =
            Arc::new(crate::storage::RocksDbStore::open(temp_dir.path()).expect("open test store"));
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        store.save(&genesis);
        let block1 = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [1u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        store.save(&block1);
        let block2 = Block {
            height: 2,
            parent: block1.hash(),
            view: 2,
            timestamp: 2,
            ..block1.clone()
        };
        store.save(&block2);

        let tracker = Arc::new(AtomicU64::new(1));
        let handler = SyncHandler::new(store, tracker);
        let request = SyncRequest {
            from_height: 0,
            to_height: None,
            max_blocks: 100,
            request_id: 1,
        };

        let before_commit = handler.handle_sync_request(request.clone());
        assert_eq!(before_commit.peer_height, 1);
        assert_eq!(
            before_commit.blocks.last().map(|block| block.height),
            Some(1)
        );
        assert!(!before_commit.has_more);

        handler.update_height(2);
        let after_commit = handler.handle_sync_request(request);
        assert_eq!(after_commit.peer_height, 2);
        assert_eq!(
            after_commit.blocks.last().map(|block| block.height),
            Some(2)
        );
        assert!(!after_commit.has_more);
    }

    #[test]
    fn sync_response_is_wire_bounded_and_pages_large_blocks() {
        let store = Arc::new(MemorySyncStore::default());
        let context = ConsensusContext::new(0, [7u8; 32]);
        let mut parent = Block::genesis(context);
        store.save(&parent);

        for height in 1..=5 {
            let block = Block {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view: height,
                height,
                parent: parent.hash(),
                payload: vec![0u8; 1024],
                proposer: [1u8; 32],
                commitment_root: [0u8; 32],
                app_hash: [height as u8; 32],
                timestamp: height,
                justify: None,
            };
            store.save(&block);
            parent = block;
        }

        let handler = SyncHandler::new(store, Arc::new(AtomicU64::new(5)));
        let byte_limit = 4 * 1024;
        let response = handler.handle_sync_request_with_limit(
            SyncRequest {
                from_height: 0,
                to_height: Some(5),
                max_blocks: 100,
                request_id: 1,
            },
            byte_limit,
        );
        let wire_size = bincode::serialized_size(&Message::SyncResponse(response.clone()))
            .expect("sync response must serialize")
            .checked_add(1)
            .and_then(|size| usize::try_from(size).ok())
            .expect("wire size must fit usize");

        assert!(wire_size <= byte_limit);
        assert!(response.has_more, "large blocks must force another page");
        assert!(response.blocks.len() < 6);
        assert!(!response.blocks.is_empty());
    }

    #[test]
    fn missing_committed_height_stops_without_skipping_later_blocks() {
        let store = Arc::new(MemorySyncStore::default());
        let context = ConsensusContext::new(0, [8u8; 32]);
        let genesis = Block::genesis(context);
        store.save(&genesis);
        let block2 = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 2,
            height: 2,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [1u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [2u8; 32],
            timestamp: 2,
            justify: None,
        };
        store.save(&block2);

        let handler = SyncHandler::new(store, Arc::new(AtomicU64::new(2)));
        let response = handler.handle_sync_request(SyncRequest {
            from_height: 0,
            to_height: Some(2),
            max_blocks: 100,
            request_id: 1,
        });

        assert_eq!(
            response
                .blocks
                .iter()
                .map(|block| block.height)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert!(!response.has_more);
    }

    #[test]
    fn snapshot_wire_limit_accepts_boundary_and_rejects_one_byte_over() {
        let empty_data_wire_size =
            bincode::serialized_size(&Message::SnapshotResponse(SnapshotResponse {
                request_id: 1,
                height: Some(0),
                data: Some(Vec::new()),
                compressed: false,
            }))
            .expect("empty snapshot response must serialize")
            .checked_add(1)
            .and_then(|size| usize::try_from(size).ok())
            .expect("wire size must fit usize");
        let boundary_data_len = MAX_SYNC_RESPONSE_BYTES - empty_data_wire_size;

        assert!(snapshot_data_fits_wire(1, 0, boundary_data_len));
        assert!(!snapshot_data_fits_wire(1, 0, boundary_data_len + 1));
    }
}
