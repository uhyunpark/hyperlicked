//! Sync API Routes
//!
//! Block export and snapshot endpoints for node synchronization.
//!
//! ## Endpoints
//!
//! - `GET /api/v1/sync/status` - Node sync status
//! - `GET /api/v1/sync/blocks` - Export block range with pagination
//! - `GET /api/v1/sync/block/:height` - Single block by height
//! - `GET /api/v1/sync/finality/:height` - Export a verified two-chain proof
//! - `GET /api/v1/sync/snapshot/latest` - Latest snapshot metadata
//! - `GET /api/v1/sync/snapshot/:height` - Download full snapshot

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::Serialize;

use crate::api::types::{
    ApiState, BlockExport, BlockRangeQuery, BlockRangeResponse, CertificateExport,
    FinalityProofExport, SnapshotExport, SnapshotMetadata, SyncStatus,
};

use crate::consensus::verify_certificate;
use crate::types::{Block, Certificate, Committee, ConsensusContext, MAX_SYNC_RESPONSE_BYTES};

/// Maximum blocks per request (prevents DoS)
const MAX_BLOCKS_PER_REQUEST: u64 = 1000;
/// Default blocks per request
const DEFAULT_BLOCKS_PER_REQUEST: u64 = 100;

/// Borrowed form used to calculate the exact JSON envelope size without
/// cloning the already-built block exports for every candidate page.
#[derive(Serialize)]
struct BlockRangeResponseView<'a> {
    blocks: &'a [BlockExport],
    #[serde(rename = "nextHeight")]
    next_height: Option<u64>,
    #[serde(rename = "totalAvailable")]
    total_available: u64,
}

fn serialized_range_size(
    encoded_block_bytes: usize,
    block_count: usize,
    next_height: Option<u64>,
    total_available: u64,
) -> Result<usize, serde_json::Error> {
    // The empty response has the same metadata and differs only by the
    // `blocks` array.  Replacing `[]` with the candidate array gives the exact
    // serialized size without repeatedly cloning the export vector.
    let empty = serde_json::to_vec(&BlockRangeResponseView {
        blocks: &[],
        next_height,
        total_available,
    })?;
    let array_bytes = 2usize
        .saturating_add(encoded_block_bytes)
        .saturating_add(block_count.saturating_sub(1));
    Ok(empty.len().saturating_sub(2).saturating_add(array_bytes))
}

/// Convert Certificate to CertificateExport
fn export_certificate(cert: &Certificate) -> CertificateExport {
    CertificateExport {
        epoch: cert.epoch,
        committee_hash: hex::encode(cert.committee_hash),
        genesis_hash: hex::encode(cert.genesis_hash),
        view: cert.view,
        block_hash: hex::encode(cert.block_hash),
        app_hash: cert.app_hash.map(|h| hex::encode(h)),
        voters: cert.voters.iter().map(|v| hex::encode(v)).collect(),
        bls_pubkeys: cert.bls_pubkeys.iter().map(|pk| hex::encode(pk)).collect(),
        agg_signature: hex::encode(&cert.agg_signature),
    }
}

