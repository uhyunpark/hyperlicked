//! 3-Bucket Mempool with Two-Phase Commit
//!
//! Orders transactions by priority:
//! 1. Non-order txs (deposits, withdrawals) - bucket 0
//! 2. Cancels - bucket 1
//! 3. Orders (GTC, IOC, ALO) - bucket 2
//!
//! Within each bucket, FIFO order is maintained.
//!
//! Two-phase commit protocol:
//! 1. `peek_block()` - marks txs as "in proposal", returns refs
//! 2. `commit_proposal()` - removes committed txs by hash
//! 3. `rollback_proposal()` - clears pending set on view change

use std::collections::{HashMap, HashSet, VecDeque};

use super::Transaction;
use crate::types::{Hash, View};

/// Transaction with metadata
#[derive(Debug, Clone)]
pub struct PendingTx {
    pub tx: Transaction,
    pub hash: Hash,
    pub timestamp: u64,
}

/// 3-bucket mempool for transaction ordering
pub struct Mempool {
    /// Bucket 0: Non-order transactions (deposits, withdrawals)
    bucket0: VecDeque<PendingTx>,
    /// Bucket 1: Cancel orders
    bucket1: VecDeque<PendingTx>,
    /// Bucket 2: Place orders (GTC, IOC, ALO)
    bucket2: VecDeque<PendingTx>,
    /// Maximum transactions per bucket
    max_per_bucket: usize,
    /// Hashes of transactions currently in an uncommitted proposal
    pending_proposal: HashSet<Hash>,
    /// View number of the current proposal (for validation)
    proposal_view: View,
    /// Per-address pending transaction count (anti-spam)
    per_address_count: HashMap<String, usize>,
    /// Maximum pending transactions per address
    max_per_address: usize,
    /// Maximum transaction age in milliseconds before eviction
    max_age_ms: u64,
}

impl Mempool {
    pub fn new(max_per_bucket: usize) -> Self {
        Self::with_config(max_per_bucket, 100, 3_600_000)
    }

    /// Create mempool with full configuration
    pub fn with_config(max_per_bucket: usize, max_per_address: usize, max_age_ms: u64) -> Self {
        Self {
            bucket0: VecDeque::new(),
            bucket1: VecDeque::new(),
            bucket2: VecDeque::new(),
            max_per_bucket,
            pending_proposal: HashSet::new(),
            proposal_view: 0,
            per_address_count: HashMap::new(),
            max_per_address,
            max_age_ms,
        }
    }

    /// Create mempool from global config
    pub fn from_config() -> Self {
        let config = crate::config::Config::global();
        Self::with_config(
            config.mempool_max_per_bucket,
            config.mempool_max_per_address,
            config.mempool_max_age_ms,
        )
    }

    /// Add a transaction to the appropriate bucket
    pub fn add(&mut self, tx: Transaction, timestamp: u64) -> Result<Hash, MempoolError> {
        let hash = crate::types::hash(&tx.to_bytes());
        let bucket = tx.bucket();
        let trader = tx.trader_address().to_string();

        // Check per-address limit
        let count = self.per_address_count.get(&trader).copied().unwrap_or(0);
        if count >= self.max_per_address {
            return Err(MempoolError::AddressLimitReached);
        }

        let queue = match bucket {
            0 => &mut self.bucket0,
            1 => &mut self.bucket1,
            _ => &mut self.bucket2,
        };

        if queue.len() >= self.max_per_bucket {
            return Err(MempoolError::BucketFull);
        }

        queue.push_back(PendingTx {
            tx,
            hash,
            timestamp,
        });

        // Increment per-address count
        *self.per_address_count.entry(trader).or_insert(0) += 1;

        Ok(hash)
    }

