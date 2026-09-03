//! Account Management
//!
//! Tracks trader balances, positions, and margin.
//! Uses integer math (satoshis/cents) for determinism.
//!
//! ## Nonce Gap Handling
//!
//! Allows transactions to arrive out of order with a configurable gap tolerance.
//! If a transaction with nonce N+k arrives before N+1 through N+k-1, it will be
//! accepted if k <= MAX_NONCE_GAP.

use std::collections::{hash_map::RandomState, BTreeSet, HashMap};
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::positions::Position;
use super::{Address, MarketConfig, Symbol};
use crate::types::{Price, Size};

/// Maximum allowed gap in nonces before rejection
pub const MAX_NONCE_GAP: u64 = 10;

/// Trader account
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub address: Address,
    /// Free collateral (in cents, e.g., USDC)
    pub balance: i64,
    /// Liquid native HYCK balance in HYCK base units.
    ///
    /// This is deliberately separate from [`Self::balance`], which remains
    /// the perp collateral balance (cents).  Native staking transactions
    /// must only debit/credit this field.
    #[serde(default)]
    pub hyck_balance: i64,
    /// Collateral locked in positions
    pub locked: i64,
    /// Positions by symbol
    pub positions: HashMap<Symbol, Position>,
    /// Next expected nonce (for replay protection)
    #[serde(default)]
    pub nonce: u64,
    /// Pending nonces that have been used out-of-order (for gap handling)
    /// These are nonces > current nonce that have already been accepted
    #[serde(default)]
    pub pending_nonces: BTreeSet<u64>,
}

/// Result of nonce validation with gap handling
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonceResult {
    /// Nonce is exactly what we expected
    Valid,
    /// Nonce is within gap tolerance (accepted but out of order)
    ValidWithGap,
    /// Nonce is too low (already used)
    TooLow { expected: u64 },
    /// Nonce is too far ahead
    GapTooLarge {
        expected: u64,
        got: u64,
        max_gap: u64,
    },
    /// Nonce has already been used (duplicate within gap window)
    AlreadyUsed,
    /// Nonce cannot be consumed because its successor is not representable.
    Exhausted,
}

