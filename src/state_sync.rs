//! Authenticated, replay-first block import.
//!
//! Snapshot bytes are deliberately not an application import boundary.  An
//! [`AppSnapshot`](crate::storage::AppSnapshot) does not contain orderbooks,
//! so accepting one as canonical state would make the resulting state root
//! depend on local reconstruction.  This module instead verifies the complete
//! block prefix against a private [`CanonicalAppHook`] and only then promotes
//! it one block at a time.

use crate::api::{CanonicalAppHook, SharedState};
use crate::consensus::{verify_certificate, AppHook};
use crate::storage::{ConsensusState, PersistentStore};
use crate::types::{Block, Certificate, CommitmentV2, Committee, ConsensusContext};

/// A stateless importer.  Keeping the operation on a zero-sized type makes
/// its trust boundary explicit: all authority is supplied by the caller.
pub struct VerifiedBlockImporter;

/// Import a finalized prefix after authenticating it against the trusted
/// committee and the exact local application/store anchor.
///
/// `blocks` contains finalized blocks in height order. `commit_child` is the
/// one unfinalized child that proves the terminal two-chain commit, and
/// `child_qc` is its QC. No network request/response type is involved in this
/// boundary.
pub fn import_verified_blocks(
    app: &mut CanonicalAppHook,
    store: &dyn PersistentStore,
    context: ConsensusContext,
    committee: &Committee,
    blocks: &[Block],
    commit_child: &Block,
    child_qc: &Certificate,
) -> Result<(), String> {
    VerifiedBlockImporter::import(
        app,
        store,
        context,
        committee,
        blocks,
        commit_child,
        child_qc,
    )
}

impl VerifiedBlockImporter {
    /// See [`import_verified_blocks`].
    pub fn import(
        app: &mut CanonicalAppHook,
        store: &dyn PersistentStore,
        context: ConsensusContext,
        committee: &Committee,
        blocks: &[Block],
        commit_child: &Block,
        child_qc: &Certificate,
    ) -> Result<(), String> {
        let plan = ImportPlan::build(
            app,
            store,
            context,
            committee,
            blocks,
            commit_child,
            child_qc,
        )?;
        plan.apply(app, store, context, blocks, commit_child, child_qc)
    }
}

#[derive(Clone)]
struct VerifiedBlock {
    commitment: CommitmentV2,
}

struct PlannedCommit {
    block_index: usize,
    trigger_child: Block,
    state: ConsensusState,
    commitment: CommitmentV2,
}

struct ImportPlan {
    planned_commits: Vec<PlannedCommit>,
    final_state: ConsensusState,
    final_commitment: CommitmentV2,
    state_needs_repair: bool,
}

impl ImportPlan {
    fn build(
        app: &CanonicalAppHook,
        store: &dyn PersistentStore,
        context: ConsensusContext,
        committee: &Committee,
        blocks: &[Block],
        commit_child: &Block,
        child_qc: &Certificate,
    ) -> Result<Self, String> {
        if blocks.is_empty() {
            return Err("verified import requires at least one finalized block".to_string());
        }
        committee
            .validate_context(context)
            .map_err(|error| format!("trusted committee context: {error}"))?;
        if !committee.bls_enabled() {
            return Err("verified import requires a trusted committee with BLS keys".to_string());
        }

        validate_block_size(commit_child)?;
        commit_child.validate_context(context)?;
        commit_child.validate()?;

        let anchor = store.get_committed_head().ok_or_else(|| {
            "verified import requires an exact durable committed head".to_string()
        })?;
        anchor.validate_context(context)?;
        anchor.validate()?;
        let indexed_anchor = store.get_by_height(anchor.height).ok_or_else(|| {
            "durable anchor is missing from the canonical height index".to_string()
        })?;
        if indexed_anchor.hash() != anchor.hash() {
            return Err(
                "durable committed head disagrees with its canonical height index".to_string(),
            );
        }

        let live_shared = app.shared_state();
        let live_state = live_shared.app.read().map_err(|_| {
            "canonical application read lock poisoned during verified import".to_string()
        })?;
        if live_state.committed_height() != anchor.height {
            return Err(format!(
                "application height {} does not match durable anchor height {}",
                live_state.committed_height(),
                anchor.height
            ));
        }
        if live_state.chain_domain() != context.genesis_hash {
            return Err("application chain domain does not match trusted context".to_string());
        }
        if live_state.current_epoch() != context.epoch {
            return Err(format!(
                "application epoch {} does not match trusted context epoch {}",
                live_state.current_epoch(),
                context.epoch
            ));
        }
        if anchor.height > 0 && live_state.compute_full_state_root() != anchor.app_hash {
            return Err("application state root does not match durable anchor".to_string());
        }
        drop(live_state);

        let live_hash = app.exact_committed_hash();
        if anchor.height > 0 {
            if live_hash != Some(anchor.hash()) {
                return Err("application head hash does not match durable anchor".to_string());
            }
        } else if let Some(live_hash) = live_hash {
            if live_hash != anchor.hash() {
                return Err(
                    "genesis application head hash does not match durable anchor".to_string(),
                );
            }
        }

        let consensus_state = load_anchor_consensus_state(store, &anchor)?;
        validate_persisted_safety_state(store, context, committee, &anchor, &consensus_state)?;
        validate_durable_anchor_metadata(store, &anchor)?;

        validate_chain_shape(
            store,
            context,
            committee,
            &anchor,
            blocks,
            commit_child,
            child_qc,
        )?;

        // A retry may observe a durable prefix produced by a previous call.
        // Only skip blocks whose exact body, commitment, and state-root
        // records are already present. Any mismatch fails before a write.
        let next_index = resume_index(store, &anchor, blocks)?;

        // Validate every already durable imported block as well as the new
        // suffix. This prevents a corrupted prefix from becoming the parent
        // of an otherwise valid replay.
        validate_durable_prefix(store, blocks, next_index)?;

        let verified = replay_on_private_hook(app, &anchor, &blocks[next_index..], commit_child)?;

        // Compute every safety-state transition, QC dependency, commitment
        // lookup, and checked view increment before the first speculative
        // write. In particular, a high QC at `u64::MAX` must fail here while
        // the durable store and live application are still untouched.
        let mut state = consensus_state.clone();
        let mut planned_commits = Vec::with_capacity(blocks.len() - next_index);
        for index in next_index..blocks.len() {
            let block = &blocks[index];
            let trigger_child = blocks.get(index + 1).unwrap_or(commit_child);
            let locked_qc = trigger_child.justify.clone().ok_or_else(|| {
                format!(
                    "trigger child at height {} is missing the QC for finalized block {}",
                    trigger_child.height, block.height
                )
            })?;
            verify_qc_for_block(
                committee,
                context,
                &locked_qc,
                block,
                "trigger-child locked QC",
            )?;

            let high_qc = if index + 1 < blocks.len() {
                blocks
                    .get(index + 2)
                    .and_then(|child_of_child| child_of_child.justify.clone())
                    .or_else(|| commit_child.justify.clone())
                    .ok_or_else(|| {
                        format!(
                            "child-of-child is missing the QC for trigger child {}",
                            trigger_child.height
                        )
                    })?
            } else {
                child_qc.clone()
            };
            verify_qc_for_block(
                committee,
                context,
                &high_qc,
                trigger_child,
                "trigger-child high QC",
            )?;

            let commitment = verified
                .get(index - next_index)
                .ok_or_else(|| {
                    format!(
                        "missing private replay result for finalized block {}",
                        block.height
                    )
                })?
                .commitment
                .clone();
            let next_state =
                safety_state_for_commit(&state, context, block, Some(high_qc), Some(locked_qc))?;
            planned_commits.push(PlannedCommit {
                block_index: index,
                trigger_child: trigger_child.clone(),
                state: next_state.clone(),
                commitment,
            });
            state = next_state;
        }

        let final_block = blocks
            .last()
            .ok_or_else(|| "verified import has no terminal finalized block".to_string())?;
        let final_locked_qc = commit_child
            .justify
            .clone()
            .ok_or_else(|| "commit child is missing the terminal locked QC".to_string())?;
        verify_qc_for_block(
            committee,
            context,
            &final_locked_qc,
            final_block,
            "terminal locked QC",
        )?;
        verify_certificate_for_block(
            committee,
            context,
            child_qc,
            commit_child,
            "terminal child QC",
        )?;
        let final_state = safety_state_for_commit(
            &state,
            context,
            final_block,
            Some(child_qc.clone()),
            Some(final_locked_qc),
        )?;
        let final_commitment = if let Some(last) = verified.last() {
            last.commitment.clone()
        } else {
            store
                .load_commitment(&final_block.hash())
                .map_err(|error| format!("loading existing terminal commitment: {error}"))?
                .ok_or_else(|| "missing private replay result for terminal block".to_string())?
        };

        // A non-empty planned suffix writes the exact final state as its last
        // atomic commit. Only a fully durable retry can need the terminal
        // metadata repair, so compare that case before any speculative row
        // is touched.
        let state_needs_repair = if planned_commits.is_empty() {
            let persisted_state = store
                .load_consensus_state()
                .map_err(|error| format!("loading post-import consensus state: {error}"))?;
            match persisted_state {
                Some(persisted) => {
                    serde_json::to_vec(&persisted).map_err(|error| {
                        format!("encoding persisted consensus state for comparison: {error}")
                    })? != serde_json::to_vec(&final_state).map_err(|error| {
                        format!("encoding final consensus state for comparison: {error}")
                    })?
                }
                None => true,
            }
        } else {
            false
        };

        Ok(Self {
            planned_commits,
            final_state,
            final_commitment,
            state_needs_repair,
        })
    }