fn export_full_block(block: &Block) -> BlockExport {
    BlockExport {
        epoch: block.epoch,
        committee_hash: hex::encode(block.committee_hash),
        genesis_hash: hex::encode(block.genesis_hash),
        height: block.height,
        view: block.view,
        hash: hex::encode(block.hash()),
        parent_hash: hex::encode(block.parent),
        commitment_root: hex::encode(block.commitment_root),
        app_hash: hex::encode(block.app_hash),
        proposer: hex::encode(block.proposer),
        timestamp: block.timestamp,
        payload_size: block.payload.len(),
        payload: Some(BASE64.encode(&block.payload)),
        justify: block.justify.as_ref().map(export_certificate),
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
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let height = app.committed_height();
    let view = app.current_view();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Expose the same authenticated schema-v3 root that canonical blocks
    // carry in `Block::app_hash`.
    let state_hash = app.compute_state_hash();
    drop(app);

    let store = state.store.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "sync status requires persistent committed-head metadata".to_string(),
        )
    })?;
    let committed_head = store.get_committed_head().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "persistent committed head is unavailable".to_string(),
        )
    })?;
    if committed_head.height != height {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "application height {} disagrees with persisted committed head height {}",
                height, committed_head.height
            ),
        ));
    }
    let indexed_head = store.get_by_height(height).ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "persistent committed height index is unavailable".to_string(),
        )
    })?;
    if indexed_head.hash() != committed_head.hash() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "persisted committed metadata and height index disagree".to_string(),
        ));
    }
    let snapshot_height = store.load_latest_snapshot_height(u64::MAX).ok().flatten();

    Ok(Json(SyncStatus {
        height,
        view,
        committed_hash: hex::encode(committed_head.hash()),
        state_hash: hex::encode(&state_hash),
        timestamp,
        latest_snapshot_height: snapshot_height,
        is_persistent: true,
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
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
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

    // Read only the bounded canonical height window.  Loading the entire
    // height-index tail before applying `limit` would make a small sync
    // request allocate the whole chain history.
    let scan_to = if limit == 0 {
        from.saturating_sub(1)
    } else {
        from.saturating_add(limit.saturating_sub(1)).min(to)
    };
    let total_available = current_height.saturating_sub(from).saturating_add(1);
    let mut exports = Vec::new();
    let mut encoded_block_bytes = 0usize;
    let mut height = from;

    while limit != 0 && height <= scan_to {
        let Some(block) = store.get_by_height(height) else {
            break;
        };

        // The height index is not a trust anchor.  Only the exact canonical
        // height requested by this page may be exported.
        if block.height != height || block.height > current_height {
            break;
        }

        let hash = block.hash();
        let export = BlockExport {
            epoch: block.epoch,
            committee_hash: hex::encode(block.committee_hash),
            genesis_hash: hex::encode(block.genesis_hash),
            height: block.height,
            view: block.view,
            hash: hex::encode(hash),
            parent_hash: hex::encode(block.parent),
            commitment_root: hex::encode(block.commitment_root),
            app_hash: hex::encode(block.app_hash),
            proposer: hex::encode(block.proposer),
            timestamp: block.timestamp,
            payload_size: block.payload.len(),
            payload: if query.include_payload {
                Some(BASE64.encode(&block.payload))
            } else {
                None
            },
            justify: block.justify.as_ref().map(export_certificate),
        };
        let encoded_len = serde_json::to_vec(&export)
            .map_err(|error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Serialization error: {}", error),
                )
            })?
            .len();
        let candidate_next = if block.height < to {
            block.height.checked_add(1)
        } else {
            None
        };
        let candidate_size = serialized_range_size(
            encoded_block_bytes.saturating_add(encoded_len),
            exports.len().saturating_add(1),
            candidate_next,
            total_available,
        )
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Serialization error: {}", error),
            )
        })?;

        if candidate_size > MAX_SYNC_RESPONSE_BYTES {
            if exports.is_empty() {
                return Err((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "single block sync response exceeds {} byte limit",
                        MAX_SYNC_RESPONSE_BYTES
                    ),
                ));
            }
            break;
        }

        encoded_block_bytes = encoded_block_bytes.saturating_add(encoded_len);
        exports.push(export);
        height = match height.checked_add(1) {
            Some(next) => next,
            None => break,
        };
    }

    // Compute next_height for pagination
    let next_height = if !exports.is_empty() {
        let last_height = exports.last().unwrap().height;
        if last_height < to {
            last_height.checked_add(1)
        } else {
            None
        }
    } else {
        None
    };

    let response = BlockRangeResponse {
        blocks: exports,
        next_height,
        total_available,
    };
    let response_size = serde_json::to_vec(&response)
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Serialization error: {}", error),
            )
        })?
        .len();
    if response_size > MAX_SYNC_RESPONSE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "block range response exceeds {} byte limit",
                MAX_SYNC_RESPONSE_BYTES
            ),
        ));
    }

    Ok(Json(response))
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

    // The height index may contain proposal/stale entries above the canonical
    // app height.  Never expose those through a public sync endpoint.
    let committed_height = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned")
        .committed_height();
    if height > committed_height {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Block at height {} is not committed", height),
        ));
    }

    let block = store.get_by_height(height).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Block at height {} not found", height),
        )
    })?;
    if block.height != height || block.height > committed_height {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Canonical block at height {} not found", height),
        ));
    }

    let hash = block.hash();
    let export = BlockExport {
        epoch: block.epoch,
        committee_hash: hex::encode(block.committee_hash),
        genesis_hash: hex::encode(block.genesis_hash),
        height: block.height,
        view: block.view,
        hash: hex::encode(hash),
        parent_hash: hex::encode(block.parent),
        commitment_root: hex::encode(block.commitment_root),
        app_hash: hex::encode(block.app_hash),
        proposer: hex::encode(block.proposer),
        timestamp: block.timestamp,
        payload_size: block.payload.len(),
        payload: Some(BASE64.encode(&block.payload)),
        justify: block.justify.as_ref().map(export_certificate),
    };
    let response_size = serde_json::to_vec(&export)
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Serialization error: {}", error),
            )
        })?
        .len();
    if response_size > MAX_SYNC_RESPONSE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "single block sync response exceeds {} byte limit",
                MAX_SYNC_RESPONSE_BYTES
            ),
        ));
    }

    Ok(Json(export))
}

