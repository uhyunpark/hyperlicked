//! Incremental State Hashing
//!
//! Provides O(k) state hashing where k is the number of changed accounts,
//! instead of O(n) where n is total accounts.
//!
//! ## Architecture
//!
//! State is divided into buckets by address prefix (first byte).
//! Each bucket maintains its own hash. Only dirty buckets are rehashed.
//!
//! ```text
//! Root Hash = H(bucket_00 || bucket_01 || ... || bucket_ff || globals)
//!
//! bucket_XX = H(account_1 || account_2 || ... || account_n)
//!              where all accounts have address prefix XX
//! ```
//!
//! ## Usage
//!
//! 1. Call `mark_dirty_account(address)` when account state changes
//! 2. Call `compute_root()` at end of block to get state hash
//! 3. Call `clear_dirty()` after block commit

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::app::accounts::Account;
use crate::types::Hash;

/// Number of account buckets (256 = one per first byte of address hash)
const NUM_BUCKETS: usize = 256;

/// Incremental state hasher
pub struct IncrementalHasher {
    /// Hash of each account bucket
    bucket_hashes: [Hash; NUM_BUCKETS],
    /// Which buckets need rehashing
    dirty_buckets: HashSet<u8>,
    /// Hash of global state (insurance, funding, staking)
    globals_hash: Hash,
    /// Whether globals need rehashing
    globals_dirty: bool,
    /// Cached root hash (None if invalid)
    cached_root: Option<Hash>,
}

impl IncrementalHasher {
    /// Create a new hasher with empty state
    ///
    /// All buckets start as dirty to ensure first compute_root hashes everything.
    pub fn new() -> Self {
        let mut dirty_buckets = HashSet::new();
        for i in 0..NUM_BUCKETS {
            dirty_buckets.insert(i as u8);
        }
        Self {
            bucket_hashes: [[0u8; 32]; NUM_BUCKETS],
            dirty_buckets,
            globals_hash: [0u8; 32],
            globals_dirty: true,
            cached_root: None,
        }
    }

    /// Mark an account as dirty (needs rehashing)
    pub fn mark_dirty_account(&mut self, address: &str) {
        let bucket = address_to_bucket(address);
        self.dirty_buckets.insert(bucket);
        self.cached_root = None;
    }

    /// Mark all accounts as dirty (e.g., after funding which affects all positions)
    pub fn mark_all_accounts_dirty(&mut self) {
        for i in 0..NUM_BUCKETS {
            self.dirty_buckets.insert(i as u8);
        }
        self.cached_root = None;
    }

    /// Mark global state as dirty
    pub fn mark_globals_dirty(&mut self) {
        self.globals_dirty = true;
        self.cached_root = None;
    }

    /// Compute root hash, rehashing only dirty portions
    ///
    /// Returns the state hash for Byzantine detection.
    pub fn compute_root(
        &mut self,
        accounts: &[Account],
        globals: &GlobalState,
    ) -> Hash {
        // Return cached if nothing changed
        if let Some(root) = self.cached_root {
            return root;
        }

        // Rehash dirty buckets
        if !self.dirty_buckets.is_empty() {
            // Group accounts by bucket
            let mut buckets: HashMap<u8, Vec<&Account>> = HashMap::new();
            for account in accounts {
                let bucket = address_to_bucket(&account.address);
                if self.dirty_buckets.contains(&bucket) {
                    buckets.entry(bucket).or_default().push(account);
                }
            }

            // Rehash each dirty bucket
            for bucket_id in self.dirty_buckets.drain() {
                let bucket_accounts = buckets.get(&bucket_id);
                self.bucket_hashes[bucket_id as usize] = hash_bucket(bucket_accounts);
            }
        }

        // Rehash globals if dirty
        if self.globals_dirty {
            self.globals_hash = hash_globals(globals);
            self.globals_dirty = false;
        }

        // Compute root from all bucket hashes + globals
        let root = compute_root_hash(&self.bucket_hashes, &self.globals_hash);
        self.cached_root = Some(root);
        root
    }

    /// Clear dirty flags after block commit
    pub fn clear_dirty(&mut self) {
        self.dirty_buckets.clear();
        self.globals_dirty = false;
    }

    /// Rebuild all hashes from scratch (e.g., after snapshot recovery)
    pub fn rebuild(&mut self, accounts: &[Account], globals: &GlobalState) {
        // Mark everything dirty
        for i in 0..NUM_BUCKETS {
            self.dirty_buckets.insert(i as u8);
        }
        self.globals_dirty = true;
        self.cached_root = None;

        // Recompute
        self.compute_root(accounts, globals);
    }

