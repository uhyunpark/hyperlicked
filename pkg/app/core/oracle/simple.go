package oracle

import (
	"sync"
)

// SimpleOracle provides mark prices for liquidation checks
// Phase 1: Uses orderbook mid-price (simple, deterministic)
// Phase 2: Will use multi-validator external price feeds (Binance, Coinbase, etc.)
type SimpleOracle struct {
	mu     sync.RWMutex
	prices map[string]int64 // symbol -> price (in cents)
}

// NewSimpleOracle creates a new oracle with empty prices
func NewSimpleOracle() *SimpleOracle {
	return &SimpleOracle{
		prices: make(map[string]int64),
	}
}

// SetPrice updates the mark price for a symbol
// Called by App after each block (uses orderbook mid-price)
func (o *SimpleOracle) SetPrice(symbol string, price int64) {
	o.mu.Lock()
	defer o.mu.Unlock()
	o.prices[symbol] = price
}

// GetMarkPrice returns the mark price for a single symbol
// Returns 0 if price is not available
func (o *SimpleOracle) GetMarkPrice(symbol string) int64 {
	o.mu.RLock()
	defer o.mu.RUnlock()
	return o.prices[symbol]
}

// GetMarkPrices returns all mark prices (thread-safe copy)
// Used by liquidation engine to compute unrealized PnL
func (o *SimpleOracle) GetMarkPrices() map[string]int64 {
	o.mu.RLock()
	defer o.mu.RUnlock()

	// Return a copy to avoid race conditions
	prices := make(map[string]int64, len(o.prices))
	for symbol, price := range o.prices {
		prices[symbol] = price
	}
	return prices
}

// ListSymbols returns all symbols with available prices
func (o *SimpleOracle) ListSymbols() []string {
	o.mu.RLock()
	defer o.mu.RUnlock()

	symbols := make([]string, 0, len(o.prices))
	for symbol := range o.prices {
		symbols = append(symbols, symbol)
	}
	return symbols
}
