//! Pacemaker: View Advancement
//!
//! The pacemaker ensures liveness by advancing views when:
//! 1. A QC is received (normal case - leader succeeded)
//! 2. Timeout expires (leader failed)
//!
//! This is a "reactive" pacemaker - it waits for events rather than polling.
//!
//! ## View Change Protocol
//!
//! When timeout occurs, the pacemaker coordinates view changes:
//! 1. Create and broadcast ViewChange message
//! 2. Collect ViewChanges from other validators
//! 3. When quorum reached, new leader broadcasts NewView

use std::time::{Duration, Instant};

use std::collections::HashMap;

use crate::crypto::bls::BlsPublicKey;
use crate::types::{
    Certificate, Committee, ConsensusContext, NewView, NodeId, View, ViewChange,
    ViewChangeCertificate,
};

use super::view_change::ViewChangeCollector;

/// Pacemaker manages view advancement
pub struct Pacemaker {
    /// Current view
    current_view: View,

    /// Base timeout duration
    base_timeout: Duration,

    /// Consecutive timeouts (for exponential backoff)
    consecutive_timeouts: u32,

    /// When current view started
    view_start: Instant,

    /// Maximum timeout multiplier (2^max_backoff)
    max_backoff: u32,

    /// View change collector (None if view change protocol disabled)
    vc_collector: Option<ViewChangeCollector>,

    /// View for which we've already sent a ViewChange (prevent double-send)
    vc_sent_for_view: Option<View>,

    /// Static consensus context used by all view-change messages.
    context: Option<ConsensusContext>,
}

impl Pacemaker {
    pub fn new(base_timeout: Duration) -> Self {
        Self {
            current_view: 0,
            base_timeout,
            consecutive_timeouts: 0,
            view_start: Instant::now(),
            max_backoff: 5, // Max 32x base timeout
            vc_collector: None,
            vc_sent_for_view: None,
            context: None,
        }
    }

    /// Enable view change protocol with given quorum size
    pub fn with_view_change(&mut self, quorum: usize) {
        self.vc_collector = Some(ViewChangeCollector::new(quorum));
    }

    /// Enable view change protocol with BLS signature verification.
    ///
    /// ViewChanges must be signed with valid BLS keys to be accepted.
    pub fn with_view_change_verified(
        &mut self,
        quorum: usize,
        validator_pubkeys: HashMap<NodeId, BlsPublicKey>,
    ) {
        self.vc_collector = Some(ViewChangeCollector::with_validators(
            quorum,
            validator_pubkeys,
        ));
    }

    /// Enable view changes with the active committee's strict weighted
    /// quorum and configured BLS keys.
    pub fn with_view_change_committee(&mut self, committee: Committee) -> Result<(), String> {
        let context = self.context.unwrap_or_else(|| committee.initial_context());
        if let Some(existing) = self.context {
            if existing != context {
                return Err("cannot change pacemaker consensus context".to_string());
            }
        }
        self.context = Some(context);
        self.vc_collector = Some(ViewChangeCollector::with_committee_and_context(
            committee, context,
        )?);
        Ok(())
    }

    /// Bind this pacemaker to a static consensus context.
    pub fn set_context(&mut self, context: ConsensusContext) -> Result<(), String> {
        if let Some(existing) = self.context {
            if existing != context {
                return Err("cannot change pacemaker consensus context".to_string());
            }
        }
        self.context = Some(context);
        Ok(())
    }

    /// Return the static consensus context, if configured.
    pub fn context(&self) -> Option<ConsensusContext> {
        self.context
    }

    /// Get current view
    pub fn current_view(&self) -> View {
        self.current_view
    }

    /// Advance to next view (called when QC received)
    pub fn advance_view(&mut self, qc: &Certificate) {
        if let Some(expected) = self.context {
            if qc.context() != expected {
                tracing::warn!("Ignoring QC from a mismatched consensus context");
                return;
            }
        } else {
            self.context = Some(qc.context());
        }
        let new_view = qc.view + 1;
        if new_view > self.current_view {
            self.current_view = new_view;
            self.consecutive_timeouts = 0; // Reset backoff on success
            self.view_start = Instant::now();
        }
    }

    /// Force advance to a specific view (called on timeout or sync)
    pub fn advance_to(&mut self, view: View) {
        if view > self.current_view {
            self.current_view = view;
            self.view_start = Instant::now();
        }
    }

