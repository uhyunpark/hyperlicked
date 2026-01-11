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

use crate::types::{Certificate, NewView, NodeId, View, ViewChange, ViewChangeCertificate};

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
        }
    }

    /// Enable view change protocol with given quorum size
    pub fn with_view_change(&mut self, quorum: usize) {
        self.vc_collector = Some(ViewChangeCollector::new(quorum));
    }

    /// Get current view
    pub fn current_view(&self) -> View {
        self.current_view
    }

    /// Advance to next view (called when QC received)
    pub fn advance_view(&mut self, qc: &Certificate) {
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

        Some(ViewChange {
            from_view: current,
            to_view: current + 1,
            high_qc,
            sender: node_id,
            signature: vec![0u8; 64], // Placeholder until BLS
        })
    }

    /// Process received ViewChange message.
    ///
    /// Returns ViewChangeCertificate if quorum reached for the target view.
    pub fn on_view_change(&mut self, vc: ViewChange) -> Option<ViewChangeCertificate> {
        self.vc_collector.as_mut()?.add(vc)
    }

    /// Process received NewView message from new leader.
    ///
    /// Advances to the new view if it's higher than current.
    pub fn on_new_view(&mut self, nv: &NewView) {
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
            view: 0,
            block_hash: [0u8; 32],
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
            view: pm.current_view(),
            block_hash: [0u8; 32],
            votes: vec![],
            voters: vec![],
            bls_pubkeys: vec![],
            agg_signature: vec![],
        };
        pm.advance_view(&qc);

        assert_eq!(pm.current_timeout(), Duration::from_secs(1));
    }
}
