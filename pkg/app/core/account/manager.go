package account

import (
	"fmt"
	"sync"

	"github.com/ethereum/go-ethereum/common"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/market"
)

// AccountManager manages all user accounts in a thread-safe manner
// Handles deposits, withdrawals, margin locking/unlocking, and position updates
// Uses in-memory cache + Pebble persistence for durability
type AccountManager struct {
	mu       sync.RWMutex
	accounts map[common.Address]*Account // address -> account (in-memory cache)
	store    *Store                      // Pebble persistence layer
}

// NewAccountManager creates an account manager with Pebble persistence
func NewAccountManager(dbPath string) (*AccountManager, error) {
	store, err := NewStore(dbPath)
	if err != nil {
		return nil, fmt.Errorf("failed to create store: %w", err)
	}

	return &AccountManager{
		accounts: make(map[common.Address]*Account),
		store:    store,
	}, nil
}

// Close closes the underlying Pebble database
func (am *AccountManager) Close() error {
	return am.store.Close()
}

// GetAccount retrieves an account by address
// Creates a new account with zero balance if it doesn't exist
// Loads from Pebble if not in cache
func (am *AccountManager) GetAccount(addr common.Address) *Account {
	am.mu.Lock()
	defer am.mu.Unlock()

	// Check cache first
	acc, exists := am.accounts[addr]
	if exists {
		return acc
	}

	// Try loading from Pebble
	acc, err := am.store.LoadAccount(addr)
	if err != nil {
		// Log error but don't fail - create new account
		fmt.Printf("[account] failed to load account %s: %v\n", addr.Hex(), err)
	}

	if acc == nil {
		// Account doesn't exist - create new
		acc = NewAccount(addr)
	}

	// Cache it
	am.accounts[addr] = acc
	return acc
}

// GetAccountReadOnly retrieves an account without creating it
// Returns nil if account doesn't exist (use for queries only)
func (am *AccountManager) GetAccountReadOnly(addr common.Address) *Account {
	am.mu.RLock()
	defer am.mu.RUnlock()
	return am.accounts[addr]
}

// Deposit adds USDC to an account (from bridge)
// Creates account if it doesn't exist
func (am *AccountManager) Deposit(addr common.Address, amount int64) error {
	if amount <= 0 {
		return fmt.Errorf("deposit amount must be positive: %d", amount)
	}

	am.mu.Lock()
	defer am.mu.Unlock()

	acc := am.getAccountLocked(addr)
	acc.USDCBalance += amount

	// Persist to Pebble
	return am.store.SaveAccount(acc)
}

// getAccountLocked is an internal helper that gets account (assumes lock is held)
func (am *AccountManager) getAccountLocked(addr common.Address) *Account {
	acc, exists := am.accounts[addr]
	if exists {
		return acc
	}

	// Try loading from Pebble
	acc, err := am.store.LoadAccount(addr)
	if err != nil {
		fmt.Printf("[account] failed to load account %s: %v\n", addr.Hex(), err)
	}

	if acc == nil {
		acc = NewAccount(addr)
	}

	am.accounts[addr] = acc
	return acc
}

// Withdraw removes USDC from an account (to bridge)
// Returns error if insufficient available balance
func (am *AccountManager) Withdraw(addr common.Address, amount int64) error {
	if amount <= 0 {
		return fmt.Errorf("withdraw amount must be positive: %d", amount)
	}

	am.mu.Lock()
	defer am.mu.Unlock()

	acc := am.getAccountLocked(addr)

	available := acc.AvailableBalance()
	if available < amount {
		return fmt.Errorf("insufficient balance: have %d, need %d (locked: %d)", available, amount, acc.LockedCollateral)
	}

	acc.USDCBalance -= amount

	// Persist to Pebble
	return am.store.SaveAccount(acc)
}

// LockCollateral locks collateral for an order or position
// Returns error if insufficient available balance
func (am *AccountManager) LockCollateral(addr common.Address, amount int64) error {
	if amount < 0 {
		return fmt.Errorf("lock amount cannot be negative: %d", amount)
	}
	if amount == 0 {
		return nil // No-op for zero amount
	}

	am.mu.Lock()
	defer am.mu.Unlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return fmt.Errorf("account not found: %s", addr.Hex())
	}

	available := acc.AvailableBalance()
	if available < amount {
		return fmt.Errorf("insufficient balance to lock: have %d, need %d", available, amount)
	}

	acc.LockedCollateral += amount
	return nil
}

