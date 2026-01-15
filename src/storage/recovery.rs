//! Recovery Logic
//!
//! Reconstructs application state from persistent storage on startup.
//!
//! ## Recovery Process
//! 1. Load consensus state (QCs, voted views, current view)
//! 2. Load latest snapshot
//! 3. Replay blocks from snapshot height to committed height
//! 4. Return recovered state ready for consensus to resume

use crate::consensus::Safety;
use crate::types::View;

use super::{AppSnapshot, ConsensusState, PersistentStore};

/// Result of recovery from storage
pub struct RecoveryResult {
    /// Recovered consensus state
    pub consensus_state: ConsensusState,
    /// App snapshot (needs block replay to reach committed state)
    pub snapshot: AppSnapshot,
    /// Height of the snapshot (blocks from here need replay)
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

    tracing::info!(
        committed_height = consensus_state.committed_height,
        current_view = consensus_state.current_view,
        "Loaded consensus state"
    );

    // 2. Load latest snapshot
    let (snapshot_height, snapshot) = store
        .load_latest_snapshot(consensus_state.committed_height)?
        .unwrap_or_else(|| (0, AppSnapshot::genesis()));

    tracing::info!(
        snapshot_height,
        blocks_to_replay = consensus_state.committed_height.saturating_sub(snapshot_height),
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
