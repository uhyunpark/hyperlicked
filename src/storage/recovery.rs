//! Recovery Logic
//!
//! Reconstructs application state from persistent storage on startup.
//!
//! ## Recovery Process
//! 1. Load consensus state (QCs, voted views, current view)
//! 2. Start from the genesis application state
//! 3. Replay every committed block through the application hook
//! 4. Return recovered state ready for consensus to resume

use crate::consensus::Safety;
use crate::types::View;

use super::{AppSnapshot, ConsensusState, PersistentStore};

/// Result of recovery from storage
pub struct RecoveryResult {
    /// Recovered consensus state
    pub consensus_state: ConsensusState,
    /// Genesis app state.  This is intentionally not the latest persisted
    /// snapshot because snapshots omit orderbooks and cannot reconstruct the
    /// canonical exchange state on their own.
    pub snapshot: AppSnapshot,
    /// Always zero for canonical recovery.
    pub snapshot_height: u64,
    /// Safety module initialized with recovered state
    pub safety: Safety,
}

/// Recover state from persistent storage
pub fn recover_from_storage<S: PersistentStore>(store: &S) -> anyhow::Result<RecoveryResult> {
    // 1. Load consensus state (or use genesis)
    let consensus_state = store
        .load_consensus_state()?
        .unwrap_or_else(ConsensusState::genesis);

    // A persisted QC is only meaningful in the exact context that was
    // persisted with it.  Reject mixed-context state before constructing the
    // Safety module; restoring first would make stale certificates observable
    // to consensus code.
    let context = consensus_state.context();
    if let Some(qc) = &consensus_state.high_qc {
        qc.validate_context(context)
            .map_err(|error| anyhow::anyhow!("persisted high QC context mismatch: {error}"))?;
    }
    if let Some(qc) = &consensus_state.locked_qc {
        qc.validate_context(context)
            .map_err(|error| anyhow::anyhow!("persisted locked QC context mismatch: {error}"))?;
    }

    tracing::info!(
        committed_height = consensus_state.committed_height,
        current_view = consensus_state.current_view,
        epoch = consensus_state.epoch,
        committee_hash = %hex::encode(consensus_state.committee_hash),
        "Loaded consensus state"
    );

    // 2. Do not use AppSnapshot for canonical recovery.  It intentionally
    // omits orderbooks and other execution-critical structures, so replay the
    // complete finalized chain from the genesis application state.
    let snapshot_height = 0;
    let snapshot = AppSnapshot::genesis();

    tracing::info!(
        snapshot_height,
        blocks_to_replay = consensus_state
            .committed_height
            .saturating_sub(snapshot_height),
        "Loaded app snapshot"
    );

    // 3. Initialize safety module from consensus state
    let safety = Safety::with_state(
        consensus_state.high_qc.clone(),
        consensus_state.locked_qc.clone(),
        &consensus_state.voted_views,
    );

    Ok(RecoveryResult {
        consensus_state,
        snapshot,
        snapshot_height,
        safety,
    })
}

/// Get blocks that need to be replayed after loading snapshot
pub fn get_blocks_to_replay<S: PersistentStore>(
    store: &S,
    snapshot_height: u64,
    committed_height: u64,
) -> anyhow::Result<Vec<crate::types::Block>> {
    if committed_height <= snapshot_height {
        return Ok(Vec::new());
    }

    let blocks = store.blocks_from_height(snapshot_height + 1)?;

    // Filter to only committed blocks
    let committed_blocks: Vec<_> = blocks
        .into_iter()
        .filter(|b| b.height <= committed_height)
        .collect();

    tracing::info!(
        count = committed_blocks.len(),
        from = snapshot_height + 1,
        to = committed_height,
        "Blocks to replay"
    );

    Ok(committed_blocks)
}

/// Initialize pacemaker at recovered view
pub fn init_pacemaker_view(current_view: View) -> View {
    // Start at the recovered view (don't go backwards)
    current_view
}
