//! Sync API Routes
//!
//! Block export and snapshot endpoints for node synchronization.
//!
//! ## Endpoints
//!
//! - `GET /api/v1/sync/status` - Node sync status
//! - `GET /api/v1/sync/blocks` - Export block range with pagination
//! - `GET /api/v1/sync/block/:height` - Single block by height
//! - `GET /api/v1/sync/snapshot/latest` - Latest snapshot metadata
//! - `GET /api/v1/sync/snapshot/:height` - Download full snapshot

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

use crate::api::types::{
    ApiState, BlockExport, BlockRangeQuery, BlockRangeResponse, CertificateExport,
    SnapshotExport, SnapshotMetadata, SyncStatus,
};

use crate::types::Certificate;

/// Maximum blocks per request (prevents DoS)
const MAX_BLOCKS_PER_REQUEST: u64 = 1000;
/// Default blocks per request
const DEFAULT_BLOCKS_PER_REQUEST: u64 = 100;

/// Convert Certificate to CertificateExport
fn export_certificate(cert: &Certificate) -> CertificateExport {
    CertificateExport {
        view: cert.view,
        block_hash: hex::encode(cert.block_hash),
        voters: cert.voters.iter().map(|v| hex::encode(v)).collect(),
        bls_pubkeys: cert.bls_pubkeys.iter().map(|pk| hex::encode(pk)).collect(),
        agg_signature: hex::encode(&cert.agg_signature),
    }
}

// =============================================================================
// Sync Status
// =============================================================================

/// Get node sync status
///
/// Returns current height, view, committed hash, and snapshot info.
pub async fn get_sync_status(
    State(state): State<ApiState>,
) -> Result<Json<SyncStatus>, (StatusCode, String)> {
    let app = state.shared.app.read().await;
    let height = app.committed_height();
    let view = app.current_view();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Get committed hash from state hash computation
    // Use the full hash computation (read-only) for API since we only have read lock
    let state_hash = app.compute_state_hash_full();

    // Check if we have persistent storage
    let (latest_snapshot_height, is_persistent) = match &state.store {
        Some(store) => {
            let snapshot_height = store
                .load_latest_snapshot(u64::MAX)
                .ok()
                .flatten()
                .map(|(h, _)| h);
            (snapshot_height, true)
        }
        None => (None, false),
    };

    Ok(Json(SyncStatus {
        height,
        view,
        committed_hash: hex::encode(&state_hash),
        state_hash: hex::encode(&state_hash),
        timestamp,
        latest_snapshot_height,
        is_persistent,
    }))
}

// =============================================================================
// Block Export
// =============================================================================

/// Get blocks in a range with pagination
///
/// Query params:
/// - `from`: Start height (required)
/// - `to`: End height (optional, defaults to latest)
/// - `limit`: Max blocks to return (default 100, max 1000)
/// - `includePayload`: Include base64 payload (default false)
pub async fn get_blocks(
    State(state): State<ApiState>,
    Query(query): Query<BlockRangeQuery>,
) -> Result<Json<BlockRangeResponse>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Persistence not enabled".to_string(),
        )
    })?;

    // Get current height
    let app = state.shared.app.read().await;
    let current_height = app.committed_height();
    drop(app);

    // Validate range
    let from = query.from;
    let to = query.to.unwrap_or(current_height).min(current_height);
    let limit = query
        .limit
        .unwrap_or(DEFAULT_BLOCKS_PER_REQUEST)
        .min(MAX_BLOCKS_PER_REQUEST);

    if from > to {
        return Err((StatusCode::BAD_REQUEST, "from > to".to_string()));
    }

    // Fetch blocks from storage
    let blocks = store.blocks_from_height(from).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Storage error: {}", e),
        )
    })?;

    // Filter to range and limit
    let mut exports: Vec<BlockExport> = blocks
        .into_iter()
        .filter(|b| b.height >= from && b.height <= to)
        .take(limit as usize)
        .map(|b| {
            let hash = b.hash();
            BlockExport {
                height: b.height,
                view: b.view,
                hash: hex::encode(hash),
                parent_hash: hex::encode(b.parent),
                app_hash: hex::encode(b.app_hash),
                proposer: hex::encode(b.proposer),
                timestamp: b.timestamp,
                payload_size: b.payload.len(),
                payload: if query.include_payload {
                    Some(BASE64.encode(&b.payload))
                } else {
                    None
                },
                justify: b.justify.as_ref().map(export_certificate),
            }
        })
        .collect();

    // Sort by height (should already be sorted, but ensure)
    exports.sort_by_key(|b| b.height);

    // Compute next_height for pagination
    let next_height = if !exports.is_empty() {
        let last_height = exports.last().unwrap().height;
        if last_height < to {
            Some(last_height + 1)
        } else {
            None
        }
    } else {
        None
    };

    Ok(Json(BlockRangeResponse {
        blocks: exports,
        next_height,
        total_available: current_height.saturating_sub(from) + 1,
    }))
}