// UnlockCollateral releases locked collateral
// Used when orders are cancelled or positions closed
func (am *AccountManager) UnlockCollateral(addr common.Address, amount int64) error {
	if amount < 0 {
		return fmt.Errorf("unlock amount cannot be negative: %d", amount)
	}
	if amount == 0 {
		return nil // No-op for zero amount
	}

	am.mu.Lock()
	defer am.mu.Unlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return fmt.Errorf("account not found: %s", addr.Hex())
	}

	if acc.LockedCollateral < amount {
		return fmt.Errorf("cannot unlock more than locked: locked=%d, unlock=%d", acc.LockedCollateral, amount)
	}

	acc.LockedCollateral -= amount
	return nil
}

// GetAvailableBalance returns balance available for new orders
// Thread-safe read
func (am *AccountManager) GetAvailableBalance(addr common.Address) int64 {
	am.mu.RLock()
	defer am.mu.RUnlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return 0
	}
	return acc.AvailableBalance()
}

// UpdatePosition updates an account's position after a fill
// Creates position if it doesn't exist
func (am *AccountManager) UpdatePosition(addr common.Address, symbol string, sizeDelta int64, price int64, marginDelta int64) error {
	am.mu.Lock()
	defer am.mu.Unlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return fmt.Errorf("account not found: %s", addr.Hex())
	}

	pos := acc.GetPosition(symbol)
	if pos == nil {
		// Create new position
		pos = &Position{
			Symbol:     symbol,
			Size:       0,
			EntryPrice: 0,
			Margin:     0,
		}
		acc.Positions[symbol] = pos
	}

	oldSize := pos.Size
	newSize := oldSize + sizeDelta

	// Update entry price (VWAP)
	if newSize == 0 {
		// Position closed - calculate realized PnL
		// Realized PnL = (exitPrice - entryPrice) × size
		// For longs (size > 0): profit when price increases
		// For shorts (size < 0): profit when price decreases (already handled by negative size)
		realizedPnL := (price - pos.EntryPrice) * oldSize
		acc.RealizedPnL += realizedPnL

		pos.Size = 0
		pos.EntryPrice = 0
		pos.Margin = 0
	} else if (oldSize >= 0 && newSize >= 0) || (oldSize <= 0 && newSize <= 0) {
		// Same direction: update VWAP
		if oldSize == 0 {
			pos.EntryPrice = price
		} else {
			// Weighted average
			absOldSize := oldSize
			if absOldSize < 0 {
				absOldSize = -absOldSize
			}
			absSizeDelta := sizeDelta
			if absSizeDelta < 0 {
				absSizeDelta = -absSizeDelta
			}
			absNewSize := newSize
			if absNewSize < 0 {
				absNewSize = -absNewSize
			}

			pos.EntryPrice = (pos.EntryPrice*absOldSize + price*absSizeDelta) / absNewSize
		}
		pos.Size = newSize
		pos.Margin += marginDelta
	} else {
		// Opposite direction: reducing/flipping position
		// Calculate realized PnL for closed portion
		absOldSize := oldSize
		if absOldSize < 0 {
			absOldSize = -absOldSize
		}
		absSizeDelta := sizeDelta
		if absSizeDelta < 0 {
			absSizeDelta = -absSizeDelta
		}

		closedSize := absOldSize
		if absSizeDelta < absOldSize {
			closedSize = absSizeDelta
		}

		// Realized PnL = (exitPrice - entryPrice) � closedSize
		// For shorts, flip the sign
		realizedPnL := (price - pos.EntryPrice) * closedSize
		if oldSize < 0 {
			realizedPnL = -realizedPnL
		}
		acc.RealizedPnL += realizedPnL

		// Update position
		pos.Size = newSize
		if newSize == 0 {
			pos.EntryPrice = 0
			pos.Margin = 0
		} else if (oldSize > 0 && newSize < 0) || (oldSize < 0 && newSize > 0) {
			// Position flipped: new entry price is fill price
			pos.EntryPrice = price
			pos.Margin = marginDelta
		} else {
			// Position reduced but not flipped
			pos.Margin += marginDelta
		}
	}

	return nil
}

