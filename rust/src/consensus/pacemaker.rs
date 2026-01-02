//! Pacemaker: View Advancement
//!
//! The pacemaker ensures liveness by advancing views when:
//! 1. A QC is received (normal case - leader succeeded)
//! 2. Timeout expires (leader failed)
//!
//! This is a "reactive" pacemaker - it waits for events rather than polling.

use std::time::{Duration, Instant};

use crate::types::{Certificate, View};

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
}

impl Pacemaker {
    pub fn new(base_timeout: Duration) -> Self {
        Self {
            current_view: 0,
            base_timeout,
            consecutive_timeouts: 0,
            view_start: Instant::now(),
            max_backoff: 5, // Max 32x base timeout
        }
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
    }
}

impl Default for Pacemaker {
    fn default() -> Self {
        Self::new(Duration::from_secs(3))
    }
}

/// Events that can advance the view
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
            agg_signature: vec![],
        };
        pm.advance_view(&qc);

        assert_eq!(pm.current_timeout(), Duration::from_secs(1));
    }
}