    /// Set view directly (for crash recovery).
    ///
    /// Unlike `advance_to`, this can set any view including lower ones.
    /// Use only during initialization from persisted state.
    pub fn set_view(&mut self, view: View) {
        self.current_view = view;
        self.view_start = Instant::now();
    }

    /// Set timeout state from persisted values (for crash recovery).
    ///
    /// Restores exponential backoff state and ViewChange tracking.
    pub fn set_timeout_state(&mut self, consecutive_timeouts: u32, vc_sent_for_view: Option<View>) {
        self.consecutive_timeouts = consecutive_timeouts;
        self.vc_sent_for_view = vc_sent_for_view;
    }

    /// Get current timeout state for persistence.
    ///
    /// Returns (consecutive_timeouts, vc_sent_for_view) for crash recovery.
    pub fn timeout_state(&self) -> (u32, Option<View>) {
        (self.consecutive_timeouts, self.vc_sent_for_view)
    }

    /// Record a timeout (for exponential backoff)
    pub fn record_timeout(&mut self) {
        self.consecutive_timeouts = self.consecutive_timeouts.saturating_add(1);
        self.current_view += 1;
        self.view_start = Instant::now();
    }

    /// Get current timeout duration (with exponential backoff)
    pub fn current_timeout(&self) -> Duration {
        let multiplier = 1u32 << self.consecutive_timeouts.min(self.max_backoff);
        self.base_timeout * multiplier
    }

    /// Check if current view has timed out
    pub fn is_timed_out(&self) -> bool {
        self.view_start.elapsed() >= self.current_timeout()
    }

    /// Time remaining in current view
    pub fn time_remaining(&self) -> Duration {
        let elapsed = self.view_start.elapsed();
        let timeout = self.current_timeout();
        timeout.saturating_sub(elapsed)
    }

    /// Reset pacemaker to initial state
    pub fn reset(&mut self) {
        self.current_view = 0;
        self.consecutive_timeouts = 0;
        self.view_start = Instant::now();
        self.vc_sent_for_view = None;
        if let Some(ref mut collector) = self.vc_collector {
            collector.clear();
        }
    }

    // =========================================================================
    // View Change Protocol Methods
    // =========================================================================

    /// Create a ViewChange message for current timeout.
    ///
    /// Returns None if:
    /// - ViewChange already sent for this view
    /// - View change protocol not enabled
    ///
    /// Note: This creates unsigned ViewChanges (placeholder signature).
    /// Use `create_signed_view_change()` for BLS-signed ViewChanges.
    pub fn create_view_change(
        &mut self,
        node_id: NodeId,
        high_qc: Option<Certificate>,
    ) -> Option<ViewChange> {
        let current = self.current_view;

        // Only send once per view
        if self.vc_sent_for_view == Some(current) {
            return None;
        }

        self.vc_sent_for_view = Some(current);

        let context = self
            .context
            .unwrap_or_else(|| ConsensusContext::new(0, [0u8; 32]));

        Some(ViewChange {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            from_view: current,
            to_view: current + 1,
            high_qc,
            sender: node_id,
            signature: vec![0u8; 64], // Placeholder - use create_signed_view_change for BLS
        })
    }

    /// Create a BLS-signed ViewChange message for current timeout.
    ///
    /// Returns None if ViewChange already sent for this view.
    pub fn create_signed_view_change(
        &mut self,
        node_id: NodeId,
        high_qc: Option<Certificate>,
        bls_sk: &crate::crypto::bls::BlsSecretKey,
    ) -> Option<ViewChange> {
        let current = self.current_view;

        // Only send once per view
        if self.vc_sent_for_view == Some(current) {
            return None;
        }

        self.vc_sent_for_view = Some(current);

        Some(super::view_change::create_signed_view_change_with_context(
            self.context
                .unwrap_or_else(|| ConsensusContext::new(0, [0u8; 32])),
            current,
            current + 1,
            high_qc,
            node_id,
            bls_sk,
        ))
    }