    /// Get statistics about the hasher state
    pub fn stats(&self) -> HasherStats {
        HasherStats {
            dirty_buckets: self.dirty_buckets.len(),
            total_buckets: NUM_BUCKETS,
            globals_dirty: self.globals_dirty,
            has_cached_root: self.cached_root.is_some(),
        }
    }
}

impl Default for IncrementalHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the hasher state
#[derive(Debug, Clone)]
pub struct HasherStats {
    pub dirty_buckets: usize,
    pub total_buckets: usize,
    pub globals_dirty: bool,
    pub has_cached_root: bool,
}

/// Global state that's hashed separately from accounts
#[derive(Debug, Clone)]
pub struct GlobalState {
    pub insurance_fund: i64,
    pub funding_rates: Vec<(String, i64)>,
    pub last_funding_times: Vec<(String, u64)>,
    pub mark_prices: Vec<(String, i64)>,
    pub orderbook_state: Vec<OrderbookSnapshot>,
    pub staking_state: StakingSnapshot,
    pub trigger_count: usize,
    pub oracle_enabled: bool,
}

/// Minimal orderbook state for hashing
#[derive(Debug, Clone)]
pub struct OrderbookSnapshot {
    pub symbol: String,
    pub best_bid: i64,
    pub best_ask: i64,
    pub last_price: i64,
}

/// Minimal staking state for hashing
#[derive(Debug, Clone)]
pub struct StakingSnapshot {
    pub current_epoch: u64,
    pub total_staked: i64,
    pub rewards_pool: i64,
    pub validator_count: usize,
    pub delegation_count: usize,
}

/// Map address to bucket (0-255)
fn address_to_bucket(address: &str) -> u8 {
    // Hash the address and use first byte as bucket
    let hash = Sha256::digest(address.as_bytes());
    hash[0]
}

/// Hash a single bucket of accounts
fn hash_bucket(accounts: Option<&Vec<&Account>>) -> Hash {
    let mut hasher = Sha256::new();

    if let Some(accts) = accounts {
        // Sort accounts for determinism
        let mut sorted: Vec<_> = accts.iter().collect();
        sorted.sort_by_key(|a| &a.address);

        for account in sorted {
            hasher.update(account.address.as_bytes());
            hasher.update(account.balance.to_le_bytes());
            hasher.update(account.locked.to_le_bytes());
            hasher.update(account.nonce.to_le_bytes());

            // Hash positions (sorted)
            let mut symbols: Vec<_> = account.positions.keys().collect();
            symbols.sort();
            for symbol in symbols {
                if let Some(pos) = account.positions.get(symbol) {
                    hasher.update(symbol.as_bytes());
                    hasher.update(pos.size.to_le_bytes());
                    hasher.update(pos.entry_price.to_le_bytes());
                    hasher.update(pos.realized_pnl.to_le_bytes());
                    hasher.update(pos.cumulative_funding.to_le_bytes());
                }
            }
        }
    }

    hasher.finalize().into()
}

/// Hash global state
fn hash_globals(globals: &GlobalState) -> Hash {
    let mut hasher = Sha256::new();

    // Insurance fund
    hasher.update(globals.insurance_fund.to_le_bytes());

    // Funding rates (sorted)
    let mut rates = globals.funding_rates.clone();
    rates.sort_by(|a, b| a.0.cmp(&b.0));
    for (symbol, rate) in &rates {
        hasher.update(symbol.as_bytes());
        hasher.update(rate.to_le_bytes());
    }

    // Last funding times (sorted)
    let mut times = globals.last_funding_times.clone();
    times.sort_by(|a, b| a.0.cmp(&b.0));
    for (symbol, time) in &times {
        hasher.update(symbol.as_bytes());
        hasher.update(time.to_le_bytes());
    }

    // Mark prices (sorted)
    let mut prices = globals.mark_prices.clone();
    prices.sort_by(|a, b| a.0.cmp(&b.0));
    for (symbol, price) in &prices {
        hasher.update(symbol.as_bytes());
        hasher.update(price.to_le_bytes());
    }

    // Orderbook state (sorted)
    let mut books = globals.orderbook_state.clone();
    books.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    for book in &books {
        hasher.update(book.symbol.as_bytes());
        hasher.update(book.best_bid.to_le_bytes());
        hasher.update(book.best_ask.to_le_bytes());
        hasher.update(book.last_price.to_le_bytes());
    }

    // Staking state
    hasher.update(globals.staking_state.current_epoch.to_le_bytes());
    hasher.update(globals.staking_state.total_staked.to_le_bytes());
    hasher.update(globals.staking_state.rewards_pool.to_le_bytes());
    hasher.update((globals.staking_state.validator_count as u64).to_le_bytes());
    hasher.update((globals.staking_state.delegation_count as u64).to_le_bytes());

    // Trigger order count (not individual orders for simplicity)
    hasher.update((globals.trigger_count as u64).to_le_bytes());

    // Oracle flag
    hasher.update(&[if globals.oracle_enabled { 1 } else { 0 }]);

    hasher.finalize().into()
}

