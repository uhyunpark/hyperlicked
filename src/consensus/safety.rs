//! Safety Rules for Voting
//!
//! Defines when a validator can safely vote for a block.
//! These rules ensure consensus safety (no conflicting commits).
//!
//! ## Rules
//!
//! A validator MUST NOT vote if:
//! 1. Already voted in this view (one vote per view)
//! 2. Block doesn't extend from highest known QC
//! 3. Block is invalid (bad structure, signature, etc.)
//! 4. AppHash doesn't match local execution
//!
//! ## Persistence Requirements (CRITICAL)
//!
//! The `voted_views` set MUST be persisted to prevent double-voting after crash.
//! Use `voted_views()` to export for persistence, and `with_state()` to recover.
//!
//! ```ignore
//! // Before shutdown or after each vote
//! let state = ConsensusState {
//!     voted_views: safety.voted_views(),
//!     high_qc: safety.high_qc().cloned(),
//!     // ...
//! };
//! store.save_consensus_state(&state)?;
//!
//! // On recovery
//! let state = store.load_consensus_state()?;
//! let safety = Safety::with_state(
//!     state.high_qc,
//!     state.locked_qc,
//!     &state.voted_views,
//! );
//! ```

use std::collections::HashSet;

use crate::types::{Block, Certificate, Hash, View};

/// Safety module: tracks votes and enforces voting rules
pub struct Safety {
    /// Views we've already voted in (prevents double voting)
    voted_views: HashSet<View>,

    /// Highest QC we've seen (blocks must extend from this)
    high_qc: Option<Certificate>,

    /// Locked QC (for liveness, see HotStuff-2 paper)
    /// Block can only be voted if it extends locked_qc or has higher view
    locked_qc: Option<Certificate>,
}

impl Safety {
    pub fn new() -> Self {
        Self {
            voted_views: HashSet::new(),
            high_qc: None,
            locked_qc: None,
        }
    }

    /// Create Safety module with recovered state (for persistence recovery)
    pub fn with_state(
        high_qc: Option<Certificate>,
        locked_qc: Option<Certificate>,
        voted_views: &[View],
    ) -> Self {
        Self {
            voted_views: voted_views.iter().copied().collect(),
            high_qc,
            locked_qc,
        }
    }

    /// Check if it's safe to vote for this block
    pub fn safe_to_vote(&self, block: &Block, local_app_hash: Hash) -> Result<(), SafetyError> {
        // Rule 1: One vote per view
        if self.voted_views.contains(&block.view) {
            return Err(SafetyError::AlreadyVoted(block.view));
        }

        // Rule 2: Must extend from high_qc (or be first block)
        if let Some(ref high_qc) = self.high_qc {
            if block.parent != high_qc.block_hash {
                return Err(SafetyError::DoesNotExtendHighQC {
                    block_parent: block.parent,
                    high_qc_hash: high_qc.block_hash,
                });
            }
        }

        // Rule 3: Check locking rule (HotStuff-2 specific)
        // A block is safe if either:
        // - It extends the locked block, OR
        // - Its view > locked_qc.view (allows view change progress)
        if let Some(ref locked_qc) = self.locked_qc {
            let extends_locked = block.parent == locked_qc.block_hash;
            let higher_view = block.view > locked_qc.view;

            if !extends_locked && !higher_view {
                return Err(SafetyError::ViolatesLocking {
                    block_view: block.view,
                    locked_view: locked_qc.view,
                });
            }
        }

        // Rule 4: AppHash must match
        if block.app_hash != local_app_hash {
            return Err(SafetyError::AppHashMismatch {
                expected: local_app_hash,
                got: block.app_hash,
            });
        }

        Ok(())
    }

    /// Record that we voted in this view
    pub fn record_vote(&mut self, view: View) {
        self.voted_views.insert(view);
    }

    /// Update high_qc if the new one is higher
    pub fn update_high_qc(&mut self, qc: Certificate) {
        let dominated = self
            .high_qc
            .as_ref()
            .map(|h| qc.view > h.view)
            .unwrap_or(true);

        if dominated {
            self.high_qc = Some(qc);
        }
    }