    /// Process received ViewChange message.
    ///
    /// Returns ViewChangeCertificate if quorum reached for the target view.
    /// Rejects ViewChanges that are too far ahead of current view to prevent
    /// memory exhaustion attacks.
    pub fn on_view_change(&mut self, vc: ViewChange) -> Option<ViewChangeCertificate> {
        use super::view_change::{validate_view_change_bounds, MAX_FUTURE_VIEWS};

        // Check if ViewChange is within acceptable range
        if let Err(e) = validate_view_change_bounds(&vc, self.current_view) {
            tracing::warn!(
                to_view = vc.to_view,
                current_view = self.current_view,
                max_future = MAX_FUTURE_VIEWS,
                error = %e,
                "Rejecting ViewChange: too far ahead"
            );
            return None;
        }

        self.vc_collector.as_mut()?.add(vc)
    }

    /// Process received NewView message from new leader.
    ///
    /// Advances to the new view if it's higher than current.
    pub fn on_new_view(&mut self, nv: &NewView) {
        let expected = self.context.unwrap_or_else(|| nv.context());
        if nv.context() != expected
            || nv.view_change_cert.context() != expected
            || nv
                .high_qc
                .as_ref()
                .is_some_and(|qc| qc.context() != expected)
        {
            tracing::warn!("Ignoring NewView from a mismatched consensus context");
            return;
        }
        if self.context.is_none() {
            self.context = Some(expected);
        }
        if nv.view > self.current_view {
            self.current_view = nv.view;
            self.consecutive_timeouts = 0;
            self.view_start = Instant::now();
            self.vc_sent_for_view = None;

            // Prune old view change data
            if let Some(ref mut collector) = self.vc_collector {
                collector.prune_below(nv.view);
            }
        }
    }

    /// Get the highest QC from collected ViewChanges for a view
    pub fn highest_qc_from_view_changes(&self, view: View) -> Option<Certificate> {
        self.vc_collector.as_ref()?.highest_qc(view)
    }

    /// Check if view change protocol is enabled
    pub fn has_view_change(&self) -> bool {
        self.vc_collector.is_some()
    }
}

impl Default for Pacemaker {
    fn default() -> Self {
        Self::new(Duration::from_secs(3))
    }
}

/// Events that can advance the view (for future use in logging/metrics)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ViewAdvanceReason {
    /// Received a valid QC
    QCReceived(Certificate),
    /// Timeout expired
    Timeout,
    /// Syncing with higher view from network
    Sync(View),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_view_starts_at_zero() {
        let pm = Pacemaker::new(Duration::from_secs(1));
        assert_eq!(pm.current_view(), 0);
    }

    #[test]
    fn test_advance_on_qc() {
        let mut pm = Pacemaker::new(Duration::from_secs(1));

        let qc = Certificate {
            epoch: 0,
            committee_hash: [0u8; 32],
            genesis_hash: [0u8; 32],
            view: 0,
            block_hash: [0u8; 32],
            app_hash: Some([0u8; 32]),
            votes: vec![],
            voters: vec![],
            bls_pubkeys: vec![],
            agg_signature: vec![],
        };

        pm.advance_view(&qc);
        assert_eq!(pm.current_view(), 1);
    }

    #[test]
    fn test_exponential_backoff() {
        let mut pm = Pacemaker::new(Duration::from_secs(1));

        assert_eq!(pm.current_timeout(), Duration::from_secs(1));

        pm.record_timeout();
        assert_eq!(pm.current_timeout(), Duration::from_secs(2));

        pm.record_timeout();
        assert_eq!(pm.current_timeout(), Duration::from_secs(4));

        pm.record_timeout();
        assert_eq!(pm.current_timeout(), Duration::from_secs(8));
    }

    #[test]
    fn test_backoff_resets_on_success() {
        let mut pm = Pacemaker::new(Duration::from_secs(1));

        // Timeout twice
        pm.record_timeout();
        pm.record_timeout();
        assert_eq!(pm.current_timeout(), Duration::from_secs(4));

        // Success resets backoff
        let qc = Certificate {
            epoch: 0,
            committee_hash: [0u8; 32],
            genesis_hash: [0u8; 32],
            view: pm.current_view(),
            block_hash: [0u8; 32],
            app_hash: Some([0u8; 32]),
            votes: vec![],
            voters: vec![],
            bls_pubkeys: vec![],
            agg_signature: vec![],
        };
        pm.advance_view(&qc);

        assert_eq!(pm.current_timeout(), Duration::from_secs(1));
    }
}