    /// Get transactions for a block (ordered by bucket)
    pub fn prepare_block(&mut self, max_txs: usize) -> Vec<Transaction> {
        let mut result = Vec::with_capacity(max_txs);
        let mut to_decrement: Vec<String> = Vec::new();

        // Drain from bucket 0 first (highest priority)
        while result.len() < max_txs {
            if let Some(pending) = self.bucket0.pop_front() {
                to_decrement.push(pending.tx.trader_address().to_string());
                result.push(pending.tx);
            } else {
                break;
            }
        }

        // Then bucket 1
        while result.len() < max_txs {
            if let Some(pending) = self.bucket1.pop_front() {
                to_decrement.push(pending.tx.trader_address().to_string());
                result.push(pending.tx);
            } else {
                break;
            }
        }

        // Finally bucket 2
        while result.len() < max_txs {
            if let Some(pending) = self.bucket2.pop_front() {
                to_decrement.push(pending.tx.trader_address().to_string());
                result.push(pending.tx);
            } else {
                break;
            }
        }

        // Decrement per-address counts
        for addr in to_decrement {
            if let Some(count) = self.per_address_count.get_mut(&addr) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_address_count.remove(&addr);
                }
            }
        }

        result
    }

    /// Peek transactions for a block without removing them (two-phase: step 1)
    ///
    /// Marks transactions as "in proposal" for the given view.
    /// Call `commit_proposal()` after block commits, or `rollback_proposal()` on view change.
    pub fn peek_block(&mut self, max_txs: usize, view: View) -> Vec<(Transaction, Hash)> {
        // Clear any stale proposal from a previous view
        if self.proposal_view != view {
            self.pending_proposal.clear();
            self.proposal_view = view;
        }

        let mut result = Vec::with_capacity(max_txs);

        // Read from bucket 0 first (highest priority)
        for pending in &self.bucket0 {
            if result.len() >= max_txs {
                break;
            }
            if !self.pending_proposal.contains(&pending.hash) {
                result.push((pending.tx.clone(), pending.hash));
                self.pending_proposal.insert(pending.hash);
            }
        }

        // Then bucket 1
        for pending in &self.bucket1 {
            if result.len() >= max_txs {
                break;
            }
            if !self.pending_proposal.contains(&pending.hash) {
                result.push((pending.tx.clone(), pending.hash));
                self.pending_proposal.insert(pending.hash);
            }
        }

        // Finally bucket 2
        for pending in &self.bucket2 {
            if result.len() >= max_txs {
                break;
            }
            if !self.pending_proposal.contains(&pending.hash) {
                result.push((pending.tx.clone(), pending.hash));
                self.pending_proposal.insert(pending.hash);
            }
        }

        result
    }

    /// Legacy peek for backward compatibility (single-node mode)
    pub fn peek_block_txs(&self, max_txs: usize) -> Vec<Transaction> {
        let mut result = Vec::with_capacity(max_txs);

        for pending in &self.bucket0 {
            if result.len() >= max_txs {
                return result;
            }
            result.push(pending.tx.clone());
        }

        for pending in &self.bucket1 {
            if result.len() >= max_txs {
                return result;
            }
            result.push(pending.tx.clone());
        }

        for pending in &self.bucket2 {
            if result.len() >= max_txs {
                return result;
            }
            result.push(pending.tx.clone());
        }

        result
    }

    /// Commit a proposal by removing transactions by hash (two-phase: step 2)
    ///
    /// Called after a block is committed to finalize removal.
    ///
    /// **View Safety**: The `view` parameter must match the view that was used in `peek_block`.
    /// If the view doesn't match (e.g., a view change occurred), the commit is rejected
    /// to prevent removing transactions that belong to a different proposal.
    ///
    /// Returns `true` if commit succeeded, `false` if view mismatch (stale commit).
    pub fn commit_proposal(&mut self, tx_hashes: &[Hash], view: View) -> bool {
        // View safety check: reject commits from stale views
        if view != self.proposal_view {
            tracing::warn!(
                expected_view = self.proposal_view,
                commit_view = view,
                "Rejecting stale commit_proposal (view mismatch)"
            );
            return false;
        }

        self.remove_by_hashes(tx_hashes);

        // Clear pending set
        self.pending_proposal.clear();
        true
    }

    /// Commit a proposal without view checking (legacy, single-node mode)
    ///
    /// Use `commit_proposal` with view parameter for multi-node safety.
    pub fn commit_proposal_unchecked(&mut self, tx_hashes: &[Hash]) {
        self.remove_by_hashes(tx_hashes);

        // Clear pending set
        self.pending_proposal.clear();
    }

    /// Helper to remove transactions by hash and update per-address counts
    fn remove_by_hashes(&mut self, tx_hashes: &[Hash]) {
        let hash_set: HashSet<_> = tx_hashes.iter().collect();

        // Collect addresses to decrement (to avoid borrow issues)
        let mut to_decrement: Vec<String> = Vec::new();

        for bucket in [&mut self.bucket0, &mut self.bucket1, &mut self.bucket2] {
            bucket.retain(|p| {
                if hash_set.contains(&p.hash) {
                    to_decrement.push(p.tx.trader_address().to_string());
                    false
                } else {
                    true
                }
            });
        }

        // Decrement counts
        for addr in to_decrement {
            if let Some(count) = self.per_address_count.get_mut(&addr) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_address_count.remove(&addr);
                }
            }
        }
    }

    /// Rollback a proposal on view change (two-phase: abort)
    ///
    /// Transactions stay in mempool for the next leader.
    /// The `view` parameter is used for logging/debugging.
    pub fn rollback_proposal(&mut self, view: View) {
        if !self.pending_proposal.is_empty() {
            tracing::debug!(
                view,
                proposal_view = self.proposal_view,
                pending_count = self.pending_proposal.len(),
                "Rolling back proposal (view change)"
            );
        }
        self.pending_proposal.clear();
    }

    /// Get the current proposal view
    pub fn proposal_view(&self) -> View {
        self.proposal_view
    }

    /// Drain transactions that were previously peeked (legacy, for single-node)
    pub fn drain_block(&mut self, count: usize) {
        let mut remaining = count;
        let mut to_decrement: Vec<String> = Vec::new();

        // Drain from bucket 0 first
        while remaining > 0 {
            if let Some(pending) = self.bucket0.pop_front() {
                to_decrement.push(pending.tx.trader_address().to_string());
                remaining -= 1;
            } else {
                break;
            }
        }

        // Then bucket 1
        while remaining > 0 {
            if let Some(pending) = self.bucket1.pop_front() {
                to_decrement.push(pending.tx.trader_address().to_string());
                remaining -= 1;
            } else {
                break;
            }
        }

        // Finally bucket 2
        while remaining > 0 {
            if let Some(pending) = self.bucket2.pop_front() {
                to_decrement.push(pending.tx.trader_address().to_string());
                remaining -= 1;
            } else {
                break;
            }
        }

        // Decrement per-address counts
        for addr in to_decrement {
            if let Some(count) = self.per_address_count.get_mut(&addr) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_address_count.remove(&addr);
                }
            }
        }
    }

    /// Check how many transactions are pending
    pub fn len(&self) -> usize {
        self.bucket0.len() + self.bucket1.len() + self.bucket2.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get pending count per bucket
    pub fn bucket_counts(&self) -> (usize, usize, usize) {
        (self.bucket0.len(), self.bucket1.len(), self.bucket2.len())
    }

    /// Remove a transaction by hash (for cancellation)
    pub fn remove(&mut self, hash: &Hash) -> bool {
        // Check each bucket
        for bucket in [&mut self.bucket0, &mut self.bucket1, &mut self.bucket2] {
            if let Some(pos) = bucket.iter().position(|p| &p.hash == hash) {
                if let Some(removed) = bucket.remove(pos) {
                    let addr = removed.tx.trader_address().to_string();
                    if let Some(count) = self.per_address_count.get_mut(&addr) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            self.per_address_count.remove(&addr);
                        }
                    }
                }
                return true;
            }
        }
        false
    }

    /// Prune transactions older than max_age_ms
    ///
    /// Should be called at the start of each block to prevent stale tx accumulation.
    /// Returns the number of transactions pruned.
    pub fn prune_stale(&mut self, current_time: u64) -> usize {
        if self.max_age_ms == 0 {
            return 0; // Age eviction disabled
        }

        let cutoff = current_time.saturating_sub(self.max_age_ms);
        let mut pruned = 0;
        let mut to_decrement: Vec<String> = Vec::new();

        for bucket in [&mut self.bucket0, &mut self.bucket1, &mut self.bucket2] {
            let before = bucket.len();
            bucket.retain(|p| {
                if p.timestamp < cutoff {
                    to_decrement.push(p.tx.trader_address().to_string());
                    false
                } else {
                    true
                }
            });
            pruned += before - bucket.len();
        }

        // Decrement counts
        for addr in to_decrement {
            if let Some(count) = self.per_address_count.get_mut(&addr) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_address_count.remove(&addr);
                }
            }
        }

        if pruned > 0 {
            tracing::debug!(pruned, "Pruned stale transactions from mempool");
        }

        pruned
    }

    /// Clear all pending transactions
    pub fn clear(&mut self) {
        self.bucket0.clear();
        self.bucket1.clear();
        self.bucket2.clear();
        self.per_address_count.clear();
    }
}