impl Account {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            balance: 0,
            hyck_balance: 0,
            locked: 0,
            positions: HashMap::new(),
            nonce: 0,
            pending_nonces: BTreeSet::new(),
        }
    }

    /// Check if nonce is valid (must be exactly current nonce)
    /// Legacy function - use validate_nonce_with_gap for gap tolerance
    pub fn validate_nonce(&self, nonce: u64) -> bool {
        nonce == self.nonce
    }

    /// Validate nonce with gap tolerance
    ///
    /// Allows out-of-order transactions within MAX_NONCE_GAP of expected nonce.
    pub fn validate_nonce_with_gap(&self, nonce: u64) -> NonceResult {
        // Exact match - ideal case
        if nonce == self.nonce {
            if self.next_nonce_after_exact().is_none() {
                return NonceResult::Exhausted;
            }
            return NonceResult::Valid;
        }

        // Too low - already used or before expected
        if nonce < self.nonce {
            return NonceResult::TooLow {
                expected: self.nonce,
            };
        }

        // Check gap tolerance
        let gap = nonce.saturating_sub(self.nonce);
        if gap > MAX_NONCE_GAP {
            return NonceResult::GapTooLarge {
                expected: self.nonce,
                got: nonce,
                max_gap: MAX_NONCE_GAP,
            };
        }

        // There is no representable successor to u64::MAX. Treat it as a
        // terminal nonce rather than allowing a later increment to wrap back
        // to zero and reopen replay protection. Keep the out-of-range result
        // above so a far-future MAX nonce is still classified as a gap error.
        if nonce == u64::MAX {
            return NonceResult::Exhausted;
        }

        // Within gap - check if already used
        if self.pending_nonces.contains(&nonce) {
            return NonceResult::AlreadyUsed;
        }

        NonceResult::ValidWithGap
    }

    /// Use a nonce with gap handling
    ///
    /// If nonce == expected, increments normally and clears pending.
    /// If nonce > expected (within gap), adds to pending_nonces.
    pub fn use_nonce_with_gap(&mut self, nonce: u64) -> Result<(), AccountError> {
        // Keep this public mutator fail-closed even when called directly by a
        // fixture or another subsystem instead of through AccountManager.
        // Otherwise an out-of-range nonce could be inserted into the pending
        // set and only discovered much later during state validation.
        match self.validate_nonce_with_gap(nonce) {
            NonceResult::Valid | NonceResult::ValidWithGap => {}
            NonceResult::TooLow { expected } => {
                return Err(AccountError::InvalidNonce {
                    expected,
                    got: nonce,
                });
            }
            NonceResult::GapTooLarge {
                expected,
                got,
                max_gap,
            } => {
                return Err(AccountError::NonceGapTooLarge {
                    expected,
                    got,
                    max_gap,
                });
            }
            NonceResult::AlreadyUsed => return Err(AccountError::NonceAlreadyUsed { nonce }),
            NonceResult::Exhausted => return Err(AccountError::NonceOverflow),
        }

        if nonce == self.nonce {
            // Compute the complete advancement before mutating anything so an
            // imported state near u64::MAX fails atomically.
            let next_nonce = self
                .next_nonce_after_exact()
                .ok_or(AccountError::NonceOverflow)?;
            let mut consumed = self
                .nonce
                .checked_add(1)
                .ok_or(AccountError::NonceOverflow)?;
            while consumed < next_nonce {
                self.pending_nonces.remove(&consumed);
                consumed = consumed.checked_add(1).ok_or(AccountError::NonceOverflow)?;
            }
            self.nonce = next_nonce;
        } else if nonce > self.nonce {
            // Out of order - add to pending
            self.pending_nonces.insert(nonce);
        }
        Ok(())
    }

    /// Increment nonce after successful transaction
    pub fn increment_nonce(&mut self) -> Result<(), AccountError> {
        self.nonce = self
            .nonce
            .checked_add(1)
            .ok_or(AccountError::NonceOverflow)?;
        Ok(())
    }

    /// Return the next nonce after consuming the current one and any
    /// contiguous pending markers. `None` means the state cannot advance
    /// without overflowing u64.
    fn next_nonce_after_exact(&self) -> Option<u64> {
        let mut next = self.nonce.checked_add(1)?;
        while self.pending_nonces.contains(&next) {
            next = next.checked_add(1)?;
        }
        Some(next)
    }

    /// Get current nonce (for API responses)
    pub fn current_nonce(&self) -> u64 {
        self.nonce
    }

    /// Total equity = balance + locked + unrealized PnL
    pub fn equity(&self, mark_prices: &HashMap<Symbol, Price>) -> i64 {
        let unrealized: i64 = self
            .positions
            .iter()
            .map(|(symbol, pos)| {
                mark_prices
                    .get(symbol)
                    .map(|&mark| pos.unrealized_pnl(mark))
                    .unwrap_or(0)
            })
            .sum();

        self.balance + self.locked + unrealized
    }

    /// Available margin for new orders
    pub fn available_margin(&self) -> i64 {
        self.balance
    }

    /// Calculate total maintenance margin required across all positions
    /// maintenance_rate_bps: maintenance margin rate in basis points (500 = 5%)
    pub fn maintenance_margin_required(
        &self,
        mark_prices: &HashMap<Symbol, Price>,
        maintenance_rate_bps: i64,
    ) -> i64 {
        self.positions
            .iter()
            .filter(|(_, pos)| pos.size != 0)
            .map(|(symbol, pos)| {
                let mark = mark_prices.get(symbol).copied().unwrap_or(pos.entry_price);
                let notional = pos.notional(mark);
                ((notional as i128 * maintenance_rate_bps as i128) / 10000) as i64
            })
            .sum()
    }

    /// Check if account should be liquidated
    /// Returns true if equity < maintenance margin required
    pub fn is_liquidatable(
        &self,
        mark_prices: &HashMap<Symbol, Price>,
        maintenance_rate_bps: i64,
    ) -> bool {
        // No positions = not liquidatable
        if self.positions.values().all(|p| p.size == 0) {
            return false;
        }

        let equity = self.equity(mark_prices);
        let maintenance = self.maintenance_margin_required(mark_prices, maintenance_rate_bps);

        equity < maintenance
    }

    /// Get position for a symbol (or empty)
    pub fn position(&self, symbol: &str) -> Position {
        self.positions.get(symbol).cloned().unwrap_or_default()
    }

    /// Update position after a fill
    pub fn apply_fill(
        &mut self,
        symbol: &str,
        side_is_buy: bool,
        fill_size: Size,
        fill_price: Price,
    ) {
        let pos = self.positions.entry(symbol.to_string()).or_default();

        let fill_size_signed = if side_is_buy { fill_size } else { -fill_size };

        if pos.size == 0 {
            // Opening new position
            pos.size = fill_size_signed;
            pos.entry_price = fill_price;
        } else if (pos.size > 0) == side_is_buy {
            // Adding to position - update average entry
            // Use i128 for notional calculations to prevent overflow
            let old_notional = pos.size.abs() as i128 * pos.entry_price as i128;
            let add_notional = fill_size as i128 * fill_price as i128;
            let new_size = pos.size.saturating_add(fill_size_signed);
            if new_size.abs() > 0 {
                let new_entry = (old_notional + add_notional) / new_size.abs() as i128;
                pos.entry_price = new_entry.clamp(0, i64::MAX as i128) as i64;
            }
            pos.size = new_size;
        } else {
            // Reducing position - realize PnL
            let close_size = fill_size.min(pos.size.abs());
            let pnl_per_unit = if pos.size > 0 {
                fill_price - pos.entry_price // Long: profit when sell higher
            } else {
                pos.entry_price - fill_price // Short: profit when buy lower
            };
            // Use i128 for PnL calculation to prevent overflow
            let realized_i128 = (close_size as i128 * pnl_per_unit as i128) / 100_000_000;
            let realized = realized_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            pos.realized_pnl = pos.realized_pnl.saturating_add(realized);
            self.balance = self.balance.saturating_add(realized);

            let old_size = pos.size;
            pos.size = pos.size.saturating_add(fill_size_signed);

            // If flipped sides
            if (old_size > 0) != (pos.size > 0) && pos.size != 0 {
                pos.entry_price = fill_price;
            }
        }

        // Clean up zero positions
        if pos.size == 0 {
            pos.entry_price = 0;
        }
    }
}