    fn apply(
        self,
        app: &mut CanonicalAppHook,
        store: &dyn PersistentStore,
        context: ConsensusContext,
        blocks: &[Block],
        commit_child: &Block,
        child_qc: &Certificate,
    ) -> Result<(), String> {
        // The scratch replay is complete before this method is entered. The
        // only state-changing operation before each atomic finalized commit
        // is the bounded speculative child write required for crash recovery.
        for planned in &self.planned_commits {
            let block = &blocks[planned.block_index];

            // Persist only the one child needed to make this prefix
            // recoverable. The store's speculative boundary is bounded and
            // first-write-wins, so a retry cannot replace an authenticated
            // body for the same hash.
            store
                .save_speculative(&planned.trigger_child)
                .map_err(|error| format!("persisting speculative trigger child: {error}"))?;

            store
                .commit_block_with_commitment_and_state_root(
                    block,
                    &planned.state,
                    Some(&planned.commitment),
                    Some(&block.app_hash),
                )
                .map_err(|error| {
                    format!(
                        "atomic finalized commit at height {}: {error}",
                        block.height
                    )
                })?;

            let live_hash = app.commit(block).map_err(|error| {
                format!(
                    "live application commit at height {}: {error}",
                    block.height
                )
            })?;
            if live_hash != block.app_hash {
                return Err(format!(
                    "live application commit returned the wrong root at height {}",
                    block.height
                ));
            }
        }

        // If a process crashed after the last finalized write, a retry may
        // have no blocks left to promote. Repair only the final safety state
        // when necessary; the finalized block write is idempotent at the
        // persistent boundary and the live application is already at it.
        let final_block = blocks
            .last()
            .ok_or_else(|| "verified import has no terminal finalized block".to_string())?;

        store
            .save_speculative(commit_child)
            .map_err(|error| format!("persisting terminal speculative child: {error}"))?;

        if self.state_needs_repair {
            store
                .commit_block_with_commitment_and_state_root(
                    final_block,
                    &self.final_state,
                    Some(&self.final_commitment),
                    Some(&final_block.app_hash),
                )
                .map_err(|error| format!("repairing terminal consensus metadata: {error}"))?;
        }

        // Publish only the already-authenticated terminal child candidate.
        // This is the in-memory half of the same recovery boundary as the
        // speculative journal row above; it runs after every finalized block,
        // root, commitment, and QC has passed.
        app.restore_speculative_chain(context, final_block, &[commit_child.clone()])?;

        verify_import_result(app, store, context, child_qc, commit_child, final_block)?;
        Ok(())
    }
}

fn validate_block_size(block: &Block) -> Result<(), String> {
    let bytes = bincode::serialize(block)
        .map_err(|error| format!("serializing block for import limit: {error}"))?;
    if bytes.len() > crate::consensus::MAX_SPECULATIVE_BLOCK_BYTES {
        return Err(format!(
            "block at height {} exceeds the {} byte import bound",
            block.height,
            crate::consensus::MAX_SPECULATIVE_BLOCK_BYTES
        ));
    }
    Ok(())
}