/// Compute root hash from bucket hashes and globals hash
fn compute_root_hash(bucket_hashes: &[Hash; NUM_BUCKETS], globals_hash: &Hash) -> Hash {
    let mut hasher = Sha256::new();

    // Hash all bucket hashes
    for bucket_hash in bucket_hashes {
        hasher.update(bucket_hash);
    }

    // Add globals hash
    hasher.update(globals_hash);

    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_to_bucket() {
        // Same address should always map to same bucket
        let bucket1 = address_to_bucket("alice");
        let bucket2 = address_to_bucket("alice");
        assert_eq!(bucket1, bucket2);

        // Different addresses likely map to different buckets
        let bucket_alice = address_to_bucket("alice");
        let bucket_bob = address_to_bucket("bob");
        // Note: could theoretically be same, but very unlikely
        assert_ne!(bucket_alice, bucket_bob);
    }

    #[test]
    fn test_incremental_hashing() {
        let mut hasher = IncrementalHasher::new();

        let accounts = vec![
            Account::new("alice"),
            Account::new("bob"),
        ];

        let globals = GlobalState {
            insurance_fund: 1000,
            funding_rates: vec![],
            last_funding_times: vec![],
            mark_prices: vec![],
            orderbook_state: vec![],
            staking_state: StakingSnapshot {
                current_epoch: 0,
                total_staked: 0,
                rewards_pool: 0,
                validator_count: 0,
                delegation_count: 0,
            },
            trigger_count: 0,
            oracle_enabled: false,
        };

        // Initial computation
        let hash1 = hasher.compute_root(&accounts, &globals);

        // Should return cached
        let hash2 = hasher.compute_root(&accounts, &globals);
        assert_eq!(hash1, hash2);

        // Mark one account dirty
        hasher.mark_dirty_account("alice");

        // After marking dirty, should still compute same hash (data didn't change)
        let hash3 = hasher.compute_root(&accounts, &globals);
        assert_eq!(hash1, hash3);
    }

    #[test]
    fn test_dirty_tracking() {
        let mut hasher = IncrementalHasher::new();

        // New hasher starts with all buckets dirty
        assert_eq!(hasher.stats().dirty_buckets, NUM_BUCKETS);

        hasher.clear_dirty();
        assert_eq!(hasher.stats().dirty_buckets, 0);
        assert!(!hasher.stats().globals_dirty);

        hasher.mark_dirty_account("alice");
        assert!(hasher.stats().dirty_buckets >= 1);

        hasher.mark_globals_dirty();
        assert!(hasher.stats().globals_dirty);

        hasher.clear_dirty();
        assert_eq!(hasher.stats().dirty_buckets, 0);
        assert!(!hasher.stats().globals_dirty);
    }

    #[test]
    fn test_different_state_different_hash() {
        let mut hasher1 = IncrementalHasher::new();
        let mut hasher2 = IncrementalHasher::new();

        let accounts1 = vec![Account::new("alice")];
        let mut accounts2 = vec![Account::new("alice")];
        accounts2[0].balance = 1000;

        let globals = GlobalState {
            insurance_fund: 0,
            funding_rates: vec![],
            last_funding_times: vec![],
            mark_prices: vec![],
            orderbook_state: vec![],
            staking_state: StakingSnapshot {
                current_epoch: 0,
                total_staked: 0,
                rewards_pool: 0,
                validator_count: 0,
                delegation_count: 0,
            },
            trigger_count: 0,
            oracle_enabled: false,
        };

        hasher1.mark_all_accounts_dirty();
        hasher2.mark_all_accounts_dirty();

        let hash1 = hasher1.compute_root(&accounts1, &globals);
        let hash2 = hasher2.compute_root(&accounts2, &globals);

        assert_ne!(hash1, hash2);
    }
}