/// Manages all accounts
const ACCOUNT_SHARD_COUNT: usize = 64;
type AccountShard = HashMap<Address, Arc<Account>>;

#[derive(Clone)]
pub struct AccountManager {
    /// Shards are copy-on-write at the map level.  Cloning a state only
    /// clones these 64 `Arc`s; mutating one account detaches its shard and
    /// then detaches the account record itself if another state still owns it.
    shards: [Arc<AccountShard>; ACCOUNT_SHARD_COUNT],
    /// A per-manager randomized selector prevents attacker-chosen addresses
    /// from concentrating writes in one shard.  `Clone` preserves the seed so
    /// parent/child states always address the same account in the same shard.
    shard_hasher: RandomState,
}

impl AccountManager {
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| Arc::new(HashMap::new())),
            shard_hasher: RandomState::new(),
        }
    }

    /// Create AccountManager from a list of accounts (for recovery from snapshot)
    pub fn from_accounts(accounts: Vec<Account>) -> Self {
        let shard_hasher = RandomState::new();
        let mut shard_maps: [AccountShard; ACCOUNT_SHARD_COUNT] =
            std::array::from_fn(|_| HashMap::new());
        for account in accounts {
            let shard = shard_index(&shard_hasher, &account.address);
            shard_maps[shard].insert(account.address.clone(), Arc::new(account));
        }

        Self {
            shards: shard_maps.map(Arc::new),
            shard_hasher,
        }
    }

    /// Get all accounts (for snapshot)
    pub fn all_accounts(&self) -> Vec<Account> {
        let mut accounts: Vec<_> = self
            .shards
            .iter()
            .flat_map(|shard| shard.values())
            .map(|account| account.as_ref().clone())
            .collect();
        accounts.sort_by(|left, right| left.address.cmp(&right.address));
        accounts
    }

    /// Validate authoritative account and position records without mutation.
    ///
    /// Negative free balance is deliberately allowed: realized losses and
    /// fees can make an account insolvent before liquidation/ADL settles it.
    /// Locked collateral, nonce bookkeeping, and position structure are
    /// stricter runtime invariants and must survive import/replay unchanged.
    pub fn validate_primary_state(
        &self,
        market_configs: &HashMap<Symbol, MarketConfig>,
    ) -> Result<(), AccountError> {
        let mut addresses: Vec<_> = self.shards.iter().flat_map(|shard| shard.keys()).collect();
        addresses.sort();

        for address in addresses {
            let account = self.shards[self.shard_index(address)]
                .get(address)
                .expect("validated account key must resolve in its shard");
            if address.is_empty()
                || address != &account.address
                || account.address != account.address.to_lowercase()
            {
                return Err(AccountError::InvalidAccountAddress);
            }
            if account.locked < 0 {
                return Err(AccountError::NegativeLockedCollateral);
            }
            if account.hyck_balance < 0 {
                return Err(AccountError::NegativeHyckBalance);
            }

            let max_pending = account.nonce.checked_add(MAX_NONCE_GAP).unwrap_or(u64::MAX);
            if account.pending_nonces.iter().any(|pending| {
                *pending == u64::MAX || *pending <= account.nonce || *pending > max_pending
            }) {
                return Err(AccountError::InvalidPendingNonce);
            }

            let mut symbols: Vec<_> = account.positions.keys().collect();
            symbols.sort();
            for symbol in symbols {
                if symbol.is_empty() {
                    return Err(AccountError::InvalidPositionSymbol);
                }
                let position = &account.positions[symbol];
                let config = market_configs
                    .get(symbol)
                    .ok_or(AccountError::UnknownPositionMarket)?;
                let absolute_size = position
                    .size
                    .checked_abs()
                    .ok_or(AccountError::InvalidPositionSize)?;
                if absolute_size > config.max_position_size {
                    return Err(AccountError::PositionSizeLimitExceeded);
                }
                if (position.size == 0 && position.entry_price != 0)
                    || (position.size != 0 && position.entry_price <= 0)
                {
                    return Err(AccountError::InvalidPositionEntryPrice);
                }
            }
        }

        Ok(())
    }

    fn shard_mut(&mut self, address: &str) -> &mut AccountShard {
        let index = self.shard_index(address);
        Arc::make_mut(&mut self.shards[index])
    }

    fn shard_index(&self, address: &str) -> usize {
        shard_index(&self.shard_hasher, address)
    }

    /// Get or create account (no faucet - use `get_or_create_with_faucet` for dev mode)
    pub fn get_or_create(&mut self, address: &str) -> &mut Account {
        let addr_lower = address.to_lowercase();
        let account = self
            .shard_mut(&addr_lower)
            .entry(addr_lower.clone())
            .or_insert_with(|| Arc::new(Account::new(&addr_lower)));
        Arc::make_mut(account)
    }

    /// Get or create account with optional faucet funding (for API layer)
    pub fn get_or_create_with_faucet(&mut self, address: &str, faucet_amount: i64) -> &mut Account {
        let addr_lower = address.to_lowercase();
        let account = self
            .shard_mut(&addr_lower)
            .entry(addr_lower.clone())
            .or_insert_with(|| {
                let mut account = Account::new(&addr_lower);
                if faucet_amount > 0 {
                    account.balance = faucet_amount;
                    tracing::info!(
                        address = %addr_lower,
                        balance = faucet_amount,
                        "New account created with faucet funds"
                    );
                }
                Arc::new(account)
            });
        Arc::make_mut(account)
    }

    /// Get account (read-only)
    pub fn get(&self, address: &str) -> Option<&Account> {
        let address = address.to_lowercase();
        self.shards[self.shard_index(&address)]
            .get(&address)
            .map(|account| account.as_ref())
    }

    /// Deposit funds
    pub fn deposit(&mut self, address: &str, amount: i64) -> Result<(), AccountError> {
        if amount <= 0 {
            return Err(AccountError::InvalidAmount);
        }
        let account = self.get_or_create(address);
        account.balance = account
            .balance
            .checked_add(amount)
            .ok_or(AccountError::BalanceOverflow)?;
        Ok(())
    }

    /// Return a liquid native HYCK balance, or zero for an account that has
    /// not been created yet.
    pub fn hyck_balance(&self, address: &str) -> i64 {
        self.get(address)
            .map(|account| account.hyck_balance)
            .unwrap_or(0)
    }

    /// Withdraw funds
    pub fn withdraw(&mut self, address: &str, amount: i64) -> Result<(), AccountError> {
        if amount <= 0 {
            return Err(AccountError::InvalidAmount);
        }
        let account = self.get_or_create(address);
        if account.balance < amount {
            return Err(AccountError::InsufficientBalance);
        }
        account.balance -= amount;
        Ok(())
    }

    /// Credit liquid native HYCK without touching perp collateral.
    pub fn deposit_hyck(&mut self, address: &str, amount: i64) -> Result<(), AccountError> {
        if amount <= 0 {
            return Err(AccountError::InvalidAmount);
        }
        let address = address.to_lowercase();
        // Preflight before taking a mutable COW path.  Besides keeping the
        // balance unchanged, an overflow on an existing account must not
        // detach its shard or create a new account as a side effect.
        if let Some(account) = self.get(&address) {
            account
                .hyck_balance
                .checked_add(amount)
                .ok_or(AccountError::HyckBalanceOverflow)?;
        }
        self.get_or_create(&address).hyck_balance += amount;
        Ok(())
    }

    /// Debit liquid native HYCK without touching perp collateral.
    pub fn withdraw_hyck(&mut self, address: &str, amount: i64) -> Result<(), AccountError> {
        if amount <= 0 {
            return Err(AccountError::InvalidAmount);
        }
        let address = address.to_lowercase();
        let balance = self
            .get(&address)
            .map(|account| account.hyck_balance)
            .unwrap_or(0);
        if balance < amount {
            return Err(AccountError::InsufficientHyckBalance);
        }
        self.get_or_create(&address).hyck_balance -= amount;
        Ok(())
    }

    /// Atomically transfer liquid native HYCK between two accounts.
    ///
    /// All balance and overflow checks happen before either account is
    /// mutated, so a failed transfer cannot leave a partial debit or credit.
    pub fn transfer_hyck(&mut self, from: &str, to: &str, amount: i64) -> Result<(), AccountError> {
        if amount <= 0 {
            return Err(AccountError::InvalidAmount);
        }

        let from = from.to_lowercase();
        let to = to.to_lowercase();
        let from_balance = self
            .get(&from)
            .map(|account| account.hyck_balance)
            .unwrap_or(0);
        if from_balance < amount {
            return Err(AccountError::InsufficientHyckBalance);
        }
        // A self-transfer has no net state change.  In particular, an
        // account at i64::MAX can still transfer to itself without the
        // transient credit overflowing.
        if from == to {
            return Ok(());
        }
        let to_balance = self
            .get(&to)
            .map(|account| account.hyck_balance)
            .unwrap_or(0);
        to_balance
            .checked_add(amount)
            .ok_or(AccountError::HyckBalanceOverflow)?;

        // The preflight above guarantees both operations succeed. Keep the
        // debit and credit adjacent so this remains one logical state change.
        self.get_or_create(&from).hyck_balance -= amount;
        self.get_or_create(&to).hyck_balance += amount;
        Ok(())
    }

    /// Lock collateral for an order
    pub fn lock_collateral(&mut self, address: &str, amount: i64) -> Result<(), AccountError> {
        let account = self.get_or_create(address);
        if account.balance < amount {
            return Err(AccountError::InsufficientBalance);
        }
        account.balance -= amount;
        account.locked += amount;
        Ok(())
    }

    /// Unlock collateral (order cancelled/filled)
    pub fn unlock_collateral(&mut self, address: &str, amount: i64) {
        let address = address.to_lowercase();
        let shard_index = self.shard_index(&address);
        if !self.shards[shard_index].contains_key(&address) {
            return;
        }
        let shard = Arc::make_mut(&mut self.shards[shard_index]);
        if let Some(account) = shard.get_mut(&address).map(Arc::make_mut) {
            let to_unlock = amount.min(account.locked);
            account.locked -= to_unlock;
            account.balance += to_unlock;
        }
    }

    /// Apply a fill to both maker and taker
    pub fn apply_fill(
        &mut self,
        maker: &str,
        taker: &str,
        symbol: &str,
        taker_is_buyer: bool,
        size: Size,
        price: Price,
        maker_fee: i64,
        taker_fee: i64,
    ) {
        // Calculate fees (in cents) — use i128 to avoid overflow for large orders
        let notional = ((size as i128) * (price as i128) / 100_000_000) as i64;
        let maker_fee_amount = ((notional as i128) * (maker_fee as i128) / 10000) as i64;
        let taker_fee_amount = ((notional as i128) * (taker_fee as i128) / 10000) as i64;

        // Apply to maker (opposite side of taker)
        let maker_account = self.get_or_create(maker);
        maker_account.apply_fill(symbol, !taker_is_buyer, size, price);
        maker_account.balance -= maker_fee_amount;

        // Apply to taker
        let taker_account = self.get_or_create(taker);
        taker_account.apply_fill(symbol, taker_is_buyer, size, price);
        taker_account.balance -= taker_fee_amount;
    }

    /// Get all accounts with positions in a symbol
    pub fn accounts_with_position(&self, symbol: &str) -> Vec<&Account> {
        self.shards
            .iter()
            .flat_map(|shard| shard.values())
            .filter(|a| {
                a.positions
                    .get(symbol)
                    .map(|p| p.size != 0)
                    .unwrap_or(false)
            })
            .map(|account| account.as_ref())
            .collect()
    }

    /// Check if account can open position (has margin)
    pub fn can_open_position(&self, address: &str, notional: i64, leverage: i64) -> bool {
        let required_margin = notional / leverage;
        self.get(address)
            .map(|a| a.balance >= required_margin)
            .unwrap_or(false)
    }

    /// Validate and consume nonce atomically (legacy, no gap tolerance)
    pub fn use_nonce(&mut self, address: &str, nonce: u64) -> Result<(), AccountError> {
        let account = self.get_or_create(address);
        if !account.validate_nonce(nonce) {
            return Err(AccountError::InvalidNonce {
                expected: account.nonce,
                got: nonce,
            });
        }
        account.increment_nonce()
    }

    /// Validate and consume nonce with gap tolerance
    ///
    /// Allows out-of-order transactions within MAX_NONCE_GAP.
    pub fn use_nonce_with_gap(&mut self, address: &str, nonce: u64) -> Result<(), AccountError> {
        let account = self.get_or_create(address);
        match account.validate_nonce_with_gap(nonce) {
            NonceResult::Valid | NonceResult::ValidWithGap => account.use_nonce_with_gap(nonce),
            NonceResult::TooLow { expected } => Err(AccountError::InvalidNonce {
                expected,
                got: nonce,
            }),
            NonceResult::GapTooLarge {
                expected,
                got,
                max_gap,
            } => Err(AccountError::NonceGapTooLarge {
                expected,
                got,
                max_gap,
            }),
            NonceResult::AlreadyUsed => Err(AccountError::NonceAlreadyUsed { nonce }),
            NonceResult::Exhausted => Err(AccountError::NonceOverflow),
        }
    }

    /// Get current nonce for an address
    pub fn get_nonce(&self, address: &str) -> u64 {
        self.get(address).map(|a| a.nonce).unwrap_or(0)
    }

    #[cfg(test)]
    fn account_ptr(&self, address: &str) -> Option<*const Account> {
        let address = address.to_lowercase();
        self.shards[self.shard_index(&address)]
            .get(&address)
            .map(Arc::as_ptr)
    }

    #[cfg(test)]
    fn shard_ptr(&self, address: &str) -> *const AccountShard {
        let address = address.to_lowercase();
        Arc::as_ptr(&self.shards[self.shard_index(&address)])
    }
}