fn require_leader(committee: &Committee, block: &Block) -> Result<(), String> {
    if committee.member(&block.proposer).is_none() {
        return Err(format!(
            "block proposer {} is not in the trusted committee",
            hex::encode(block.proposer)
        ));
    }
    if committee.leader(block.view) != block.proposer {
        return Err(format!(
            "block proposer {} is not the trusted leader for view {}",
            hex::encode(block.proposer),
            block.view
        ));
    }
    Ok(())
}

fn verify_certificate_for_block(
    committee: &Committee,
    context: ConsensusContext,
    certificate: &Certificate,
    target: &Block,
    boundary: &str,
) -> Result<(), String> {
    verify_certificate(
        committee,
        certificate,
        context,
        target.view,
        &target.hash(),
        Some(&target.app_hash),
        true,
    )
    .map_err(|error| format!("{boundary}: {error}"))
}

fn verify_qc_for_block(
    committee: &Committee,
    context: ConsensusContext,
    certificate: &Certificate,
    target: &Block,
    boundary: &str,
) -> Result<(), String> {
    verify_certificate_for_block(committee, context, certificate, target, boundary)
}

fn load_anchor_consensus_state(
    store: &dyn PersistentStore,
    anchor: &Block,
) -> Result<ConsensusState, String> {
    let state = store
        .load_consensus_state()
        .map_err(|error| format!("loading durable consensus state: {error}"))?
        .ok_or_else(|| {
            format!(
                "durable committed head at height {} has no consensus state",
                anchor.height
            )
        })?;
    Ok(state)
}

fn validate_durable_anchor_metadata(
    store: &dyn PersistentStore,
    anchor: &Block,
) -> Result<(), String> {
    if anchor.height == 0 {
        return Ok(());
    }
    let root = store
        .load_state_root(&anchor.hash())
        .map_err(|error| format!("loading durable anchor state root: {error}"))?
        .ok_or_else(|| "durable non-genesis anchor has no state-root record".to_string())?;
    if root != anchor.app_hash {
        return Err("durable anchor state-root record does not match its block".to_string());
    }
    let commitment = store
        .load_commitment(&anchor.hash())
        .map_err(|error| format!("loading durable anchor commitment: {error}"))?
        .ok_or_else(|| "durable non-genesis anchor has no Commitment v2 record".to_string())?;
    let root = commitment
        .root()
        .map_err(|error| format!("durable anchor commitment root: {error}"))?;
    if root != anchor.commitment_root {
        return Err("durable anchor commitment does not match its block".to_string());
    }
    Ok(())
}

fn validate_persisted_safety_state(
    store: &dyn PersistentStore,
    context: ConsensusContext,
    committee: &Committee,
    anchor: &Block,
    state: &ConsensusState,
) -> Result<(), String> {
    if state.context() != context {
        return Err("durable consensus state context does not match trusted context".to_string());
    }
    if state.committed_height != anchor.height || state.committed_hash != anchor.hash() {
        return Err("durable consensus state does not match exact committed anchor".to_string());
    }
    for (name, certificate) in [
        ("durable high QC", state.high_qc.as_ref()),
        ("durable locked QC", state.locked_qc.as_ref()),
    ] {
        let Some(certificate) = certificate else {
            continue;
        };
        let target = store
            .get(&certificate.block_hash)
            .ok_or_else(|| format!("{name} references a block missing from durable storage"))?;
        target.validate_context(context)?;
        target.validate()?;
        if target.height > anchor.height.saturating_add(1) {
            return Err(format!(
                "{name} points beyond the recoverable child boundary"
            ));
        }
        if target.height == anchor.height.saturating_add(1) && target.parent != anchor.hash() {
            return Err(format!("{name} points to a non-child speculative branch"));
        }
        if target.height <= anchor.height {
            let canonical = store.get_by_height(target.height).ok_or_else(|| {
                format!("{name} target is absent from the canonical height index")
            })?;
            if canonical.hash() != target.hash() {
                return Err(format!("{name} points to a non-canonical branch"));
            }
        }
        verify_certificate_for_block(committee, context, certificate, &target, name)?;
    }
    Ok(())
}

fn validate_chain_shape(
    store: &dyn PersistentStore,
    context: ConsensusContext,
    committee: &Committee,
    anchor: &Block,
    blocks: &[Block],
    commit_child: &Block,
    child_qc: &Certificate,
) -> Result<(), String> {
    let mut parent = if blocks[0].parent == anchor.hash() {
        anchor.clone()
    } else {
        store
            .get(&blocks[0].parent)
            .ok_or_else(|| "first imported block parent is not durable".to_string())?
    };

    for block in blocks {
        validate_block_size(block)?;
        block.validate_context(context)?;
        block.validate()?;
        require_leader(committee, block)?;
        if block.parent != parent.hash() {
            return Err(format!(
                "imported block at height {} does not extend its exact parent",
                block.height
            ));
        }
        if block.height != parent.height.saturating_add(1) {
            return Err(format!(
                "imported block at height {} is not sequential after {}",
                block.height, parent.height
            ));
        }
        block.validate_parent_timestamp(parent.timestamp)?;
        validate_block_justify(committee, context, block, &parent)?;
        parent = block.clone();
    }

    commit_child.validate_context(context)?;
    commit_child.validate()?;
    require_leader(committee, commit_child)?;
    if commit_child.parent != parent.hash()
        || commit_child.height != parent.height.saturating_add(1)
    {
        return Err("terminal commit child does not extend the finalized suffix".to_string());
    }
    commit_child.validate_parent_timestamp(parent.timestamp)?;
    validate_block_justify(committee, context, commit_child, &parent)?;
    verify_certificate_for_block(
        committee,
        context,
        child_qc,
        commit_child,
        "terminal child QC",
    )?;
    Ok(())
}

fn validate_block_justify(
    committee: &Committee,
    context: ConsensusContext,
    block: &Block,
    parent: &Block,
) -> Result<(), String> {
    if block.height == 1 {
        if block.justify.is_some() {
            return Err("height-one block must not carry a parent QC".to_string());
        }
        return Ok(());
    }
    let certificate = block
        .justify
        .as_ref()
        .ok_or_else(|| format!("block at height {} is missing its parent QC", block.height))?;
    verify_qc_for_block(committee, context, certificate, parent, "block parent QC")
}