fn finality_unavailable(error: impl Into<String>) -> (StatusCode, String) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!("finality proof unavailable: {}", error.into()),
    )
}

fn trusted_finality_root(state: &ApiState) -> Result<(ConsensusContext, Committee), String> {
    let app = state
        .shared
        .app
        .read()
        .map_err(|_| "application state lock poisoned".to_string())?;
    let committee = app
        .staking()
        .authoritative_committee()
        .cloned()
        .ok_or_else(|| "authoritative committee is not bound".to_string())?;
    let context = app
        .staking()
        .static_consensus_context()
        .ok_or_else(|| "authoritative consensus context is unavailable".to_string())?;
    committee.validate_context(context)?;
    Ok((context, committee))
}

fn validate_finality_block_shape(
    block: &Block,
    expected_context: ConsensusContext,
    committee: &Committee,
) -> Result<(), String> {
    block.validate_context(expected_context)?;
    block.validate()?;
    if block.height == 0 {
        let genesis = Block::genesis(expected_context);
        if block.hash() != genesis.hash() || block.justify.is_some() {
            return Err("block is not the canonical genesis".to_string());
        }
        return Ok(());
    }
    if committee.member(&block.proposer).is_none() {
        return Err(format!(
            "block {} proposer is not in the trusted committee",
            block.height
        ));
    }
    if committee.leader(block.view) != block.proposer {
        return Err(format!(
            "block {} proposer is not the scheduled committee leader",
            block.height
        ));
    }
    Ok(())
}

fn validate_canonical_target(
    store: &(dyn crate::storage::PersistentStore + Send + Sync),
    target: &Block,
    expected_context: ConsensusContext,
    committee: &Committee,
) -> Result<(), String> {
    validate_finality_block_shape(target, expected_context, committee)?;
    if target.height == 0 {
        return Ok(());
    }

    let parent_height = target
        .height
        .checked_sub(1)
        .ok_or_else(|| "target parent height underflow".to_string())?;
    let parent = store
        .get_by_height(parent_height)
        .ok_or_else(|| format!("canonical parent at height {} is missing", parent_height))?;
    if parent.height != parent_height || parent.hash() != target.parent {
        return Err("target does not point to the exact canonical parent".to_string());
    }
    validate_finality_block_shape(&parent, expected_context, committee)?;
    target.validate_parent_timestamp(parent.timestamp)?;

    if target.height == 1 {
        if target.justify.is_some() {
            return Err("height-1 target must not carry a justification".to_string());
        }
    } else {
        let justify = target
            .justify
            .as_ref()
            .ok_or_else(|| "target is missing its parent justification".to_string())?;
        verify_certificate(
            committee,
            justify,
            expected_context,
            parent.view,
            &parent.hash(),
            Some(&parent.app_hash),
            true,
        )
        .map_err(|error| format!("target parent justification is invalid: {error}"))?;
    }
    Ok(())
}

