//! Event-Driven Liquidation Queue
//!
//! Maintains a priority queue of accounts by health factor, so liquidation
//! checks only process the riskiest accounts instead of scanning all accounts.
//!
//! ## How It Works
//!
//! 1. Accounts with positions are tracked with their estimated health factor
//! 2. When positions change (fills, funding), the queue is updated
//! 3. Each block, only the top-N riskiest accounts are checked for liquidation
//! 4. Healthy accounts are skipped entirely

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::types::Price;

use super::accounts::AccountManager;
use super::state::MAINTENANCE_MARGIN_BPS;
use super::Symbol;

/// Health factor in basis points (10000 = 100% = exactly at maintenance margin)
pub type HealthBps = i64;

/// Entry in the liquidation queue
#[derive(Debug, Clone)]
struct QueueEntry {
    /// Account address
    address: String,
    /// Cached health factor (lower = riskier)
    health_bps: HealthBps,
    /// Generation counter to handle stale entries
    generation: u64,
}

// Order by health (lowest first = riskiest)
impl Eq for QueueEntry {}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
            && self.health_bps == other.health_bps
            && self.generation == other.generation
    }
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering: lower health = higher priority
        other
            .health_bps
            .cmp(&self.health_bps)
            // For equal health, lower address is higher priority.  Without
            // this tie-breaker BinaryHeap order depends on insertion order.
            .then_with(|| other.address.cmp(&self.address))
            .then_with(|| self.generation.cmp(&other.generation))
    }
}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Event-driven liquidation queue
///
/// Tracks accounts by health factor for efficient liquidation scanning.
pub struct LiquidationQueue {
    /// Priority queue ordered by health factor (lowest first)
    queue: BinaryHeap<QueueEntry>,
    /// Current generation for each address (to invalidate stale entries)
    generations: HashMap<String, u64>,
    /// Addresses currently in the queue
    in_queue: HashSet<String>,
    /// Maximum accounts to check per block
    max_check_per_block: usize,
}

impl LiquidationQueue {
    /// Create a new liquidation queue
    pub fn new(max_check_per_block: usize) -> Self {
        Self {
            queue: BinaryHeap::new(),
            generations: HashMap::new(),
            in_queue: HashSet::new(),
            max_check_per_block,
        }
    }

    /// Update an account's health factor in the queue
    ///
    /// Call this when:
    /// - Account receives a fill (position changes)
    /// - Funding is applied
    /// - Mark prices update significantly
    pub fn update_account(
        &mut self,
        address: &str,
        accounts: &AccountManager,
        mark_prices: &HashMap<Symbol, Price>,
    ) {
        let addr = address.to_lowercase();

        // Calculate current health factor
        let health_bps = match accounts.get(&addr) {
            Some(account) => {
                // Skip accounts with no positions
                if account.positions.values().all(|p| p.size == 0) {
                    self.remove_account(&addr);
                    return;
                }
                calculate_health_bps(account, mark_prices)
            }
            None => {
                self.remove_account(&addr);
                return;
            }
        };

        // Increment generation to invalidate any existing entry
        let generation = self.generations.entry(addr.clone()).or_insert(0);
        *generation += 1;

        // Add new entry with current health
        self.queue.push(QueueEntry {
            address: addr.clone(),
            health_bps,
            generation: *generation,
        });
        self.in_queue.insert(addr);
    }

    /// Remove an account from the queue (e.g., when position closed)
    pub fn remove_account(&mut self, address: &str) {
        let addr = address.to_lowercase();
        // Increment generation to invalidate any existing entries
        if let Some(gen) = self.generations.get_mut(&addr) {
            *gen += 1;
        }
        self.in_queue.remove(&addr);
    }