fn resume_index(
    store: &dyn PersistentStore,
    anchor: &Block,
    blocks: &[Block],
) -> Result<usize, String> {
    let Some(first) = blocks.first() else {
        return Err("cannot resume an empty import".to_string());
    };
    if first.parent == anchor.hash() && first.height == anchor.height.saturating_add(1) {
        return Ok(0);
    }
    let Some(index) = blocks
        .iter()
        .position(|block| block.hash() == anchor.hash())
    else {
        return Err(
            "durable head is neither the import anchor nor an imported prefix block".to_string(),
        );
    };
    if blocks[index].height != anchor.height {
        return Err("import prefix height does not match durable head".to_string());
    }
    if index > 0 {
        let persisted = store
            .get_by_height(blocks[index - 1].height)
            .ok_or_else(|| {
                "import prefix before durable head is missing from storage".to_string()
            })?;
        if persisted.hash() != blocks[index - 1].hash() {
            return Err("import prefix before durable head diverges from storage".to_string());
        }
    }
    Ok(index + 1)
}

fn validate_durable_prefix(
    store: &dyn PersistentStore,
    blocks: &[Block],
    prefix_len: usize,
) -> Result<(), String> {
    for block in &blocks[..prefix_len] {
        let stored = store
            .get_by_height(block.height)
            .ok_or_else(|| format!("durable prefix is missing height {}", block.height))?;
        if stored.hash() != block.hash() {
            return Err(format!(
                "durable prefix diverges from imported block at height {}",
                block.height
            ));
        }
        if block.height == 0 {
            continue;
        }
        let root = store
            .load_state_root(&block.hash())
            .map_err(|error| format!("loading prefix state root at {}: {error}", block.height))?
            .ok_or_else(|| format!("prefix state root missing at height {}", block.height))?;
        if root != block.app_hash {
            return Err(format!(
                "prefix state root mismatch at height {}",
                block.height
            ));
        }
        let commitment = store
            .load_commitment(&block.hash())
            .map_err(|error| format!("loading prefix commitment at {}: {error}", block.height))?
            .ok_or_else(|| format!("prefix commitment missing at height {}", block.height))?;
        let root = commitment
            .root()
            .map_err(|error| format!("prefix commitment root at {}: {error}", block.height))?;
        if root != block.commitment_root {
            return Err(format!(
                "prefix commitment mismatch at height {}",
                block.height
            ));
        }
    }
    Ok(())
}

fn replay_on_private_hook(
    app: &CanonicalAppHook,
    anchor: &Block,
    blocks: &[Block],
    commit_child: &Block,
) -> Result<Vec<VerifiedBlock>, String> {
    let private_state = {
        let live = app.shared_state();
        let state = live.app.read().map_err(|_| {
            "canonical application read lock poisoned while creating replay state".to_string()
        })?;
        state.clone_for_verified_component_child()
    };
    let mut scratch =
        CanonicalAppHook::for_verified_anchor(SharedState::new(private_state), anchor.hash());
    let mut verified = Vec::with_capacity(blocks.len());

    for block in blocks {
        let entry = execute_and_verify_private(&mut scratch, block)?;
        scratch
            .commit(block)
            .map_err(|error| format!("private canonical replay commit: {error}"))?;
        verified.push(entry);
    }

    // The child is executed and authenticated, but intentionally not
    // committed in scratch: it must remain speculative in the live store.
    execute_and_verify_private(&mut scratch, commit_child)?;
    Ok(verified)
}

fn execute_and_verify_private(
    scratch: &mut CanonicalAppHook,
    block: &Block,
) -> Result<VerifiedBlock, String> {
    scratch
        .validate_block(block)
        .map_err(|error| format!("private block validation at {}: {error}", block.height))?;
    let app_hash = scratch.execute(block);
    if app_hash != block.app_hash {
        return Err(format!(
            "private replay app hash mismatch at height {}",
            block.height
        ));
    }
    let commitment = scratch
        .preflight_commitment(block)
        .map_err(|error| format!("private replay commitment at {}: {error}", block.height))?
        .ok_or_else(|| format!("private replay produced no commitment at {}", block.height))?;
    let commitment_root = commitment.root().map_err(|error| {
        format!(
            "private replay commitment root at {}: {error}",
            block.height
        )
    })?;
    if commitment_root != block.commitment_root {
        return Err(format!(
            "private replay commitment mismatch at height {}",
            block.height
        ));
    }
    let state_root = scratch
        .preflight_state_root(block)
        .map_err(|error| format!("private replay state root at {}: {error}", block.height))?
        .ok_or_else(|| format!("private replay produced no state root at {}", block.height))?;
    if state_root != block.app_hash {
        return Err(format!(
            "private replay state-root mismatch at height {}",
            block.height
        ));
    }
    Ok(VerifiedBlock { commitment })
}

fn safety_state_for_commit(
    previous: &ConsensusState,
    context: ConsensusContext,
    block: &Block,
    high_qc: Option<Certificate>,
    locked_qc: Option<Certificate>,
) -> Result<ConsensusState, String> {
    let mut next = previous.clone();
    next.epoch = context.epoch;
    next.committee_hash = context.committee_hash;
    next.genesis_hash = context.genesis_hash;
    next.committed_height = block.height;
    next.committed_hash = block.hash();
    next.high_qc = high_qc;
    next.locked_qc = locked_qc;
    next.consecutive_timeouts = 0;
    next.vc_sent_for_view = None;
    if let Some(high_qc) = &next.high_qc {
        next.current_view =
            next.current_view
                .max(high_qc.view.checked_add(1).ok_or_else(|| {
                    "high QC view cannot advance the recovered current view".to_string()
                })?);
    }
    Ok(next)
}