impl Default for Mempool {
    fn default() -> Self {
        Self::new(10_000)
    }
}

/// Mempool errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum MempoolError {
    #[error("bucket full")]
    BucketFull,
    #[error("transaction already exists")]
    Duplicate,
    #[error("address has too many pending transactions")]
    AddressLimitReached,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{OrderType, Side};

    #[test]
    fn test_bucket_ordering() {
        let mut mempool = Mempool::new(100);

        // Add transactions out of order
        mempool.add(Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 50000,
            size: 100,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }, 1).unwrap();

        mempool.add(Transaction::CancelOrder {
            trader: "bob".into(),
            order_id: "order1".into(),
        }, 2).unwrap();

        mempool.add(Transaction::Deposit {
            trader: "charlie".into(),
            amount: 10000,
        }, 3).unwrap();

        // Should come out in bucket order
        let block = mempool.prepare_block(10);

        assert_eq!(block.len(), 3);
        assert!(matches!(block[0], Transaction::Deposit { .. }));
        assert!(matches!(block[1], Transaction::CancelOrder { .. }));
        assert!(matches!(block[2], Transaction::PlaceOrder { .. }));
    }

    #[test]
    fn test_fifo_within_bucket() {
        let mut mempool = Mempool::new(100);

        // Add two orders
        mempool.add(Transaction::PlaceOrder {
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 50000,
            size: 100,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }, 1).unwrap();

        mempool.add(Transaction::PlaceOrder {
            trader: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            price: 51000,
            size: 100,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }, 2).unwrap();

        let block = mempool.prepare_block(10);

        // First order should be alice's (FIFO)
        if let Transaction::PlaceOrder { trader, .. } = &block[0] {
            assert_eq!(trader, "alice");
        } else {
            panic!("Expected PlaceOrder");
        }
    }

    #[test]
    fn test_bucket_full() {
        let mut mempool = Mempool::new(1);

        mempool.add(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        }, 1).unwrap();

        // Should fail - bucket full
        let result = mempool.add(Transaction::Deposit {
            trader: "bob".into(),
            amount: 200,
        }, 2);

        assert!(matches!(result, Err(MempoolError::BucketFull)));
    }

    #[test]
    fn test_max_txs_limit() {
        let mut mempool = Mempool::new(100);

        for i in 0..10 {
            mempool.add(Transaction::Deposit {
                trader: format!("trader{}", i),
                amount: 100,
            }, i as u64).unwrap();
        }

        // Only get 3
        let block = mempool.prepare_block(3);
        assert_eq!(block.len(), 3);

        // 7 should remain
        assert_eq!(mempool.len(), 7);
    }

    #[test]
    fn test_per_address_limit() {
        // Max 3 pending txs per address
        let mut mempool = Mempool::with_config(100, 3, 0);

        // Add 3 txs from alice - should work
        for i in 0..3 {
            mempool.add(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100 + i,
            }, i as u64).unwrap();
        }

        // 4th tx from alice should fail
        let result = mempool.add(Transaction::Deposit {
            trader: "alice".into(),
            amount: 200,
        }, 4);
        assert!(matches!(result, Err(MempoolError::AddressLimitReached)));

        // But bob can still add
        mempool.add(Transaction::Deposit {
            trader: "bob".into(),
            amount: 100,
        }, 5).unwrap();
    }

    #[test]
    fn test_per_address_count_tracking() {
        let mut mempool = Mempool::with_config(100, 10, 0);

        // Add 3 txs from alice
        let mut hashes = Vec::new();
        for i in 0..3 {
            let hash = mempool.add(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100 + i,
            }, i as u64).unwrap();
            hashes.push(hash);
        }

        assert_eq!(mempool.per_address_count.get("alice"), Some(&3));

        // Commit 2 txs
        mempool.commit_proposal_unchecked(&hashes[0..2]);

        // Count should decrement to 1
        assert_eq!(mempool.per_address_count.get("alice"), Some(&1));
        assert_eq!(mempool.len(), 1);

        // Remove last one
        mempool.remove(&hashes[2]);
        assert_eq!(mempool.per_address_count.get("alice"), None);
    }

    #[test]
    fn test_age_eviction() {
        // Max age: 1000ms
        let mut mempool = Mempool::with_config(100, 100, 1000);

        // Add old tx (timestamp 0)
        mempool.add(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        }, 0).unwrap();

        // Add recent tx (timestamp 900)
        mempool.add(Transaction::Deposit {
            trader: "bob".into(),
            amount: 200,
        }, 900).unwrap();

        assert_eq!(mempool.len(), 2);

        // Prune at time 1001 (alice's tx is stale)
        let pruned = mempool.prune_stale(1001);
        assert_eq!(pruned, 1);
        assert_eq!(mempool.len(), 1);

        // Bob's tx should remain
        let block = mempool.prepare_block(10);
        assert_eq!(block.len(), 1);
        if let Transaction::Deposit { trader, .. } = &block[0] {
            assert_eq!(trader, "bob");
        }
    }

    #[test]
    fn test_age_eviction_disabled() {
        // Max age: 0 = disabled
        let mut mempool = Mempool::with_config(100, 100, 0);

        mempool.add(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100,
        }, 0).unwrap();

        // Should not prune even at very high time
        let pruned = mempool.prune_stale(1_000_000_000);
        assert_eq!(pruned, 0);
        assert_eq!(mempool.len(), 1);
    }

    #[test]
    fn test_prepare_block_decrements_address_count() {
        let mut mempool = Mempool::with_config(100, 10, 0);

        // Add 5 txs from alice
        for i in 0..5 {
            mempool.add(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100 + i,
            }, i as u64).unwrap();
        }

        assert_eq!(mempool.per_address_count.get("alice"), Some(&5));

        // prepare_block should drain and decrement counts
        let block = mempool.prepare_block(3);
        assert_eq!(block.len(), 3);
        assert_eq!(mempool.per_address_count.get("alice"), Some(&2));

        // Drain remaining
        let block = mempool.prepare_block(10);
        assert_eq!(block.len(), 2);
        assert_eq!(mempool.per_address_count.get("alice"), None); // Removed when 0
    }

    #[test]
    fn test_prepare_block_allows_resubmission() {
        // Regression: prepare_block must decrement counts so new txs can be submitted
        let mut mempool = Mempool::with_config(100, 3, 0);

        // Fill to address limit
        for i in 0..3 {
            mempool.add(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100 + i,
            }, i as u64).unwrap();
        }

        // Should be at limit
        let result = mempool.add(Transaction::Deposit {
            trader: "alice".into(),
            amount: 999,
        }, 10);
        assert!(matches!(result, Err(MempoolError::AddressLimitReached)));

        // Drain all via prepare_block
        let block = mempool.prepare_block(10);
        assert_eq!(block.len(), 3);

        // Should be able to submit again
        mempool.add(Transaction::Deposit {
            trader: "alice".into(),
            amount: 200,
        }, 11).unwrap();
        assert_eq!(mempool.per_address_count.get("alice"), Some(&1));
    }

    #[test]
    fn test_drain_block_decrements_address_count() {
        let mut mempool = Mempool::with_config(100, 10, 0);

        for i in 0..4 {
            mempool.add(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100 + i,
            }, i as u64).unwrap();
        }

        assert_eq!(mempool.per_address_count.get("alice"), Some(&4));

        // drain_block should also decrement counts
        mempool.drain_block(2);
        assert_eq!(mempool.per_address_count.get("alice"), Some(&2));
        assert_eq!(mempool.len(), 2);

        mempool.drain_block(2);
        assert_eq!(mempool.per_address_count.get("alice"), None);
        assert_eq!(mempool.len(), 0);
    }
}