    /// Update locked_qc (called when we see QC on a QC)
    pub fn update_locked_qc(&mut self, qc: Certificate) {
        let dominated = self
            .locked_qc
            .as_ref()
            .map(|l| qc.view > l.view)
            .unwrap_or(true);

        if dominated {
            self.locked_qc = Some(qc);
        }
    }

    /// Get the current high_qc
    pub fn high_qc(&self) -> Option<&Certificate> {
        self.high_qc.as_ref()
    }

    /// Get the current locked_qc
    pub fn locked_qc(&self) -> Option<&Certificate> {
        self.locked_qc.as_ref()
    }

    /// Get view of high_qc (0 if none)
    pub fn high_qc_view(&self) -> View {
        self.high_qc.as_ref().map(|qc| qc.view).unwrap_or(0)
    }

    /// Clear votes for views below the given view (garbage collection)
    pub fn prune_votes_below(&mut self, view: View) {
        self.voted_views.retain(|&v| v >= view);
    }

    /// Export voted_views for persistence
    ///
    /// Returns a Vec of views we've voted in. This MUST be persisted
    /// to prevent double-voting after crash recovery.
    pub fn voted_views(&self) -> Vec<View> {
        self.voted_views.iter().copied().collect()
    }
}

impl Default for Safety {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors from safety checks
#[derive(Debug, Clone, thiserror::Error)]
pub enum SafetyError {
    #[error("already voted in view {0}")]
    AlreadyVoted(View),

    #[error("block parent {block_parent:?} doesn't extend high_qc {high_qc_hash:?}")]
    DoesNotExtendHighQC {
        block_parent: Hash,
        high_qc_hash: Hash,
    },

    #[error("block view {block_view} violates locking at view {locked_view}")]
    ViolatesLocking { block_view: View, locked_view: View },

    #[error("app_hash mismatch: expected {expected:?}, got {got:?}")]
    AppHashMismatch { expected: Hash, got: Hash },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_vote_allowed() {
        let safety = Safety::new();
        let block = Block::genesis();
        let local_hash = [0u8; 32];

        assert!(safety.safe_to_vote(&block, local_hash).is_ok());
    }

    #[test]
    fn test_double_vote_rejected() {
        let mut safety = Safety::new();
        let block = Block::genesis();
        let local_hash = [0u8; 32];

        // First vote OK
        assert!(safety.safe_to_vote(&block, local_hash).is_ok());
        safety.record_vote(block.view);

        // Second vote in same view rejected
        assert!(matches!(
            safety.safe_to_vote(&block, local_hash),
            Err(SafetyError::AlreadyVoted(_))
        ));
    }

    #[test]
    fn test_app_hash_mismatch() {
        let safety = Safety::new();
        let block = Block::genesis();
        let wrong_hash = [1u8; 32]; // Different from block.app_hash

        assert!(matches!(
            safety.safe_to_vote(&block, wrong_hash),
            Err(SafetyError::AppHashMismatch { .. })
        ));
    }

    #[test]
    fn test_voted_views_persistence() {
        // Simulate voting in several views
        let mut safety = Safety::new();
        safety.record_vote(5);
        safety.record_vote(7);
        safety.record_vote(10);

        // Export for persistence
        let voted_views = safety.voted_views();
        assert_eq!(voted_views.len(), 3);

        // Simulate recovery from persisted state
        let recovered_safety = Safety::with_state(None, None, &voted_views);

        // Verify we can't double-vote after recovery
        let block5 = Block {
            view: 5,
            height: 1,
            parent: [0u8; 32],
            payload: vec![],
            proposer: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 0,
            justify: None,
        };

        assert!(matches!(
            recovered_safety.safe_to_vote(&block5, [0u8; 32]),
            Err(SafetyError::AlreadyVoted(5))
        ));
    }

    #[test]
    fn test_voted_views_prune_and_persist() {
        let mut safety = Safety::new();
        safety.record_vote(1);
        safety.record_vote(5);
        safety.record_vote(10);
        safety.record_vote(15);

        // Prune old views
        safety.prune_votes_below(8);

        // Only views >= 8 should remain
        let voted_views = safety.voted_views();
        assert_eq!(voted_views.len(), 2);
        assert!(voted_views.contains(&10));
        assert!(voted_views.contains(&15));
        assert!(!voted_views.contains(&1));
        assert!(!voted_views.contains(&5));
    }
}