fn verify_import_result(
    app: &CanonicalAppHook,
    store: &dyn PersistentStore,
    context: ConsensusContext,
    child_qc: &Certificate,
    commit_child: &Block,
    final_block: &Block,
) -> Result<(), String> {
    let live = app.shared_state();
    let state = live.app.read().map_err(|_| {
        "canonical application read lock poisoned after verified import".to_string()
    })?;
    if state.committed_height() != final_block.height
        || state.compute_full_state_root() != final_block.app_hash
    {
        return Err("live application head/root does not match imported final block".to_string());
    }
    drop(state);
    if app.exact_committed_hash() != Some(final_block.hash()) {
        return Err("live application hash head does not match imported final block".to_string());
    }

    let head = store
        .get_committed_head()
        .ok_or_else(|| "store lost its committed head after verified import".to_string())?;
    if head.hash() != final_block.hash() {
        return Err("store committed head does not match imported final block".to_string());
    }
    let root = store
        .load_state_root(&final_block.hash())
        .map_err(|error| format!("loading imported final state root: {error}"))?
        .ok_or_else(|| "imported final state root is missing".to_string())?;
    if root != final_block.app_hash {
        return Err("imported final state root does not match final block".to_string());
    }
    let state = store
        .load_consensus_state()
        .map_err(|error| format!("loading imported final consensus state: {error}"))?
        .ok_or_else(|| "imported final consensus state is missing".to_string())?;
    if state.context() != context
        || state.committed_height != final_block.height
        || state.committed_hash != final_block.hash()
        || state.high_qc.as_ref() != Some(child_qc)
        || state.locked_qc.as_ref() != commit_child.justify.as_ref()
    {
        return Err(
            "imported final consensus metadata is not the terminal two-chain proof".to_string(),
        );
    }
    if store.get(&commit_child.hash()).is_none() {
        return Err("terminal speculative child is missing after verified import".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use tempfile::TempDir;

    use crate::api::SharedState;
    use crate::app::candles::Candle;
    use crate::app::staking::{StaticValidatorBootstrap, MIN_SELF_STAKE};
    use crate::app::AppState;
    use crate::consensus::{AppHook, BlockStore};
    use crate::storage::{AppSnapshot, ConsensusState, PersistentStore, RocksDbStore};
    use crate::types::{ConsensusConfig, Hash, Vote};

    struct Fixture {
        config: ConsensusConfig,
        context: ConsensusContext,
        committee: Committee,
        finalized: Vec<Block>,
        child: Block,
        child_qc: Certificate,
    }

    struct FaultStore {
        inner: RocksDbStore,
        fail_after_speculative: AtomicBool,
        fail_after_commit: AtomicBool,
        speculative_writes: Mutex<Vec<Hash>>,
        committed_states: Mutex<Vec<(Hash, ConsensusState)>>,
    }

    impl FaultStore {
        fn new(inner: RocksDbStore) -> Self {
            Self {
                inner,
                fail_after_speculative: AtomicBool::new(false),
                fail_after_commit: AtomicBool::new(false),
                speculative_writes: Mutex::new(Vec::new()),
                committed_states: Mutex::new(Vec::new()),
            }
        }
    }

    impl BlockStore for FaultStore {
        fn save(&self, block: &Block) {
            self.inner.save(block);
        }

        fn save_speculative(&self, block: &Block) -> anyhow::Result<()> {
            self.inner.save_speculative(block)?;
            self.speculative_writes.lock().unwrap().push(block.hash());
            if self.fail_after_speculative.swap(false, Ordering::SeqCst) {
                anyhow::bail!("injected failure after speculative write");
            }
            Ok(())
        }

        fn get(&self, hash: &Hash) -> Option<Block> {
            self.inner.get(hash)
        }

        fn get_by_height(&self, height: u64) -> Option<Block> {
            self.inner.get_by_height(height)
        }

        fn set_committed(&self, hash: &Hash) {
            self.inner.set_committed(hash);
        }

        fn get_committed_head(&self) -> Option<Block> {
            self.inner.get_committed_head()
        }
    }

    impl PersistentStore for FaultStore {
        fn save_consensus_state(&self, state: &ConsensusState) -> anyhow::Result<()> {
            self.inner.save_consensus_state(state)
        }

        fn load_consensus_state(&self) -> anyhow::Result<Option<ConsensusState>> {
            self.inner.load_consensus_state()
        }

        fn save_snapshot(&self, height: u64, snapshot: &AppSnapshot) -> anyhow::Result<()> {
            self.inner.save_snapshot(height, snapshot)
        }

        fn load_latest_snapshot(
            &self,
            before_height: u64,
        ) -> anyhow::Result<Option<(u64, AppSnapshot)>> {
            self.inner.load_latest_snapshot(before_height)
        }

        fn load_latest_snapshot_height(&self, before_height: u64) -> anyhow::Result<Option<u64>> {
            self.inner.load_latest_snapshot_height(before_height)
        }

        fn blocks_from_height(&self, from_height: u64) -> anyhow::Result<Vec<Block>> {
            self.inner.blocks_from_height(from_height)
        }

        fn commit_block(&self, block: &Block, state: &ConsensusState) -> anyhow::Result<()> {
            self.inner.commit_block(block, state)
        }

        fn commit_block_with_commitment_and_state_root(
            &self,
            block: &Block,
            state: &ConsensusState,
            commitment: Option<&CommitmentV2>,
            state_root: Option<&Hash>,
        ) -> anyhow::Result<()> {
            self.inner.commit_block_with_commitment_and_state_root(
                block, state, commitment, state_root,
            )?;
            self.committed_states
                .lock()
                .unwrap()
                .push((block.hash(), state.clone()));
            if self.fail_after_commit.swap(false, Ordering::SeqCst) {
                anyhow::bail!("injected failure after atomic finalized commit");
            }
            Ok(())
        }

        fn load_block_artifacts(&self, hash: &Hash) -> anyhow::Result<Option<Vec<u8>>> {
            self.inner.load_block_artifacts(hash)
        }

        fn load_state_root(&self, hash: &Hash) -> anyhow::Result<Option<Hash>> {
            self.inner.load_state_root(hash)
        }

        fn save_candles_batch(&self, entries: &[(Vec<u8>, Vec<u8>)]) -> anyhow::Result<()> {
            self.inner.save_candles_batch(entries)
        }

        fn load_candles(
            &self,
            symbol: &str,
            interval_str: &str,
            limit: usize,
        ) -> anyhow::Result<Vec<Candle>> {
            self.inner.load_candles(symbol, interval_str, limit)
        }
    }

    fn qc(config: &ConsensusConfig, context: ConsensusContext, block: &Block) -> Certificate {
        let vote = Vote::new_bls(
            context,
            block.view,
            block.hash(),
            block.app_hash,
            config.node_id,
            &config.bls_secret_key().expect("fixture BLS key"),
        );
        Certificate::new_bls(
            context,
            block.view,
            block.hash(),
            vec![vote.clone()],
            vote.signature.clone(),
        )
        .expect("fixture QC")
    }

    fn finalized_block(
        hook: &mut CanonicalAppHook,
        context: ConsensusContext,
        parent: &Block,
        height: u64,
        justify: Option<Certificate>,
    ) -> Block {
        let mut block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: height,
            height,
            parent: parent.hash(),
            payload: Vec::new(),
            proposer: [1u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: height,
            justify,
        };
        block.app_hash = hook.execute(&block);
        let commitment = hook
            .derive_execution_commitment(&block)
            .expect("fixture commitment")
            .expect("fixture commitment artifact");
        block.commitment_root = commitment.root().expect("fixture commitment root");
        hook.seal_execution_commitment(&block)
            .expect("fixture commitment seal");
        block
    }

    fn fixture_with_finalized_len(finalized_len: usize) -> Fixture {
        assert!(finalized_len > 0);
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = [7u8; 32];
        let context = config.context().expect("fixture context");
        let committee = config.committee().expect("fixture committee");
        let source_state = bootstrapped_state(&config, context, &committee);
        let shared = SharedState::new(source_state);
        let mut source = CanonicalAppHook::new(shared);
        let genesis = Block::genesis(context);
        let mut parent = genesis;
        let mut finalized = Vec::with_capacity(finalized_len);
        for height in 1..=finalized_len as u64 {
            let justify = if height == 1 {
                None
            } else {
                Some(qc(&config, context, &parent))
            };
            let block = finalized_block(&mut source, context, &parent, height, justify);
            parent = block.clone();
            finalized.push(block);
        }
        let child = finalized_block(
            &mut source,
            context,
            &parent,
            finalized_len as u64 + 1,
            Some(qc(&config, context, &parent)),
        );
        let child_qc = qc(&config, context, &child);
        Fixture {
            config,
            context,
            committee,
            finalized,
            child,
            child_qc,
        }
    }

    fn fixture() -> Fixture {
        fixture_with_finalized_len(2)
    }

    fn install_genesis(store: &RocksDbStore, context: ConsensusContext) -> Block {
        let genesis = Block::genesis(context);
        let state = ConsensusState {
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
        store
            .commit_block(&genesis, &state)
            .expect("fixture genesis commit");
        genesis
    }

    fn bootstrapped_state(
        config: &ConsensusConfig,
        context: ConsensusContext,
        committee: &Committee,
    ) -> AppState {
        let mut state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        state.set_consensus_context(context);
        let secret = config.bls_secret_key().expect("fixture BLS key");
        state
            .bootstrap_static_committee(
                committee,
                &[StaticValidatorBootstrap {
                    operator: format!("system:genesis:{}", hex::encode(config.node_id)),
                    node_id: config.node_id,
                    voting_power: 1,
                    bls_pubkey: secret.public_key().to_bytes().to_vec(),
                    bls_proof_of_possession: secret
                        .create_proof_of_possession(&context.genesis_hash, &config.node_id)
                        .to_bytes()
                        .to_vec(),
                    self_stake: MIN_SELF_STAKE,
                    commission_bps: 0,
                }],
            )
            .expect("fixture staking bootstrap");
        state
            .bind_authoritative_committee(committee.clone())
            .expect("fixture committee binding");
        state
    }

    fn fresh_app(context: ConsensusContext, committee: &Committee) -> CanonicalAppHook {
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = context.genesis_hash;
        let state = bootstrapped_state(&config, context, committee);
        CanonicalAppHook::new(SharedState::new(state))
    }

    #[test]
    fn imports_two_block_prefix_with_penultimate_recovery_qc() {
        let fixture = fixture();
        let directory = TempDir::new().expect("fixture tempdir");
        let store = RocksDbStore::open(directory.path()).expect("fixture store");
        install_genesis(&store, fixture.context);
        let mut app = fresh_app(fixture.context, &fixture.committee);

        VerifiedBlockImporter::import(
            &mut app,
            &store,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &fixture.child_qc,
        )
        .expect("verified import");

        let final_block = fixture.finalized.last().unwrap();
        assert_eq!(
            store.get_committed_head().unwrap().hash(),
            final_block.hash()
        );
        assert_eq!(app.candidate_count(), 1);
        let state = store
            .load_consensus_state()
            .unwrap()
            .expect("final safety state");
        assert_eq!(state.high_qc, Some(fixture.child_qc.clone()));
        assert_eq!(state.locked_qc, fixture.child.justify.clone());
        assert!(state.current_view >= fixture.child_qc.view + 1);
        assert_eq!(state.consecutive_timeouts, 0);
        assert_eq!(state.vc_sent_for_view, None);
    }

    #[test]
    fn import_is_idempotent_after_a_valid_prefix_and_reopen() {
        let fixture = fixture();
        let directory = TempDir::new().expect("fixture tempdir");
        let store = RocksDbStore::open(directory.path()).expect("fixture store");
        install_genesis(&store, fixture.context);
        let mut app = fresh_app(fixture.context, &fixture.committee);
        VerifiedBlockImporter::import(
            &mut app,
            &store,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &fixture.child_qc,
        )
        .expect("first import");
        VerifiedBlockImporter::import(
            &mut app,
            &store,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &fixture.child_qc,
        )
        .expect("same-process retry");

        drop(app);
        drop(store);
        let reopened = RocksDbStore::open(directory.path()).expect("reopen store");
        let mut recovered = fresh_app(fixture.context, &fixture.committee);
        for block in &fixture.finalized {
            recovered.commit(block).expect("replay durable prefix");
        }
        VerifiedBlockImporter::import(
            &mut recovered,
            &reopened,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &fixture.child_qc,
        )
        .expect("reopen retry");
        assert_eq!(recovered.candidate_count(), 1);
    }

    #[test]
    fn invalid_terminal_app_hash_leaves_live_app_and_store_unchanged() {
        let fixture = fixture();
        let directory = TempDir::new().expect("fixture tempdir");
        let store = RocksDbStore::open(directory.path()).expect("fixture store");
        let genesis = install_genesis(&store, fixture.context);
        let mut app = fresh_app(fixture.context, &fixture.committee);
        let before_root = app
            .shared_state()
            .app
            .read()
            .unwrap()
            .compute_full_state_root();
        let before_head = store.get_committed_head().unwrap().hash();

        let mut bad_last = fixture.finalized[1].clone();
        bad_last.app_hash[0] ^= 1;
        let mut bad_child = fixture.child.clone();
        bad_child.parent = bad_last.hash();
        bad_child.justify = Some(qc(&fixture.config, fixture.context, &bad_last));
        let bad_child_qc = qc(&fixture.config, fixture.context, &bad_child);
        let error = VerifiedBlockImporter::import(
            &mut app,
            &store,
            fixture.context,
            &fixture.committee,
            &[fixture.finalized[0].clone(), bad_last],
            &bad_child,
            &bad_child_qc,
        )
        .expect_err("invalid terminal app hash");
        assert!(error.contains("app hash"));
        assert_eq!(store.get_committed_head().unwrap().hash(), before_head);
        assert_eq!(
            app.shared_state()
                .app
                .read()
                .unwrap()
                .compute_full_state_root(),
            before_root
        );
        assert_eq!(app.candidate_count(), 0);
        assert_eq!(store.get(&genesis.hash()).unwrap().hash(), genesis.hash());
    }

    #[test]
    fn invalid_terminal_commitment_leaves_live_app_and_store_unchanged() {
        let fixture = fixture();
        let directory = TempDir::new().expect("fixture tempdir");
        let store = RocksDbStore::open(directory.path()).expect("fixture store");
        install_genesis(&store, fixture.context);
        let mut app = fresh_app(fixture.context, &fixture.committee);
        let before_root = app
            .shared_state()
            .app
            .read()
            .unwrap()
            .compute_full_state_root();
        let before_head = store.get_committed_head().unwrap().hash();
        let before_state = store.load_consensus_state().unwrap();

        let mut bad_last = fixture.finalized[1].clone();
        bad_last.commitment_root[0] ^= 1;
        let mut bad_child = fixture.child.clone();
        bad_child.parent = bad_last.hash();
        bad_child.justify = Some(qc(&fixture.config, fixture.context, &bad_last));
        let bad_child_qc = qc(&fixture.config, fixture.context, &bad_child);
        let error = VerifiedBlockImporter::import(
            &mut app,
            &store,
            fixture.context,
            &fixture.committee,
            &[fixture.finalized[0].clone(), bad_last],
            &bad_child,
            &bad_child_qc,
        )
        .expect_err("invalid terminal commitment");
        assert!(error.contains("commitment"));
        assert_eq!(store.get_committed_head().unwrap().hash(), before_head);
        assert_eq!(
            serde_json::to_vec(&store.load_consensus_state().unwrap()).unwrap(),
            serde_json::to_vec(&before_state).unwrap()
        );
        assert_eq!(
            app.shared_state()
                .app
                .read()
                .unwrap()
                .compute_full_state_root(),
            before_root
        );
        assert_eq!(app.candidate_count(), 0);
        assert!(store.get(&bad_child.hash()).is_none());
    }

    #[test]
    fn invalid_terminal_qc_leaves_live_app_and_store_unchanged() {
        let fixture = fixture();
        let directory = TempDir::new().expect("fixture tempdir");
        let store = RocksDbStore::open(directory.path()).expect("fixture store");
        install_genesis(&store, fixture.context);
        let mut app = fresh_app(fixture.context, &fixture.committee);
        let before_root = app
            .shared_state()
            .app
            .read()
            .unwrap()
            .compute_full_state_root();
        let mut bad_qc = fixture.child_qc.clone();
        bad_qc.agg_signature[0] ^= 1;
        let error = VerifiedBlockImporter::import(
            &mut app,
            &store,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &bad_qc,
        )
        .expect_err("invalid terminal QC");
        assert!(error.contains("certificate") || error.contains("signature"));
        assert_eq!(store.get_committed_head().unwrap().height, 0);
        assert_eq!(
            app.shared_state()
                .app
                .read()
                .unwrap()
                .compute_full_state_root(),
            before_root
        );
        assert_eq!(app.candidate_count(), 0);
    }

    #[test]
    fn overflowing_high_qc_is_rejected_before_any_store_or_live_mutation() {
        let fixture = fixture();
        let directory = TempDir::new().expect("fixture tempdir");
        let store = RocksDbStore::open(directory.path()).expect("fixture store");
        install_genesis(&store, fixture.context);
        let mut app = fresh_app(fixture.context, &fixture.committee);

        let before_head = store.get_committed_head().unwrap();
        let before_blocks: Vec<Hash> = store
            .blocks_from_height(0)
            .unwrap()
            .into_iter()
            .map(|block| block.hash())
            .collect();
        let before_state = serde_json::to_vec(&store.load_consensus_state().unwrap()).unwrap();
        let before_live_height = app.shared_state().app.read().unwrap().committed_height();
        let before_live_root = app
            .shared_state()
            .app
            .read()
            .unwrap()
            .compute_full_state_root();
        let before_live_hash = app.exact_committed_hash();
        let before_candidates = app.candidate_count();

        let mut source = fresh_app(fixture.context, &fixture.committee);
        source
            .commit(&fixture.finalized[0])
            .expect("fixture prefix commit");
        source
            .commit(&fixture.finalized[1])
            .expect("fixture prefix commit");
        let mut overflowing_child = fixture.child.clone();
        overflowing_child.view = u64::MAX;
        overflowing_child.app_hash = [0u8; 32];
        overflowing_child.commitment_root = [0u8; 32];
        overflowing_child.app_hash = source.execute(&overflowing_child);
        let commitment = source
            .derive_execution_commitment(&overflowing_child)
            .expect("overflowing child commitment")
            .expect("overflowing child commitment artifact");
        overflowing_child.commitment_root = commitment.root().expect("overflowing child root");
        source
            .seal_execution_commitment(&overflowing_child)
            .expect("overflowing child commitment seal");
        let overflowing_child_qc = qc(&fixture.config, fixture.context, &overflowing_child);
        let error = VerifiedBlockImporter::import(
            &mut app,
            &store,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &overflowing_child,
            &overflowing_child_qc,
        )
        .expect_err("high QC view overflow");
        assert!(
            error.contains("cannot advance the recovered current view"),
            "unexpected overflow error: {error}"
        );

        assert_eq!(
            store.get_committed_head().unwrap().hash(),
            before_head.hash()
        );
        assert_eq!(
            store
                .blocks_from_height(0)
                .unwrap()
                .into_iter()
                .map(|block| block.hash())
                .collect::<Vec<_>>(),
            before_blocks
        );
        assert_eq!(
            serde_json::to_vec(&store.load_consensus_state().unwrap()).unwrap(),
            before_state
        );
        for block in &fixture.finalized {
            assert!(store.get(&block.hash()).is_none());
        }
        assert!(store.get(&overflowing_child.hash()).is_none());
        assert_eq!(
            app.shared_state().app.read().unwrap().committed_height(),
            before_live_height
        );
        assert_eq!(
            app.shared_state()
                .app
                .read()
                .unwrap()
                .compute_full_state_root(),
            before_live_root
        );
        assert_eq!(app.exact_committed_hash(), before_live_hash);
        assert_eq!(app.candidate_count(), before_candidates);
    }

    #[test]
    fn three_block_import_persists_each_prefix_safety_qc_exactly() {
        let fixture = fixture_with_finalized_len(3);
        let directory = TempDir::new().expect("fixture tempdir");
        let base = RocksDbStore::open(directory.path()).expect("fixture store");
        install_genesis(&base, fixture.context);
        drop(base);

        let fault =
            FaultStore::new(RocksDbStore::open(directory.path()).expect("fault-injection store"));
        let mut app = fresh_app(fixture.context, &fixture.committee);
        VerifiedBlockImporter::import(
            &mut app,
            &fault,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &fixture.child_qc,
        )
        .expect("three-block verified import");

        let states = fault.committed_states.lock().unwrap();
        assert_eq!(states.len(), 3);
        assert_eq!(states[0].0, fixture.finalized[0].hash());
        assert_eq!(states[0].1.locked_qc, fixture.finalized[1].justify.clone());
        assert_eq!(states[0].1.high_qc, fixture.finalized[2].justify.clone());
        assert_eq!(states[1].0, fixture.finalized[1].hash());
        assert_eq!(states[1].1.locked_qc, fixture.finalized[2].justify.clone());
        assert_eq!(states[1].1.high_qc, fixture.child.justify.clone());
        assert_eq!(states[2].0, fixture.finalized[2].hash());
        assert_eq!(states[2].1.locked_qc, fixture.child.justify.clone());
        assert_eq!(states[2].1.high_qc, Some(fixture.child_qc.clone()));
        assert_eq!(
            fault.speculative_writes.lock().unwrap().as_slice(),
            &[
                fixture.finalized[1].hash(),
                fixture.finalized[2].hash(),
                fixture.child.hash(),
                fixture.child.hash(),
            ]
        );
        assert_eq!(
            fault.get_committed_head().unwrap().hash(),
            fixture.finalized[2].hash()
        );
    }

    #[test]
    fn speculative_write_failure_reopens_and_recovers_a_valid_prefix() {
        let fixture = fixture_with_finalized_len(3);
        let directory = TempDir::new().expect("fixture tempdir");
        let base = RocksDbStore::open(directory.path()).expect("fixture store");
        install_genesis(&base, fixture.context);
        drop(base);

        let fault =
            FaultStore::new(RocksDbStore::open(directory.path()).expect("fault-injection store"));
        fault.fail_after_speculative.store(true, Ordering::SeqCst);
        let mut app = fresh_app(fixture.context, &fixture.committee);
        let error = VerifiedBlockImporter::import(
            &mut app,
            &fault,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &fixture.child_qc,
        )
        .expect_err("injected speculative boundary failure");
        assert!(error.contains("injected failure after speculative write"));
        assert_eq!(fault.get_committed_head().unwrap().height, 0);
        assert!(fault.committed_states.lock().unwrap().is_empty());
        assert_eq!(
            fault.speculative_writes.lock().unwrap().as_slice(),
            &[fixture.finalized[1].hash()]
        );
        assert!(fault.get(&fixture.finalized[1].hash()).is_some());
        drop(app);
        drop(fault);

        let reopened = RocksDbStore::open(directory.path()).expect("reopen store");
        assert_eq!(reopened.get_committed_head().unwrap().height, 0);
        assert!(reopened.get(&fixture.finalized[1].hash()).is_some());
        let mut recovered = fresh_app(fixture.context, &fixture.committee);
        VerifiedBlockImporter::import(
            &mut recovered,
            &reopened,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &fixture.child_qc,
        )
        .expect("retry after speculative boundary failure");
        assert_eq!(
            reopened.get_committed_head().unwrap().hash(),
            fixture.finalized[2].hash()
        );
        assert_eq!(recovered.candidate_count(), 1);
    }

    #[test]
    fn post_commit_failure_reopens_after_a_durable_prefix_and_finishes() {
        let fixture = fixture_with_finalized_len(3);
        let directory = TempDir::new().expect("fixture tempdir");
        let base = RocksDbStore::open(directory.path()).expect("fixture store");
        install_genesis(&base, fixture.context);
        drop(base);

        let fault =
            FaultStore::new(RocksDbStore::open(directory.path()).expect("fault-injection store"));
        fault.fail_after_commit.store(true, Ordering::SeqCst);
        let mut app = fresh_app(fixture.context, &fixture.committee);
        let error = VerifiedBlockImporter::import(
            &mut app,
            &fault,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &fixture.child_qc,
        )
        .expect_err("injected post-commit boundary failure");
        assert!(error.contains("injected failure after atomic finalized commit"));
        assert_eq!(fault.committed_states.lock().unwrap().len(), 1);
        assert_eq!(
            fault.get_committed_head().unwrap().hash(),
            fixture.finalized[0].hash()
        );
        assert_eq!(app.shared_state().app.read().unwrap().committed_height(), 0);
        assert!(fault.get(&fixture.finalized[1].hash()).is_some());
        drop(app);
        drop(fault);

        let reopened = RocksDbStore::open(directory.path()).expect("reopen store");
        assert_eq!(
            reopened.get_committed_head().unwrap().hash(),
            fixture.finalized[0].hash()
        );
        let mut recovered = fresh_app(fixture.context, &fixture.committee);
        recovered
            .commit(&fixture.finalized[0])
            .expect("replay durable prefix into live app");
        VerifiedBlockImporter::import(
            &mut recovered,
            &reopened,
            fixture.context,
            &fixture.committee,
            &fixture.finalized,
            &fixture.child,
            &fixture.child_qc,
        )
        .expect("retry after post-commit boundary failure");
        assert_eq!(
            reopened.get_committed_head().unwrap().hash(),
            fixture.finalized[2].hash()
        );
        assert_eq!(recovered.candidate_count(), 1);
    }
}