    /// Get addresses that need liquidation check this block
    ///
    /// Returns up to `max_check_per_block` addresses, starting with riskiest.
    pub fn get_accounts_to_check(&mut self) -> Vec<String> {
        let mut result = Vec::with_capacity(self.max_check_per_block);
        let mut seen = HashSet::new();

        while result.len() < self.max_check_per_block {
            let entry = match self.queue.pop() {
                Some(e) => e,
                None => break,
            };

            // Skip stale entries
            let current_gen = self.generations.get(&entry.address).copied().unwrap_or(0);
            if entry.generation != current_gen {
                continue;
            }

            // Skip duplicates
            if seen.contains(&entry.address) {
                continue;
            }

            seen.insert(entry.address.clone());
            result.push(entry.address);
        }

        result
    }

    /// Rebuild the queue from scratch (e.g., after snapshot recovery)
    pub fn rebuild(&mut self, accounts: &AccountManager, mark_prices: &HashMap<Symbol, Price>) {
        self.queue.clear();
        self.generations.clear();
        self.in_queue.clear();

        for account in accounts.all_accounts() {
            // Skip accounts with no positions
            if account.positions.values().all(|p| p.size == 0) {
                continue;
            }

            let health_bps = calculate_health_bps(&account, mark_prices);
            let addr = account.address.clone();

            self.generations.insert(addr.clone(), 1);
            self.in_queue.insert(addr.clone());
            self.queue.push(QueueEntry {
                address: addr,
                health_bps,
                generation: 1,
            });
        }
    }

    /// Get number of accounts in the queue
    pub fn len(&self) -> usize {
        self.in_queue.len()
    }

    /// Check if queue is empty
    pub fn is_empty(&self) -> bool {
        self.in_queue.is_empty()
    }
}

impl Default for LiquidationQueue {
    fn default() -> Self {
        Self::new(100) // Default to checking 100 accounts per block
    }
}