// ApplyFees deducts taker fee or credits maker rebate
// Fees are in USDC cents (same as balance)
func (am *AccountManager) ApplyFees(addr common.Address, feeDelta int64) error {
	am.mu.Lock()
	defer am.mu.Unlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return fmt.Errorf("account not found: %s", addr.Hex())
	}

	acc.USDCBalance += feeDelta // Negative for fees, positive for rebates

	if feeDelta < 0 {
		acc.TotalFeesPaid += -feeDelta
	} else {
		acc.TotalFeesEarned += feeDelta
	}

	return nil
}

// RecordTrade increments trade statistics
func (am *AccountManager) RecordTrade(addr common.Address, volume int64) error {
	am.mu.Lock()
	defer am.mu.Unlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return fmt.Errorf("account not found: %s", addr.Hex())
	}

	acc.TradeCount++
	acc.TotalVolume += volume
	return nil
}

// ListAccounts returns all registered accounts
// Returns a snapshot copy to avoid holding the lock
func (am *AccountManager) ListAccounts() []*Account {
	am.mu.RLock()
	defer am.mu.RUnlock()

	accounts := make([]*Account, 0, len(am.accounts))
	for _, acc := range am.accounts {
		accounts = append(accounts, acc)
	}
	return accounts
}

// Count returns the total number of accounts
func (am *AccountManager) Count() int {
	am.mu.RLock()
	defer am.mu.RUnlock()
	return len(am.accounts)
}

// ValidateAccount checks account invariants
func (am *AccountManager) ValidateAccount(addr common.Address) error {
	am.mu.RLock()
	defer am.mu.RUnlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return fmt.Errorf("account not found: %s", addr.Hex())
	}

	return acc.Validate()
}

// CheckMarginRequirement verifies if account has sufficient margin for a new position
// Returns error if:
//   - Position would exceed max leverage
//   - Insufficient available balance for required margin
//   - New position would exceed max position size
//
// Formula: Required margin = Notional × InitialMarginBps / 10000
func (am *AccountManager) CheckMarginRequirement(addr common.Address, mkt *market.Market, price, sizeDelta int64) error {
	am.mu.RLock()
	defer am.mu.RUnlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return fmt.Errorf("account not found: %s", addr.Hex())
	}

	// Calculate required initial margin for this order
	requiredMargin := mkt.RequiredInitialMargin(price, absInt64(sizeDelta))

	// Check 1: Sufficient available balance for margin
	available := acc.AvailableBalance()
	if available < requiredMargin {
		return fmt.Errorf("insufficient margin: have %d, need %d", available, requiredMargin)
	}

	// Check 2: New position size doesn't exceed max
	pos := acc.GetPosition(mkt.Symbol)
	currentSize := int64(0)
	if pos != nil {
		currentSize = pos.Size
	}
	newSize := currentSize + sizeDelta
	if absInt64(newSize) > mkt.MaxPosition {
		return fmt.Errorf("position would exceed max size: new=%d, max=%d", absInt64(newSize), mkt.MaxPosition)
	}

	// Check 3: Total account leverage doesn't exceed max
	// Compute total notional value of all positions + new position
	totalNotional := absInt64(newSize) * price // New position notional
	for symbol, p := range acc.Positions {
		if symbol == mkt.Symbol {
			continue // Already counted above
		}
		// For existing positions, use entry price (we don't have mark price here yet)
		// TODO: Use mark price from oracle when available
		totalNotional += absInt64(p.Size) * p.EntryPrice
	}

	// Total margin available = current balance - used margin + this new margin
	totalMarginAvailable := acc.USDCBalance
	effectiveLeverage := int64(0)
	if totalMarginAvailable > 0 {
		effectiveLeverage = totalNotional / totalMarginAvailable
	}

	if effectiveLeverage > mkt.MaxLeverage {
		return fmt.Errorf("leverage %dx exceeds max %dx", effectiveLeverage, mkt.MaxLeverage)
	}

	return nil
}

