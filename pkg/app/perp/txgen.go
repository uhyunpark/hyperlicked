package perp

import (
	"fmt"
	"math/rand"
	"time"
)

// TxGenerator creates random trading transactions for load testing
type TxGenerator struct {
	accounts []string // List of simulated trader addresses
	symbols  []string // List of tradeable markets
	orderID  int      // Counter for unique order IDs
	rng      *rand.Rand
}

// NewTxGenerator creates a new transaction generator
func NewTxGenerator(numAccounts int, symbols []string) *TxGenerator {
	accounts := make([]string, numAccounts)
	for i := 0; i < numAccounts; i++ {
		accounts[i] = fmt.Sprintf("trader_%d", i+1)
	}

	return &TxGenerator{
		accounts: accounts,
		symbols:  symbols,
		orderID:  1,
		rng:      rand.New(rand.NewSource(time.Now().UnixNano())),
	}
}

// GenerateOrder creates a random order transaction
func (g *TxGenerator) GenerateOrder() []byte {
	account := g.accounts[g.rng.Intn(len(g.accounts))]
	symbol := g.symbols[g.rng.Intn(len(g.symbols))]

	// Random order type: 70% GTC, 20% IOC, 10% ALO
	var orderType string
	r := g.rng.Intn(100)
	if r < 70 {
		orderType = "GTC"
	} else if r < 90 {
		orderType = "IOC"
	} else {
		orderType = "ALO"
	}

	// Random side: 50% BUY, 50% SELL
	side := "BUY"
	if g.rng.Intn(2) == 1 {
		side = "SELL"
	}

	// Random price around $50,000 BTC (±5%)
	// BTC-USDT: tick=1 cent, lot=100 (0.01 BTC)
	basePrice := 50000
	priceVariation := g.rng.Intn(5000) - 2500 // ±2500 = ±5%
	price := basePrice + priceVariation
	if price < 1000 {
		price = 1000
	}

	// Random quantity: 1 to 100 lots (0.01 to 1.0 BTC)
	// MinNotional = 10000 cents = $100
	// At $50k/BTC: need at least 100 lots × 50000 = 5,000,000 > 10000 ✓
	qty := g.rng.Intn(100) + 1

	// Generate unique order ID
	orderID := fmt.Sprintf("%s_o%d", account, g.orderID)
	g.orderID++

	// Format: O:TYPE:SYMBOL:SIDE:price=X:qty=Y:id=Z
	tx := fmt.Sprintf("O:%s:%s:%s:price=%d:qty=%d:id=%s",
		orderType, symbol, side, price, qty, orderID)

	return []byte(tx)
}

// GenerateCancel creates a random cancel transaction
// Note: In real scenario, this should cancel existing orders
// For load testing, we just generate cancel commands
func (g *TxGenerator) GenerateCancel() []byte {
	account := g.accounts[g.rng.Intn(len(g.accounts))]
	symbol := g.symbols[g.rng.Intn(len(g.symbols))]

	// Cancel a recent order (last 100 orders)
	orderNum := g.orderID - g.rng.Intn(100)
	if orderNum < 1 {
		orderNum = 1
	}
	orderID := fmt.Sprintf("%s_o%d", account, orderNum)

	// Format: C:SYMBOL:ORDER_ID
	tx := fmt.Sprintf("C:%s:%s", symbol, orderID)
	return []byte(tx)
}

// GenerateMix creates a random transaction (90% orders, 10% cancels)
func (g *TxGenerator) GenerateMix() []byte {
	if g.rng.Intn(100) < 90 {
		return g.GenerateOrder()
	}
	return g.GenerateCancel()
}

// GenerateBatch creates multiple random transactions
func (g *TxGenerator) GenerateBatch(count int) [][]byte {
	batch := make([][]byte, count)
	for i := 0; i < count; i++ {
		batch[i] = g.GenerateMix()
	}
	return batch
}

// Stats for load testing analysis
type TxGenStats struct {
	TotalOrders   int
	TotalCancels  int
	OrdersPerSec  float64
	CancelsPerSec float64
}

// GetStats returns current generation statistics
func (g *TxGenerator) GetStats(elapsed time.Duration) TxGenStats {
	seconds := elapsed.Seconds()
	if seconds == 0 {
		seconds = 1
	}

	return TxGenStats{
		TotalOrders:   g.orderID - 1,
		OrdersPerSec:  float64(g.orderID-1) / seconds,
	}
}