/// Calculate health factor in basis points
///
/// health_bps = (equity * 10000) / maintenance_margin
///
/// - 10000 = exactly at maintenance margin (100%)
/// - < 10000 = below maintenance, liquidatable
/// - > 10000 = above maintenance, healthy
fn calculate_health_bps(
    account: &super::accounts::Account,
    mark_prices: &HashMap<Symbol, Price>,
) -> HealthBps {
    let equity = account.equity(mark_prices);
    let maintenance = account.maintenance_margin_required(mark_prices, MAINTENANCE_MARGIN_BPS);

    if maintenance == 0 {
        return i64::MAX; // No positions, infinitely healthy
    }

    // health_bps = equity * 10000 / maintenance
    // Cap to prevent overflow
    let capped_equity = equity.min(i64::MAX / 10000);
    (capped_equity * 10000) / maintenance
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::accounts::Account;

    fn create_test_account(address: &str, balance: i64, btc_position: i64) -> Account {
        let mut account = Account::new(address);
        account.balance = balance;
        if btc_position != 0 {
            account.apply_fill("BTC-USDT", btc_position > 0, btc_position.abs(), 5_000_000);
        }
        account
    }

    #[test]
    fn test_health_factor_calculation() {
        let account = create_test_account("trader", 500_000, 100_000_000); // $5k, 1 BTC long
        let mut mark_prices = HashMap::new();

        // At $50k, equity = $5k, maintenance = $2.5k (5%), health = 200%
        mark_prices.insert("BTC-USDT".to_string(), 5_000_000);
        let health = calculate_health_bps(&account, &mark_prices);
        assert_eq!(health, 20000); // 200%

        // At $46k, unrealized = -$4k, equity = $1k, maintenance = $2.3k
        mark_prices.insert("BTC-USDT".to_string(), 4_600_000);
        let health = calculate_health_bps(&account, &mark_prices);
        assert!(health < 10000); // Below maintenance
    }

    #[test]
    fn test_queue_ordering() {
        let mut queue = LiquidationQueue::new(10);
        let mut accounts = AccountManager::new();
        let mark_prices: HashMap<Symbol, Price> =
            [("BTC-USDT".to_string(), 5_000_000)].into_iter().collect();

        // Create accounts with different risk levels
        let account1 = create_test_account("risky", 250_000, 100_000_000); // $2.5k, 1 BTC
        let account2 = create_test_account("safe", 1_000_000, 100_000_000); // $10k, 1 BTC
        let account3 = create_test_account("medium", 500_000, 100_000_000); // $5k, 1 BTC

        // Add to account manager
        *accounts.get_or_create("risky") = account1;
        *accounts.get_or_create("safe") = account2;
        *accounts.get_or_create("medium") = account3;

        // Update queue
        queue.update_account("risky", &accounts, &mark_prices);
        queue.update_account("safe", &accounts, &mark_prices);
        queue.update_account("medium", &accounts, &mark_prices);

        // Get accounts - should be ordered by risk (lowest health first)
        let to_check = queue.get_accounts_to_check();
        assert_eq!(to_check.len(), 3);
        assert_eq!(to_check[0], "risky"); // Lowest health first
        assert_eq!(to_check[2], "safe"); // Highest health last
    }

    #[test]
    fn test_equal_health_accounts_are_address_ordered() {
        fn build_queue(order: &[&str]) -> Vec<String> {
            let mut queue = LiquidationQueue::new(10);
            let mut accounts = AccountManager::new();
            let mark_prices: HashMap<Symbol, Price> =
                [("BTC-USDT".to_string(), 5_000_000)].into_iter().collect();

            for address in order {
                *accounts.get_or_create(address) =
                    create_test_account(address, 500_000, 100_000_000);
                queue.update_account(address, &accounts, &mark_prices);
            }
            queue.get_accounts_to_check()
        }

        assert_eq!(
            build_queue(&["charlie", "alice", "bob"]),
            vec!["alice", "bob", "charlie"]
        );
        assert_eq!(
            build_queue(&["bob", "charlie", "alice"]),
            vec!["alice", "bob", "charlie"]
        );
    }

    #[test]
    fn test_account_removal() {
        let mut queue = LiquidationQueue::new(10);
        let mut accounts = AccountManager::new();
        let mark_prices: HashMap<Symbol, Price> =
            [("BTC-USDT".to_string(), 5_000_000)].into_iter().collect();

        let account = create_test_account("trader", 500_000, 100_000_000);
        *accounts.get_or_create("trader") = account;

        queue.update_account("trader", &accounts, &mark_prices);
        assert_eq!(queue.len(), 1);

        queue.remove_account("trader");
        assert_eq!(queue.len(), 0);

        // Stale entries should be skipped
        let to_check = queue.get_accounts_to_check();
        assert!(to_check.is_empty());
    }

    #[test]
    fn test_rebuild() {
        let mut queue = LiquidationQueue::new(10);
        let mut accounts = AccountManager::new();
        let mark_prices: HashMap<Symbol, Price> =
            [("BTC-USDT".to_string(), 5_000_000)].into_iter().collect();

        // Create multiple accounts
        *accounts.get_or_create("alice") = create_test_account("alice", 500_000, 100_000_000);
        *accounts.get_or_create("bob") = create_test_account("bob", 500_000, 50_000_000);
        *accounts.get_or_create("charlie") = create_test_account("charlie", 500_000, 0); // No position

        queue.rebuild(&accounts, &mark_prices);

        // Only accounts with positions should be in queue
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn test_max_check_limit() {
        let mut queue = LiquidationQueue::new(2); // Only check 2 per block
        let mut accounts = AccountManager::new();
        let mark_prices: HashMap<Symbol, Price> =
            [("BTC-USDT".to_string(), 5_000_000)].into_iter().collect();

        // Create 5 accounts
        for i in 0..5 {
            let addr = format!("trader{}", i);
            *accounts.get_or_create(&addr) =
                create_test_account(&addr, 500_000 + (i as i64 * 100_000), 100_000_000);
            queue.update_account(&addr, &accounts, &mark_prices);
        }

        // Should only return 2 (the limit)
        let to_check = queue.get_accounts_to_check();
        assert_eq!(to_check.len(), 2);
    }
}