fn shard_index(hasher: &RandomState, address: &str) -> usize {
    let mut state = hasher.build_hasher();
    address.hash(&mut state);
    (state.finish() as usize) & (ACCOUNT_SHARD_COUNT - 1)
}

impl Default for AccountManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Account errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum AccountError {
    #[error("invalid amount")]
    InvalidAmount,
    #[error("account balance overflow")]
    BalanceOverflow,
    #[error("native HYCK balance overflow")]
    HyckBalanceOverflow,
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("insufficient native HYCK balance")]
    InsufficientHyckBalance,
    #[error("account not found")]
    NotFound,
    #[error("account map key/address is empty, non-canonical, or mismatched")]
    InvalidAccountAddress,
    #[error("locked collateral cannot be negative")]
    NegativeLockedCollateral,
    #[error("native HYCK balance cannot be negative")]
    NegativeHyckBalance,
    #[error("pending nonce is not within the valid nonce gap")]
    InvalidPendingNonce,
    #[error("position symbol is empty")]
    InvalidPositionSymbol,
    #[error("position references an unknown market")]
    UnknownPositionMarket,
    #[error("position size cannot be represented safely")]
    InvalidPositionSize,
    #[error("position exceeds its market size limit")]
    PositionSizeLimitExceeded,
    #[error("position size and entry price are inconsistent")]
    InvalidPositionEntryPrice,
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
    #[error("nonce gap too large: expected {expected}, got {got}, max gap is {max_gap}")]
    NonceGapTooLarge {
        expected: u64,
        got: u64,
        max_gap: u64,
    },
    #[error("nonce already used: {nonce}")]
    NonceAlreadyUsed { nonce: u64 },
    #[error("nonce counter exhausted")]
    NonceOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deposit_withdraw() {
        let mut mgr = AccountManager::new();

        mgr.deposit("alice", 10000).unwrap();
        assert_eq!(mgr.get("alice").unwrap().balance, 10000);

        mgr.withdraw("alice", 3000).unwrap();
        assert_eq!(mgr.get("alice").unwrap().balance, 7000);

        assert!(mgr.withdraw("alice", 10000).is_err());
    }

    #[test]
    fn native_hyck_balance_is_separate_from_perp_collateral() {
        let mut mgr = AccountManager::new();
        mgr.deposit("alice", 500).unwrap();
        mgr.deposit_hyck("alice", 1_000_000).unwrap();
        assert_eq!(mgr.get("alice").unwrap().balance, 500);
        assert_eq!(mgr.hyck_balance("alice"), 1_000_000);

        mgr.withdraw_hyck("alice", 250_000).unwrap();
        assert_eq!(mgr.get("alice").unwrap().balance, 500);
        assert_eq!(mgr.hyck_balance("alice"), 750_000);
    }

    #[test]
    fn native_hyck_transfer_failures_are_atomic() {
        let mut mgr = AccountManager::new();
        mgr.deposit_hyck("alice", 10).unwrap();
        mgr.deposit_hyck("bob", i64::MAX).unwrap();

        let insufficient = mgr.transfer_hyck("alice", "carol", 11);
        assert!(matches!(
            insufficient,
            Err(AccountError::InsufficientHyckBalance)
        ));
        assert_eq!(mgr.hyck_balance("alice"), 10);
        assert_eq!(mgr.hyck_balance("carol"), 0);

        let overflow = mgr.transfer_hyck("alice", "bob", 1);
        assert!(matches!(overflow, Err(AccountError::HyckBalanceOverflow)));
        assert_eq!(mgr.hyck_balance("alice"), 10);
        assert_eq!(mgr.hyck_balance("bob"), i64::MAX);

        // Self-transfer is a no-op, including at the largest representable
        // balance where a transient credit would otherwise overflow.
        assert!(mgr.transfer_hyck("bob", "BOB", i64::MAX).is_ok());
        assert_eq!(mgr.hyck_balance("bob"), i64::MAX);
    }

    #[test]
    fn failed_native_hyck_credit_or_debit_does_not_create_an_account() {
        let mut mgr = AccountManager::new();
        assert!(matches!(
            mgr.withdraw_hyck("missing", 1),
            Err(AccountError::InsufficientHyckBalance)
        ));
        assert!(mgr.get("missing").is_none());

        mgr.deposit_hyck("full", i64::MAX).unwrap();
        assert!(matches!(
            mgr.deposit_hyck("full", 1),
            Err(AccountError::HyckBalanceOverflow)
        ));
        assert_eq!(mgr.hyck_balance("full"), i64::MAX);
    }

    #[test]
    fn test_account_manager_clone_isolates_changed_accounts_and_shares_untouched() {
        let mut parent = AccountManager::new();
        let alice = "alice".to_string();
        let bob = (0..ACCOUNT_SHARD_COUNT * 4)
            .map(|index| format!("bob-{index}"))
            .find(|address| parent.shard_index(address) != parent.shard_index(&alice))
            .expect("test fixture must find two distinct account shards");
        parent.deposit(&alice, 100).unwrap();
        parent.deposit(&bob, 200).unwrap();

        let parent_alice = parent.account_ptr(&alice).unwrap();
        let parent_bob = parent.account_ptr(&bob).unwrap();
        let parent_alice_shard = parent.shard_ptr(&alice);
        let parent_bob_shard = parent.shard_ptr(&bob);
        let mut child = parent.clone();
        let mut sibling = parent.clone();

        assert_eq!(child.shard_ptr(&alice), parent_alice_shard);
        assert_eq!(child.shard_ptr(&bob), parent_bob_shard);

        child.deposit(&alice, 10).unwrap();
        sibling.use_nonce(&alice, 0).unwrap();

        assert_eq!(parent.get(&alice).unwrap().balance, 100);
        assert_eq!(child.get(&alice).unwrap().balance, 110);
        assert_eq!(sibling.get_nonce(&alice), 1);
        assert_eq!(parent.get_nonce(&alice), 0);

        // The changed account detaches both the account Arc and its shard;
        // an untouched account keeps both the account Arc and its shard.
        assert_eq!(child.account_ptr(&bob), Some(parent_bob));
        assert_eq!(sibling.account_ptr(&bob), Some(parent_bob));
        assert_eq!(child.shard_ptr(&bob), parent_bob_shard);
        assert_eq!(sibling.shard_ptr(&bob), parent_bob_shard);
        assert_ne!(child.account_ptr(&alice), Some(parent_alice));
        assert_ne!(sibling.account_ptr(&alice), Some(parent_alice));
        assert_ne!(child.account_ptr(&alice), sibling.account_ptr(&alice));
        assert_ne!(child.shard_ptr(&alice), parent_alice_shard);
        assert_ne!(sibling.shard_ptr(&alice), parent_alice_shard);
        assert_ne!(child.shard_ptr(&alice), sibling.shard_ptr(&alice));
    }

    #[test]
    fn test_fill_increases_position() {
        let mut account = Account::new("trader");

        // Buy 1 BTC at $50,000
        account.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        let pos = account.position("BTC-USDT");
        assert_eq!(pos.size, 100_000_000);
        assert_eq!(pos.entry_price, 5_000_000);

        // Buy 1 more BTC at $52,000 -> avg = $51,000
        account.apply_fill("BTC-USDT", true, 100_000_000, 5_200_000);

        let pos = account.position("BTC-USDT");
        assert_eq!(pos.size, 200_000_000);
        assert_eq!(pos.entry_price, 5_100_000);
    }

    #[test]
    fn test_fill_reduces_position() {
        let mut account = Account::new("trader");
        account.balance = 100_000_000; // $1M

        // Long 2 BTC at $50,000
        account.apply_fill("BTC-USDT", true, 200_000_000, 5_000_000);

        // Sell 1 BTC at $51,000 -> realize $1,000 profit
        account.apply_fill("BTC-USDT", false, 100_000_000, 5_100_000);

        let pos = account.position("BTC-USDT");
        assert_eq!(pos.size, 100_000_000); // 1 BTC left
        assert_eq!(pos.realized_pnl, 100_000); // $1,000 realized
    }

    #[test]
    fn test_nonce_validation() {
        let mut mgr = AccountManager::new();

        // First nonce should be 0
        assert_eq!(mgr.get_nonce("alice"), 0);

        // Use nonce 0
        assert!(mgr.use_nonce("alice", 0).is_ok());
        assert_eq!(mgr.get_nonce("alice"), 1);

        // Replay should fail
        assert!(mgr.use_nonce("alice", 0).is_err());

        // Wrong nonce should fail
        assert!(mgr.use_nonce("alice", 5).is_err());

        // Next nonce should work
        assert!(mgr.use_nonce("alice", 1).is_ok());
        assert_eq!(mgr.get_nonce("alice"), 2);
    }

    #[test]
    fn test_is_liquidatable() {
        let mut account = Account::new("trader");
        account.balance = 500_000; // $5,000

        // Long 1 BTC at $50,000
        account.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);

        let mut mark_prices = HashMap::new();

        // At entry price, equity = $5,000, maintenance = $2,500 (5% of $50k)
        mark_prices.insert("BTC-USDT".to_string(), 5_000_000);
        assert!(!account.is_liquidatable(&mark_prices, 500)); // Not liquidatable

        // Price drops to $48,000, unrealized PnL = -$2,000
        // equity = 5000 - 2000 = $3,000, maintenance = $2,400 (5% of $48k)
        mark_prices.insert("BTC-USDT".to_string(), 4_800_000);
        assert!(!account.is_liquidatable(&mark_prices, 500)); // Still OK

        // Price drops to $46,000, unrealized PnL = -$4,000
        // equity = 5000 - 4000 = $1,000, maintenance = $2,300 (5% of $46k)
        mark_prices.insert("BTC-USDT".to_string(), 4_600_000);
        assert!(account.is_liquidatable(&mark_prices, 500)); // Liquidatable!
    }

    #[test]
    fn test_nonce_gap_validation() {
        let mut account = Account::new("trader");

        // Exact nonce is valid
        assert_eq!(account.validate_nonce_with_gap(0), NonceResult::Valid);

        // Too low is invalid
        account.nonce = 5;
        assert_eq!(
            account.validate_nonce_with_gap(3),
            NonceResult::TooLow { expected: 5 }
        );

        // Within gap is valid with gap flag
        assert_eq!(
            account.validate_nonce_with_gap(7),
            NonceResult::ValidWithGap
        );

        // At max gap is valid
        assert_eq!(
            account.validate_nonce_with_gap(15),
            NonceResult::ValidWithGap
        );

        // Beyond max gap is invalid
        assert_eq!(
            account.validate_nonce_with_gap(16),
            NonceResult::GapTooLarge {
                expected: 5,
                got: 16,
                max_gap: MAX_NONCE_GAP
            }
        );
    }

    #[test]
    fn test_nonce_gap_usage() {
        let mut account = Account::new("trader");

        // Use nonce 0 (exact match)
        account.use_nonce_with_gap(0).unwrap();
        assert_eq!(account.nonce, 1);
        assert!(account.pending_nonces.is_empty());

        // Use nonce 3 (gap of 2)
        account.use_nonce_with_gap(3).unwrap();
        assert_eq!(account.nonce, 1); // Not incremented yet
        assert!(account.pending_nonces.contains(&3));

        // Use nonce 2 (gap of 1)
        account.use_nonce_with_gap(2).unwrap();
        assert_eq!(account.nonce, 1);
        assert!(account.pending_nonces.contains(&2));
        assert!(account.pending_nonces.contains(&3));

        // Use nonce 1 (fills the gap)
        account.use_nonce_with_gap(1).unwrap();
        assert_eq!(account.nonce, 4); // Incremented past all pending
        assert!(account.pending_nonces.is_empty());
    }

    #[test]
    fn test_nonce_already_used() {
        let mut account = Account::new("trader");
        account.nonce = 5;

        // Use nonce 7 (gap)
        account.use_nonce_with_gap(7).unwrap();
        assert!(account.pending_nonces.contains(&7));

        // Try to use 7 again - should be AlreadyUsed
        assert_eq!(account.validate_nonce_with_gap(7), NonceResult::AlreadyUsed);
    }

    #[test]
    fn test_position_flip_detection() {
        let mut account = Account::new("trader");
        account.balance = 100_000_000; // $1M

        // Open long 1 BTC at $50,000
        account.apply_fill("BTC-USDT", true, 100_000_000, 5_000_000);
        let pos = account.position("BTC-USDT");
        assert_eq!(pos.size, 100_000_000);
        assert_eq!(pos.entry_price, 5_000_000);

        // Sell 2 BTC at $51,000 -> flips from long to short
        // Should realize PnL on the 1 BTC close, then entry_price = fill_price for new short
        account.apply_fill("BTC-USDT", false, 200_000_000, 5_100_000);
        let pos = account.position("BTC-USDT");
        assert_eq!(pos.size, -100_000_000); // Now short 1 BTC
        assert_eq!(pos.entry_price, 5_100_000); // Entry price updated to flip price
    }

    #[test]
    fn test_account_manager_nonce_with_gap() {
        let mut mgr = AccountManager::new();

        // Use nonces out of order
        assert!(mgr.use_nonce_with_gap("alice", 0).is_ok());
        assert!(mgr.use_nonce_with_gap("alice", 2).is_ok()); // Gap
        assert!(mgr.use_nonce_with_gap("alice", 1).is_ok()); // Fills gap

        // Now nonce should be 3
        assert_eq!(mgr.get_nonce("alice"), 3);

        // Gap too large should fail
        assert!(matches!(
            mgr.use_nonce_with_gap("alice", 20),
            Err(AccountError::NonceGapTooLarge { .. })
        ));

        // Already used should fail
        mgr.use_nonce_with_gap("alice", 5).unwrap(); // Add pending
        assert!(matches!(
            mgr.use_nonce_with_gap("alice", 5),
            Err(AccountError::NonceAlreadyUsed { .. })
        ));
    }

    #[test]
    fn nonce_counter_overflow_fails_closed_without_mutation() {
        let mut account = Account::new("trader");

        assert!(matches!(
            account.use_nonce_with_gap(u64::MAX),
            Err(AccountError::NonceGapTooLarge { .. })
        ));
        assert_eq!(account.nonce, 0);
        assert!(account.pending_nonces.is_empty());

        account.nonce = u64::MAX;

        assert_eq!(
            account.validate_nonce_with_gap(u64::MAX),
            NonceResult::Exhausted
        );
        assert!(matches!(
            account.use_nonce_with_gap(u64::MAX),
            Err(AccountError::NonceOverflow)
        ));
        assert_eq!(account.nonce, u64::MAX);
        assert!(account.pending_nonces.is_empty());

        let mut account = Account::new("trader");
        account.nonce = u64::MAX - 1;
        account.pending_nonces.insert(u64::MAX);
        assert_eq!(
            account.validate_nonce_with_gap(u64::MAX - 1),
            NonceResult::Exhausted
        );
        assert!(matches!(
            account.use_nonce_with_gap(u64::MAX - 1),
            Err(AccountError::NonceOverflow)
        ));
        assert_eq!(account.nonce, u64::MAX - 1);
        assert!(account.pending_nonces.contains(&u64::MAX));
    }

    #[test]
    fn primary_validation_accepts_insolvency_but_checks_nonce_and_position_structure() {
        let mut manager = AccountManager::new();
        let account = manager.get_or_create("alice");
        account.balance = -1;
        account.locked = 10;
        account.nonce = 2;
        account.pending_nonces.insert(4);
        account.apply_fill("BTC-USDT", true, 100, 5_000_000);

        let configs = HashMap::from([("BTC-USDT".to_string(), MarketConfig::default())]);
        manager.validate_primary_state(&configs).unwrap();

        manager.get_or_create("alice").pending_nonces.insert(2);
        assert!(matches!(
            manager.validate_primary_state(&configs),
            Err(AccountError::InvalidPendingNonce)
        ));
        manager.get_or_create("alice").pending_nonces.remove(&2);

        manager
            .get_or_create("alice")
            .positions
            .get_mut("BTC-USDT")
            .unwrap()
            .entry_price = 0;
        assert!(matches!(
            manager.validate_primary_state(&configs),
            Err(AccountError::InvalidPositionEntryPrice)
        ));
    }
}