/// Get a single block by height
pub async fn get_block_by_height(
    State(state): State<ApiState>,
    Path(height): Path<u64>,
) -> Result<Json<BlockExport>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Persistence not enabled".to_string(),
        )
    })?;

    let block = store.get_by_height(height).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Block at height {} not found", height),
        )
    })?;

    let hash = block.hash();
    Ok(Json(BlockExport {
        height: block.height,
        view: block.view,
        hash: hex::encode(hash),
        parent_hash: hex::encode(block.parent),
        app_hash: hex::encode(block.app_hash),
        proposer: hex::encode(block.proposer),
        timestamp: block.timestamp,
        payload_size: block.payload.len(),
        payload: Some(BASE64.encode(&block.payload)),
        justify: block.justify.as_ref().map(export_certificate),
    }))
}

// =============================================================================
// Snapshot Export
// =============================================================================

/// Get latest snapshot metadata
pub async fn get_latest_snapshot(
    State(state): State<ApiState>,
) -> Result<Json<SnapshotMetadata>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Persistence not enabled".to_string(),
        )
    })?;

    let (height, snapshot) = store.load_latest_snapshot(u64::MAX).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Storage error: {}", e),
        )
    })?.ok_or_else(|| {
        (StatusCode::NOT_FOUND, "No snapshots available".to_string())
    })?;

    // Compute size (serialize snapshot)
    let snapshot_bytes = serde_json::to_vec(&snapshot).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Serialization error: {}", e),
        )
    })?;

    // Compute state hash from snapshot
    let state_hash = crate::types::hash(&snapshot_bytes);

    Ok(Json(SnapshotMetadata {
        height,
        timestamp: snapshot.timestamp,
        state_hash: hex::encode(state_hash),
        size_bytes: snapshot_bytes.len() as u64,
        account_count: snapshot.accounts.len(),
        market_count: snapshot.market_configs.len(),
    }))
}

/// Get full snapshot by height
pub async fn get_snapshot(
    State(state): State<ApiState>,
    Path(height): Path<u64>,
) -> Result<Json<SnapshotExport>, (StatusCode, String)> {
    let store = state.store.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Persistence not enabled".to_string(),
        )
    })?;

    // Find snapshot at or before requested height
    let (snap_height, snapshot) = store.load_latest_snapshot(height).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Storage error: {}", e),
        )
    })?.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("No snapshot at or before height {}", height),
        )
    })?;

    // Serialize snapshot
    let snapshot_bytes = serde_json::to_vec(&snapshot).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Serialization error: {}", e),
        )
    })?;

    let state_hash = crate::types::hash(&snapshot_bytes);

    let metadata = SnapshotMetadata {
        height: snap_height,
        timestamp: snapshot.timestamp,
        state_hash: hex::encode(state_hash),
        size_bytes: snapshot_bytes.len() as u64,
        account_count: snapshot.accounts.len(),
        market_count: snapshot.market_configs.len(),
    };

    Ok(Json(SnapshotExport {
        metadata,
        data: BASE64.encode(&snapshot_bytes),
    }))
}
