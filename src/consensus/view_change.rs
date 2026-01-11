//! View Change Protocol
//!
//! Handles coordinated view changes when leader fails.
//! Implements HotStuff-2 Case-2 pacemaker pattern.
//!
//! ## Protocol Flow
//!
//! 1. Validator times out waiting for leader's proposal
//! 2. Validator broadcasts ViewChange with their high_qc
//! 3. New leader collects 2f+1 ViewChanges
//! 4. New leader broadcasts NewView with highest QC among ViewChanges
//! 5. All validators advance to new view

use std::collections::HashMap;

use crate::types::{Certificate, NodeId, View, ViewChange, ViewChangeCertificate};

/// Collects ViewChange messages and forms ViewChangeCertificates
pub struct ViewChangeCollector {
    /// ViewChange messages per target view: view -> (sender -> ViewChange)
    view_changes: HashMap<View, HashMap<NodeId, ViewChange>>,

    /// Quorum size needed (2f+1)
    quorum: usize,
}

impl ViewChangeCollector {
    /// Create a new collector with the given quorum size
    pub fn new(quorum: usize) -> Self {
        Self {
            view_changes: HashMap::new(),
            quorum,
        }
    }

    /// Add a ViewChange message.
    ///
    /// Returns ViewChangeCertificate if quorum reached for the target view.
    pub fn add(&mut self, vc: ViewChange) -> Option<ViewChangeCertificate> {
        let target_view = vc.to_view;
        let sender = vc.sender;

        // Validate the view change
        if let Err(e) = validate_view_change(&vc) {
            tracing::warn!(error = %e, "Invalid ViewChange received");
            return None;
        }

        let view_map = self.view_changes.entry(target_view).or_default();

        // Don't accept duplicate from same sender
        if view_map.contains_key(&sender) {
            return None;
        }

        view_map.insert(sender, vc);

        // Check if we've reached quorum
        if view_map.len() >= self.quorum {
            let vcs: Vec<ViewChange> = view_map.values().cloned().collect();
            Some(ViewChangeCertificate::new(target_view, vcs))
        } else {
            None
        }
    }

    /// Get the highest QC from collected ViewChange messages for a view
    pub fn highest_qc(&self, view: View) -> Option<Certificate> {
        self.view_changes
            .get(&view)?
            .values()
            .filter_map(|vc| vc.high_qc.clone())
            .max_by_key(|qc| qc.view)
    }

    /// Get current count of ViewChanges for a target view
    pub fn count(&self, view: View) -> usize {
        self.view_changes
            .get(&view)
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Prune old view change data below a certain view
    pub fn prune_below(&mut self, view: View) {
        self.view_changes.retain(|v, _| *v >= view);
    }

    /// Clear all collected view changes
    pub fn clear(&mut self) {
        self.view_changes.clear();
    }
}

/// Validates a ViewChange message
pub fn validate_view_change(vc: &ViewChange) -> Result<(), ViewChangeError> {
    // Must be advancing to a higher view
    if vc.to_view <= vc.from_view {
        return Err(ViewChangeError::InvalidViewAdvance {
            from: vc.from_view,
            to: vc.to_view,
        });
    }

    // to_view should be from_view + 1 (normal case)
    // Allow larger jumps for catch-up scenarios
    if vc.to_view > vc.from_view + 100 {
        return Err(ViewChangeError::ViewJumpTooLarge {
            from: vc.from_view,
            to: vc.to_view,
        });
    }

    // TODO: Verify signature when BLS is implemented

    Ok(())
}

/// Errors that can occur during view change validation
#[derive(Debug, Clone, thiserror::Error)]
pub enum ViewChangeError {
    #[error("invalid view advance: from {from} to {to} (must increase)")]
    InvalidViewAdvance { from: View, to: View },

    #[error("view jump too large: from {from} to {to} (max 100)")]
    ViewJumpTooLarge { from: View, to: View },

    #[error("invalid signature")]
    InvalidSignature,

    #[error("sender mismatch")]
    SenderMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_view_change(from: View, to: View, sender: u8) -> ViewChange {
        ViewChange {
            from_view: from,
            to_view: to,
            high_qc: None,
            sender: [sender; 32],
            signature: vec![0u8; 64],
        }
    }

    #[test]
    fn test_collector_reaches_quorum() {
        let mut collector = ViewChangeCollector::new(2);

        // First ViewChange - no quorum yet
        let vc1 = make_view_change(5, 6, 1);
        assert!(collector.add(vc1).is_none());
        assert_eq!(collector.count(6), 1);

        // Second ViewChange - reaches quorum
        let vc2 = make_view_change(5, 6, 2);
        let cert = collector.add(vc2);
        assert!(cert.is_some());

        let cert = cert.unwrap();
        assert_eq!(cert.view, 6);
        assert_eq!(cert.view_changes.len(), 2);
    }

    #[test]
    fn test_collector_ignores_duplicates() {
        let mut collector = ViewChangeCollector::new(2);

        let vc1 = make_view_change(5, 6, 1);
        collector.add(vc1.clone());
        collector.add(vc1); // Duplicate

        assert_eq!(collector.count(6), 1);
    }

    #[test]
    fn test_collector_different_views() {
        let mut collector = ViewChangeCollector::new(2);

        // ViewChanges for different target views
        let vc1 = make_view_change(5, 6, 1);
        let vc2 = make_view_change(6, 7, 2);

        collector.add(vc1);
        collector.add(vc2);

        assert_eq!(collector.count(6), 1);
        assert_eq!(collector.count(7), 1);
    }

    #[test]
    fn test_highest_qc_extraction() {
        let mut collector = ViewChangeCollector::new(3);

        // ViewChange with QC at view 10
        let mut vc1 = make_view_change(15, 16, 1);
        vc1.high_qc = Some(Certificate::new(10, [1u8; 32], vec![]));

        // ViewChange with QC at view 12 (higher)
        let mut vc2 = make_view_change(15, 16, 2);
        vc2.high_qc = Some(Certificate::new(12, [2u8; 32], vec![]));

        // ViewChange with no QC
        let vc3 = make_view_change(15, 16, 3);

        collector.add(vc1);
        collector.add(vc2);
        collector.add(vc3);

        let highest = collector.highest_qc(16);
        assert!(highest.is_some());
        assert_eq!(highest.unwrap().view, 12);
    }

    #[test]
    fn test_validate_view_change() {
        // Valid view change
        let valid = make_view_change(5, 6, 1);
        assert!(validate_view_change(&valid).is_ok());

        // Invalid: to_view <= from_view
        let invalid = make_view_change(6, 5, 1);
        assert!(matches!(
            validate_view_change(&invalid),
            Err(ViewChangeError::InvalidViewAdvance { .. })
        ));

        // Invalid: jump too large
        let big_jump = make_view_change(5, 200, 1);
        assert!(matches!(
            validate_view_change(&big_jump),
            Err(ViewChangeError::ViewJumpTooLarge { .. })
        ));
    }

    #[test]
    fn test_prune_below() {
        let mut collector = ViewChangeCollector::new(2);

        collector.add(make_view_change(5, 6, 1));
        collector.add(make_view_change(10, 11, 2));
        collector.add(make_view_change(15, 16, 3));

        // Prune views below 12
        collector.prune_below(12);

        assert_eq!(collector.count(6), 0);
        assert_eq!(collector.count(11), 0);
        assert_eq!(collector.count(16), 1);
    }
}