fn validate_finality_child(
    target: &Block,
    child: &Block,
    commit_qc: &Certificate,
    expected_context: ConsensusContext,
    committee: &Committee,
) -> Result<(), String> {
    validate_finality_block_shape(child, expected_context, committee)?;
    let expected_height = target
        .height
        .checked_add(1)
        .ok_or_else(|| "finality child height overflows u64".to_string())?;
    if child.height != expected_height || child.parent != target.hash() {
        return Err("finality child is not the exact child of target".to_string());
    }
    child.validate_parent_timestamp(target.timestamp)?;

    let justify = child
        .justify
        .as_ref()
        .ok_or_else(|| "finality child is missing the QC for target".to_string())?;
    verify_certificate(
        committee,
        justify,
        expected_context,
        target.view,
        &target.hash(),
        Some(&target.app_hash),
        true,
    )
    .map_err(|error| format!("child justification is invalid: {error}"))?;
    verify_certificate(
        committee,
        commit_qc,
        expected_context,
        child.view,
        &child.hash(),
        Some(&child.app_hash),
        true,
    )
    .map_err(|error| format!("commit QC is invalid: {error}"))?;
    Ok(())
}

fn validate_commit_descendant(
    child: &Block,
    grandchild: &Block,
    expected_context: ConsensusContext,
    committee: &Committee,
) -> Result<Certificate, String> {
    validate_finality_block_shape(grandchild, expected_context, committee)?;
    let expected_height = child
        .height
        .checked_add(1)
        .ok_or_else(|| "commit descendant height overflows u64".to_string())?;
    if grandchild.height != expected_height || grandchild.parent != child.hash() {
        return Err("commit descendant is not the exact child of the proof child".to_string());
    }
    grandchild.validate_parent_timestamp(child.timestamp)?;
    let commit_qc = grandchild
        .justify
        .clone()
        .ok_or_else(|| "commit descendant is missing its child QC".to_string())?;
    verify_certificate(
        committee,
        &commit_qc,
        expected_context,
        child.view,
        &child.hash(),
        Some(&child.app_hash),
        true,
    )
    .map_err(|error| format!("commit descendant QC is invalid: {error}"))?;
    Ok(commit_qc)
}