// CheckLiquidation checks if an account should be liquidated
// Returns true if account equity falls below maintenance margin requirement
//
// Liquidation occurs when: Total Equity < Total Maintenance Margin
// Where:
//   - Total Equity = Balance + Unrealized PnL (mark-to-market)
//   - Total Maintenance Margin = sum of (Position Notional × MaintenanceMarginBps) for all positions
//
// Parameters:
//   - addr: Account address to check
//   - markets: Map of symbol → Market (for maintenance margin params)
//   - markPrices: Map of symbol → current mark price (for computing unrealized PnL)
//
// Returns: (shouldLiquidate, accountEquity, requiredMargin, error)
func (am *AccountManager) CheckLiquidation(addr common.Address, markets map[string]*market.Market, markPrices map[string]int64) (bool, int64, int64, error) {
	am.mu.RLock()
	defer am.mu.RUnlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return false, 0, 0, fmt.Errorf("account not found: %s", addr.Hex())
	}

	// No positions = no liquidation risk
	if len(acc.Positions) == 0 {
		return false, acc.USDCBalance, 0, nil
	}

	// Calculate total equity (balance + unrealized PnL)
	totalEquity := acc.TotalEquity(markPrices)

	// Calculate total maintenance margin requirement
	totalMaintenanceMargin := int64(0)
	for symbol, pos := range acc.Positions {
		if pos.Size == 0 {
			continue
		}

		mkt, ok := markets[symbol]
		if !ok {
			// No market data = can't compute margin, skip
			continue
		}

		markPrice, ok := markPrices[symbol]
		if !ok {
			// No mark price = can't compute PnL, use entry price as fallback
			markPrice = pos.EntryPrice
		}

		// Maintenance margin = Notional × MaintenanceMarginBps / 10000
		notional := absInt64(pos.Size) * markPrice
		maintenanceMargin := (notional * mkt.MaintenanceMarginBps) / 10000
		totalMaintenanceMargin += maintenanceMargin
	}

	// Liquidate if equity < maintenance margin
	shouldLiquidate := totalEquity < totalMaintenanceMargin

	return shouldLiquidate, totalEquity, totalMaintenanceMargin, nil
}

// Liquidate closes all positions for an underwater account
// Called by liquidation engine when CheckLiquidation returns true
//
// Process:
//  1. Close all positions at mark price (simulates market order liquidation)
//  2. Realize all PnL
//  3. Unlock all position margins
//  4. If remaining balance < 0, transfer deficit to insurance fund
//
// Returns: (finalBalance, deficit, error)
//   - finalBalance: Account balance after liquidation
//   - deficit: Amount owed to insurance fund (0 if account solvent)
func (am *AccountManager) Liquidate(addr common.Address, markets map[string]*market.Market, markPrices map[string]int64) (int64, int64, error) {
	am.mu.Lock()
	defer am.mu.Unlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return 0, 0, fmt.Errorf("account not found: %s", addr.Hex())
	}

	if len(acc.Positions) == 0 {
		return acc.USDCBalance, 0, nil
	}

	// Close all positions at mark price
	for symbol, pos := range acc.Positions {
		if pos.Size == 0 {
			continue
		}

		markPrice, ok := markPrices[symbol]
		if !ok {
			// No mark price, use entry price as fallback
			markPrice = pos.EntryPrice
		}

		// Realize PnL: (markPrice - entryPrice) × size
		realizedPnL := (markPrice - pos.EntryPrice) * pos.Size
		acc.RealizedPnL += realizedPnL
		acc.USDCBalance += realizedPnL // Apply PnL to balance

		// Unlock position margin
		acc.LockedCollateral -= pos.Margin

		// Close position
		pos.Size = 0
		pos.EntryPrice = 0
		pos.Margin = 0
	}

	deficit := int64(0)
	if acc.USDCBalance < 0 {
		deficit = -acc.USDCBalance
		acc.USDCBalance = 0 // Bankrupt, zero out balance
	}

	finalBalance := acc.USDCBalance
	return finalBalance, deficit, nil
}

// absInt64 returns absolute value of int64
func absInt64(x int64) int64 {
	if x < 0 {
		return -x
	}
	return x
}
