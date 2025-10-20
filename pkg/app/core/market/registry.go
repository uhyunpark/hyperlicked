package market

import (
	"fmt"
	"sync"
)

// MarketRegistry manages multiple markets in a thread-safe manner
// Supports registration, lookup, and status updates for all trading markets
type MarketRegistry struct {
	mu      sync.RWMutex
	markets map[string]*Market // symbol -> market
}

// NewMarketRegistry creates an empty market registry
func NewMarketRegistry() *MarketRegistry {
	return &MarketRegistry{
		markets: make(map[string]*Market),
	}
}

// RegisterMarket adds a new market to the registry
// Returns error if market with same symbol already exists
func (mr *MarketRegistry) RegisterMarket(m *Market) error {
	if m == nil {
		return fmt.Errorf("cannot register nil market")
	}

	mr.mu.Lock()
	defer mr.mu.Unlock()

	if _, exists := mr.markets[m.Symbol]; exists {
		return fmt.Errorf("market %s already registered", m.Symbol)
	}

	mr.markets[m.Symbol] = m
	return nil
}

// GetMarket retrieves a market by symbol
// Returns error if market not found
func (mr *MarketRegistry) GetMarket(symbol string) (*Market, error) {
	mr.mu.RLock()
	defer mr.mu.RUnlock()

	m, exists := mr.markets[symbol]
	if !exists {
		return nil, fmt.Errorf("market %s not found", symbol)
	}

	return m, nil
}

// ListMarkets returns all registered markets
// Returns a copy of the slice to avoid concurrent modification
func (mr *MarketRegistry) ListMarkets() []*Market {
	mr.mu.RLock()
	defer mr.mu.RUnlock()

	markets := make([]*Market, 0, len(mr.markets))
	for _, m := range mr.markets {
		markets = append(markets, m)
	}

	return markets
}

// ListActiveMarkets returns only markets with Active status
func (mr *MarketRegistry) ListActiveMarkets() []*Market {
	mr.mu.RLock()
	defer mr.mu.RUnlock()

	markets := make([]*Market, 0)
	for _, m := range mr.markets {
		if m.Status == Active {
			markets = append(markets, m)
		}
	}

	return markets
}

// UpdateMarketStatus changes the trading status of a market
// Used for emergency pausing, settling, etc.
func (mr *MarketRegistry) UpdateMarketStatus(symbol string, status MarketStatus) error {
	mr.mu.Lock()
	defer mr.mu.Unlock()

	m, exists := mr.markets[symbol]
	if !exists {
		return fmt.Errorf("market %s not found", symbol)
	}

	// Validate status transition
	if err := mr.validateStatusTransition(m.Status, status); err != nil {
		return err
	}

	m.Status = status
	return nil
}

// validateStatusTransition checks if status change is valid
func (mr *MarketRegistry) validateStatusTransition(from, to MarketStatus) error {
	// Active → Paused: allowed (emergency halt)
	// Paused → Active: allowed (resume trading)
	// Active/Paused → Settling: allowed (start settlement)
	// Settling → Settled: allowed (finalize)
	// Settled → *: not allowed (terminal state)

	if from == Settled {
		return fmt.Errorf("cannot change status from Settled (terminal state)")
	}

	// All other transitions are allowed for now
	return nil
}

// RemoveMarket removes a market from the registry
// Should only be used for testing or admin operations
// Returns error if market has active positions (safety check)
func (mr *MarketRegistry) RemoveMarket(symbol string) error {
	mr.mu.Lock()
	defer mr.mu.Unlock()

	m, exists := mr.markets[symbol]
	if !exists {
		return fmt.Errorf("market %s not found", symbol)
	}

	// Safety check: only allow removal of settled markets
	if m.Status != Settled {
		return fmt.Errorf("cannot remove market %s with status %s (must be Settled)", symbol, m.Status)
	}

	delete(mr.markets, symbol)
	return nil
}

// Count returns the total number of registered markets
func (mr *MarketRegistry) Count() int {
	mr.mu.RLock()
	defer mr.mu.RUnlock()
	return len(mr.markets)
}

// Exists checks if a market is registered
func (mr *MarketRegistry) Exists(symbol string) bool {
	mr.mu.RLock()
	defer mr.mu.RUnlock()
	_, exists := mr.markets[symbol]
	return exists
}
