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
use crate::types::{Block, SnapshotRequest, SnapshotResponse, SyncRequest, SyncResponse};

/// Maximum blocks per sync response
const MAX_BLOCKS_PER_RESPONSE: u64 = 100;

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

    /// Handle a SyncRequest and return a SyncResponse
    pub fn handle_sync_request(&self, req: SyncRequest) -> SyncResponse {
        let peer_height = self.current_height.load(Ordering::SeqCst);

        // Limit blocks per request
        let max_blocks = req.max_blocks.min(MAX_BLOCKS_PER_RESPONSE);
        let to_height = req.to_height.unwrap_or(peer_height).min(peer_height);

        // Fetch blocks from storage
        let blocks: Vec<Block> = match self.store.blocks_from_height(req.from_height) {
            Ok(all_blocks) => all_blocks
                .into_iter()
                .filter(|b| b.height >= req.from_height && b.height <= to_height)
                .take(max_blocks as usize)
                .collect(),
            Err(_) => Vec::new(),
        };

        // Check if there are more blocks
        let has_more = blocks.last().map(|b| b.height < to_height).unwrap_or(false);

        SyncResponse {
            request_id: req.request_id,
            blocks,
            peer_height,
            has_more,
        }
    }

    /// Handle a SnapshotRequest and return a SnapshotResponse
    pub fn handle_snapshot_request(&self, req: SnapshotRequest) -> SnapshotResponse {
        let height = req.height.unwrap_or(u64::MAX);

        match self.store.load_latest_snapshot(height) {
            Ok(Some((snap_height, snapshot))) => {
                // Serialize snapshot
                let data = match serde_json::to_vec(&snapshot) {
                    Ok(bytes) => Some(bytes),
                    Err(_) => None,
                };

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
    use super::*;

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
}