/// Export the exact two-chain proof for a committed target height.
///
/// A target below the persisted committed head uses canonical descendants and
/// their justify QC.  The current head uses the persisted high/locked QC and
/// the exact speculative block addressed by that QC.  Missing or invalid
/// evidence is deliberately reported as 503 rather than downgraded to an
/// unverifiable response.
pub async fn get_finality_proof(
    State(state): State<ApiState>,
    Path(height): Path<u64>,
) -> Result<Json<FinalityProofExport>, (StatusCode, String)> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(|| finality_unavailable("persistence is not enabled"))?;
    let (trusted_context, committee) =
        trusted_finality_root(&state).map_err(finality_unavailable)?;
    let consensus = store
        .load_consensus_state()
        .map_err(|error| finality_unavailable(format!("consensus state read failed: {error}")))?
        .ok_or_else(|| finality_unavailable("persisted consensus state is missing"))?;
    let persisted_context = consensus.context();
    if persisted_context != trusted_context {
        return Err(finality_unavailable(
            "persisted consensus context does not match the trusted runtime context",
        ));
    }
    let app_height = state
        .shared
        .app
        .read()
        .map_err(|_| finality_unavailable("application state lock poisoned"))?
        .committed_height();
    if app_height != consensus.committed_height {
        return Err(finality_unavailable(
            "application and persisted committed heights disagree",
        ));
    }
    let committed_head = store
        .get_committed_head()
        .ok_or_else(|| finality_unavailable("persisted committed block head is missing"))?;
    if committed_head.height != consensus.committed_height
        || committed_head.hash() != consensus.committed_hash
    {
        return Err(finality_unavailable(
            "persisted committed block head and consensus metadata disagree",
        ));
    }
    if height > consensus.committed_height {
        return Err(finality_unavailable(format!(
            "height {} is above the persisted committed head",
            height
        )));
    }

    let target = store.get_by_height(height).ok_or_else(|| {
        finality_unavailable(format!("canonical target at height {} is missing", height))
    })?;
    if target.height != height {
        return Err(finality_unavailable(
            "height index returned a block with the wrong height",
        ));
    }
    if height == consensus.committed_height && target.hash() != consensus.committed_hash {
        return Err(finality_unavailable(
            "persisted committed hash does not match the exact target block",
        ));
    }
    validate_canonical_target(store.as_ref(), &target, trusted_context, &committee)
        .map_err(finality_unavailable)?;

    let child_height = height
        .checked_add(1)
        .ok_or_else(|| finality_unavailable("finality child height overflows u64"))?;
    let mut candidates: Vec<(Block, Certificate)> = Vec::new();

    if height < consensus.committed_height {
        let child = store.get_by_height(child_height).ok_or_else(|| {
            finality_unavailable(format!(
                "canonical finality child at height {} is missing",
                child_height
            ))
        })?;
        if child.height != child_height || child.parent != target.hash() {
            return Err(finality_unavailable(
                "canonical finality child does not extend the exact target",
            ));
        }

        if let Some(grandchild) = child_height
            .checked_add(1)
            .and_then(|h| store.get_by_height(h))
        {
            if grandchild.height <= consensus.committed_height
                && grandchild.height == child_height.saturating_add(1)
                && grandchild.parent == child.hash()
            {
                if let Ok(commit_qc) =
                    validate_commit_descendant(&child, &grandchild, trusted_context, &committee)
                {
                    candidates.push((child.clone(), commit_qc));
                }
            }
        }

        // A historical target immediately below the committed head may not
        // have a canonical grandchild yet; a persisted QC for this exact
        // canonical child is still a valid commit certificate.
        for qc in [consensus.high_qc.as_ref(), consensus.locked_qc.as_ref()]
            .into_iter()
            .flatten()
        {
            if qc.block_hash == child.hash() {
                candidates.push((child.clone(), qc.clone()));
            }
        }
    } else {
        // The current committed target's child can remain speculative, so it
        // must be addressed by the persisted QC hash rather than a height
        // index that may be absent or stale.
        for qc in [consensus.high_qc.as_ref(), consensus.locked_qc.as_ref()]
            .into_iter()
            .flatten()
        {
            if qc.block_hash != target.hash() {
                let Some(child) = store.get(&qc.block_hash) else {
                    continue;
                };
                if child.height == child_height && child.parent == target.hash() {
                    candidates.push((child, qc.clone()));
                }
            }
        }
    }

    for (child, commit_qc) in candidates {
        if validate_finality_child(&target, &child, &commit_qc, trusted_context, &committee).is_ok()
        {
            let response = FinalityProofExport {
                target: export_full_block(&target),
                child: export_full_block(&child),
                commit_qc: export_certificate(&commit_qc),
            };
            let size = serde_json::to_vec(&response)
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
                .len();
            if size > MAX_SYNC_RESPONSE_BYTES {
                return Err((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "finality proof exceeds {} byte limit",
                        MAX_SYNC_RESPONSE_BYTES
                    ),
                ));
            }
            return Ok(Json(response));
        }
    }

    Err(finality_unavailable(
        "no trusted QC proves the exact child of the requested target",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::SharedState;
    use crate::app::staking::{StaticValidatorBootstrap, MIN_SELF_STAKE};
    use crate::app::AppState;
    use crate::consensus::{form_certificate, AppHook, BlockStore};
    use crate::crypto::bls::BlsSecretKey;
    use crate::storage::{ConsensusState, PersistentStore, RocksDbStore};
    use crate::types::{Block, CommitmentV2, ConsensusConfig, ConsensusContext, Vote};
    use axum::extract::State;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn stale_store_and_state() -> (ApiState, TempDir) {
        let shared = SharedState::new(AppState::new());
        let directory = TempDir::new().unwrap();
        let store = Arc::new(RocksDbStore::open(directory.path()).unwrap());
        let mut stale_block = Block::genesis(ConsensusContext::new(0, [7u8; 32]));
        stale_block.height = 7;
        store.save(&stale_block);
        (ApiState::with_store(shared, store), directory)
    }

    fn finality_state() -> (ApiState, TempDir, Block, Block) {
        let voters: Vec<_> = (1u8..=4).map(|id| [id; 32]).collect();
        let secrets: Vec<_> = (1u8..=4)
            .map(|id| {
                let mut seed = [0u8; 32];
                seed[0] = id;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id: voters[0],
            validators: voters.clone(),
            voting_powers: vec![1; 4],
            view_timeout_ms: 1000,
            bls_pubkeys: secrets
                .iter()
                .map(|secret| secret.public_key().to_bytes().to_vec())
                .collect(),
            bls_secret_key: Some(secrets[0].to_bytes()),
        };
        let committee = config.committee().unwrap();
        let context = committee.context_with_genesis(0, [9u8; 32]);
        let mut app = AppState::new_with_chain_domain(context.genesis_hash);
        app.set_consensus_context(context);
        let bootstrap: Vec<_> = voters
            .iter()
            .zip(secrets.iter())
            .map(|(node_id, secret)| StaticValidatorBootstrap {
                operator: format!("system:genesis:{}", hex::encode(node_id)),
                node_id: *node_id,
                voting_power: 1,
                bls_pubkey: secret.public_key().to_bytes().to_vec(),
                bls_proof_of_possession: secret
                    .create_proof_of_possession(&context.genesis_hash, node_id)
                    .to_bytes()
                    .to_vec(),
                self_stake: MIN_SELF_STAKE,
                commission_bps: 0,
            })
            .collect();
        app.bootstrap_static_committee(&committee, &bootstrap)
            .unwrap();
        app.bind_authoritative_committee(committee.clone()).unwrap();

        let genesis = Block::genesis(context);
        let mut target = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            height: 1,
            view: 0,
            parent: genesis.hash(),
            payload: vec![],
            proposer: committee.leader(0),
            commitment_root: CommitmentV2::default().root().unwrap(),
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        target.app_hash = app.execute(&target);
        let target_hash = target.hash();

        let votes_for_target: Vec<_> = secrets
            .iter()
            .zip(voters.iter())
            .take(3)
            .map(|(secret, voter)| {
                Vote::new_bls(
                    context,
                    target.view,
                    target_hash,
                    target.app_hash,
                    *voter,
                    secret,
                )
            })
            .collect();
        let justify = form_certificate(&committee, context, votes_for_target, true).unwrap();

        let child = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            height: 2,
            view: 1,
            parent: target_hash,
            payload: vec![],
            proposer: committee.leader(1),
            commitment_root: CommitmentV2::default().root().unwrap(),
            app_hash: [2u8; 32],
            timestamp: 2,
            justify: Some(justify),
        };
        let votes_for_child: Vec<_> = secrets
            .iter()
            .zip(voters.iter())
            .take(3)
            .map(|(secret, voter)| {
                Vote::new_bls(
                    context,
                    child.view,
                    child.hash(),
                    child.app_hash,
                    *voter,
                    secret,
                )
            })
            .collect();
        let commit_qc = form_certificate(&committee, context, votes_for_child, true).unwrap();

        let directory = TempDir::new().unwrap();
        let store = Arc::new(RocksDbStore::open(directory.path()).unwrap());
        store.save(&genesis);
        store.save(&target);
        store.save_block(&child).unwrap();
        store.set_committed(&target.hash());
        store
            .save_consensus_state(&ConsensusState {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                high_qc: Some(commit_qc),
                locked_qc: None,
                voted_views: vec![],
                current_view: child.view,
                committed_height: target.height,
                committed_hash: target.hash(),
                consecutive_timeouts: 0,
                vc_sent_for_view: None,
            })
            .unwrap();

        let shared = SharedState::new(app);
        (
            ApiState::with_store(shared, store),
            directory,
            target,
            child,
        )
    }

    fn historical_finality_state() -> (ApiState, TempDir, Block, Block, Block) {
        let (state, directory, target, child) = finality_state();
        let store = state.store.as_ref().unwrap();
        let (context, committee) = trusted_finality_root(&state).unwrap();
        let child_qc = store
            .load_consensus_state()
            .unwrap()
            .unwrap()
            .high_qc
            .expect("fixture child QC");
        let grandchild = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            height: child.height + 1,
            view: child.view + 1,
            parent: child.hash(),
            payload: vec![],
            proposer: committee.leader(child.view + 1),
            commitment_root: CommitmentV2::default().root().unwrap(),
            app_hash: [3u8; 32],
            timestamp: child.timestamp + 1,
            justify: Some(child_qc),
        };

        // The endpoint requires the application and persisted committed head
        // to agree. Advance the fixture app to the same historical head; the
        // proof itself remains authenticated only by block/QC validation.
        {
            let mut app = state.shared.app.write().unwrap();
            app.execute(&child);
            app.execute(&grandchild);
        }
        // The base fixture keeps the child speculative for the head-proof
        // case. Historical proofs require both descendants in the canonical
        // height index so the endpoint can select the grandchild justify QC.
        store.save(&child);
        store.save(&grandchild);
        store.set_committed(&grandchild.hash());
        let mut consensus = store.load_consensus_state().unwrap().unwrap();
        consensus.high_qc = None;
        consensus.locked_qc = None;
        consensus.current_view = grandchild.view;
        consensus.committed_height = grandchild.height;
        consensus.committed_hash = grandchild.hash();
        store.save_consensus_state(&consensus).unwrap();

        (state, directory, target, child, grandchild)
    }

    fn persist_historical_head(state: &ApiState, grandchild: &Block) {
        let store = state.store.as_ref().unwrap();
        store.save(grandchild);
        store.set_committed(&grandchild.hash());
        let mut consensus = store.load_consensus_state().unwrap().unwrap();
        consensus.high_qc = None;
        consensus.locked_qc = None;
        consensus.committed_height = grandchild.height;
        consensus.committed_hash = grandchild.hash();
        store.save_consensus_state(&consensus).unwrap();
    }

    #[test]
    fn serialized_range_size_accepts_exact_limit_and_rejects_one_byte_over() {
        let empty_size = serialized_range_size(0, 0, None, 0).unwrap();
        let item_size = MAX_SYNC_RESPONSE_BYTES - empty_size;
        assert_eq!(
            serialized_range_size(item_size, 1, None, 0).unwrap(),
            MAX_SYNC_RESPONSE_BYTES
        );
        assert!(
            serialized_range_size(item_size + 1, 1, None, 0).unwrap() > MAX_SYNC_RESPONSE_BYTES
        );
    }

    #[tokio::test]
    async fn single_block_does_not_expose_uncommitted_height_index() {
        let (state, _directory) = stale_store_and_state();

        let error = get_block_by_height(State(state), Path(7))
            .await
            .expect_err("uncommitted block must not be exported");
        assert_eq!(error.0, StatusCode::NOT_FOUND);
        assert!(error.1.contains("not committed"));
    }

    #[tokio::test]
    async fn range_does_not_expose_uncommitted_height_index() {
        let (state, _directory) = stale_store_and_state();

        let response = get_blocks(
            State(state),
            Query(BlockRangeQuery {
                from: 0,
                to: Some(7),
                limit: None,
                include_payload: false,
            }),
        )
        .await
        .expect("range should clamp to committed height");
        assert!(response.0.blocks.iter().all(|block| block.height <= 0));
        assert!(response.0.blocks.iter().all(|block| block.height != 7));
    }

    #[tokio::test]
    async fn finality_endpoint_returns_valid_head_proof() {
        let (state, _directory, target, child) = finality_state();
        let response = get_finality_proof(State(state), Path(target.height))
            .await
            .expect("valid persisted two-chain proof should export")
            .0;
        assert_eq!(response.target.height, target.height);
        assert_eq!(response.target.hash, hex::encode(target.hash()));
        assert_eq!(response.child.height, child.height);
        assert_eq!(response.child.parent_hash, hex::encode(target.hash()));
        assert!(!response.commit_qc.agg_signature.is_empty());
    }

    #[tokio::test]
    async fn historical_finality_endpoint_uses_canonical_grandchild_qc() {
        let (state, _directory, target, child, grandchild) = historical_finality_state();
        let response = get_finality_proof(State(state), Path(target.height))
            .await
            .expect("historical canonical two-chain proof should export")
            .0;
        assert_eq!(response.target.hash, hex::encode(target.hash()));
        assert_eq!(response.child.hash, hex::encode(child.hash()));
        assert_eq!(
            response.commit_qc.block_hash,
            hex::encode(grandchild.justify.as_ref().unwrap().block_hash)
        );
        assert_eq!(response.commit_qc.view, child.view);
    }

    #[tokio::test]
    async fn historical_finality_endpoint_fails_closed_for_forged_grandchild_qc() {
        for corruption in 0..4 {
            let (state, _directory, target, child, grandchild) = historical_finality_state();
            let mut forged = grandchild.clone();
            let justify = forged.justify.as_mut().unwrap();
            match corruption {
                0 => justify.block_hash = target.hash(),
                1 => justify.app_hash = Some(target.app_hash),
                2 => justify.view = forged.view,
                3 => justify.agg_signature[0] ^= 1,
                _ => unreachable!(),
            }
            // Keep the same historical committed height while replacing only
            // the canonical grandchild row under test.
            persist_historical_head(&state, &forged);

            let error = get_finality_proof(State(state), Path(target.height))
                .await
                .expect_err("forged grandchild QC must not be exported");
            assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(forged.parent, child.hash());
        }
    }

    #[tokio::test]
    async fn historical_finality_endpoint_rejects_inconsistent_persisted_head() {
        let (state, _directory, target, _child, _grandchild) = historical_finality_state();
        let store = state.store.as_ref().unwrap();
        store.set_committed(&target.hash());

        let error = get_finality_proof(State(state), Path(target.height))
            .await
            .expect_err("persisted head and consensus metadata mismatch must fail closed");
        assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.1.contains("consensus metadata disagree"));
    }

    #[tokio::test]
    async fn finality_endpoint_fails_closed_without_persisted_qc() {
        let (state, _directory, target, _child) = finality_state();
        let store = state.store.as_ref().unwrap();
        let mut consensus = store.load_consensus_state().unwrap().unwrap();
        consensus.high_qc = None;
        store.save_consensus_state(&consensus).unwrap();

        let error = get_finality_proof(State(state), Path(target.height))
            .await
            .expect_err("missing terminal QC must not produce an unverifiable proof");
        assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn sync_status_uses_exact_persisted_committed_hash() {
        let (state, _directory, target, _child) = finality_state();
        let response = get_sync_status(State(state))
            .await
            .expect("persisted committed head should make status available")
            .0;
        assert_eq!(response.committed_hash, hex::encode(target.hash()));
        assert_ne!(response.committed_hash, response.state_hash);
    }

    #[tokio::test]
    async fn sync_status_fails_closed_without_persistent_head() {
        let state = ApiState::new(SharedState::new(AppState::new()));
        let error = get_sync_status(State(state))
            .await
            .expect_err("status must not invent a committed hash without persistence");
        assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    }
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

    let (height, snapshot) = store
        .load_latest_snapshot(u64::MAX)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Storage error: {}", e),
            )
        })?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "No snapshots available".to_string()))?;

    // Compute size using the same bounded snapshot codec used by storage.
    let snapshot_bytes = snapshot.to_bounded_json().map_err(|error| {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("Snapshot is not exportable: {error}"),
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
    let (snap_height, snapshot) = store
        .load_latest_snapshot(height)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Storage error: {}", e),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("No snapshot at or before height {}", height),
            )
        })?;

    // Serialize snapshot through the bounded codec before creating the
    // base64 HTTP envelope.
    let snapshot_bytes = snapshot.to_bounded_json().map_err(|error| {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("Snapshot is not exportable: {error}"),
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

    // Standard base64 characters never need JSON escaping. Size the complete
    // envelope before allocating the encoded payload so an oversized export
    // cannot transiently allocate well beyond the HTTP response budget.
    let empty_response_size = serde_json::to_vec(&SnapshotExport {
        metadata: metadata.clone(),
        data: String::new(),
    })
    .map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Serialization error: {}", error),
        )
    })?
    .len();
    let encoded_len = base64::encoded_len(snapshot_bytes.len(), true).ok_or_else(|| {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "snapshot base64 length overflow".to_string(),
        )
    })?;
    let response_size = empty_response_size.saturating_add(encoded_len);
    if response_size > MAX_SYNC_RESPONSE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "snapshot HTTP response exceeds {} byte limit",
                MAX_SYNC_RESPONSE_BYTES
            ),
        ));
    }

    let response = SnapshotExport {
        metadata,
        data: BASE64.encode(&snapshot_bytes),
    };

    Ok(Json(response))
}
