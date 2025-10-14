package core

import (
	"fmt"
	"sync"

	"github.com/ethereum/go-ethereum/common"
)

// AccountManager manages all user accounts in a thread-safe manner
// Handles deposits, withdrawals, margin locking/unlocking, and position updates
type AccountManager struct {
	mu       sync.RWMutex
	accounts map[common.Address]*Account // address -> account
}

// NewAccountManager creates an empty account manager
func NewAccountManager() *AccountManager {
	return &AccountManager{
		accounts: make(map[common.Address]*Account),
	}
}

// GetAccount retrieves an account by address
// Creates a new account with zero balance if it doesn't exist
func (am *AccountManager) GetAccount(addr common.Address) *Account {
	am.mu.Lock()
	defer am.mu.Unlock()

	acc, exists := am.accounts[addr]
	if !exists {
		acc = NewAccount(addr)
		am.accounts[addr] = acc
	}
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

	acc, exists := am.accounts[addr]
	if !exists {
		acc = NewAccount(addr)
		am.accounts[addr] = acc
	}

	acc.USDCBalance += amount
	return nil
}

// Withdraw removes USDC from an account (to bridge)
// Returns error if insufficient available balance
func (am *AccountManager) Withdraw(addr common.Address, amount int64) error {
	if amount <= 0 {
		return fmt.Errorf("withdraw amount must be positive: %d", amount)
	}

	am.mu.Lock()
	defer am.mu.Unlock()

	acc, exists := am.accounts[addr]
	if !exists {
		return fmt.Errorf("account not found: %s", addr.Hex())
	}

	available := acc.AvailableBalance()
	if available < amount {
		return fmt.Errorf("insufficient balance: have %d, need %d (locked: %d)", available, amount, acc.LockedCollateral)
	}

	acc.USDCBalance -= amount
	return nil
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
