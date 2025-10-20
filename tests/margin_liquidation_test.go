package tests

import (
	"testing"

	"github.com/ethereum/go-ethereum/common"
	"github.com/uhyunpark/hyperlicked/pkg/app/core"
)

var (
	trader1 = common.HexToAddress("0x1111111111111111111111111111111111111111")
	trader2 = common.HexToAddress("0x2222222222222222222222222222222222222222")
)

// TestMarginRequirementCheck tests pre-trade margin validation
func TestMarginRequirementCheck(t *testing.T) {
	am := newTestAccountManager(t)
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Deposit $1000 (100,000 cents)
	am.Deposit(trader1, 100000)

	// Test 1: Sufficient margin for small position
	// Price: $50 (50000 ticks), Qty: 100 lots (1 HYPL)
	// Notional: 50000 × 100 = 5,000,000
	// Required margin @ 50x leverage (200 bps): 5,000,000 × 200 / 10000 = 100,000 (exactly $1000)
	err := am.CheckMarginRequirement(trader1, market, 50000, 100)
	if err != nil {
		t.Errorf("expected sufficient margin, got error: %v", err)
	}

	// Test 2: Insufficient margin for large position
	// Qty: 200 lots (2 HYPL) → notional = 10,000,000 → margin = 200,000 (need $2000, have $1000)
	err = am.CheckMarginRequirement(trader1, market, 50000, 200)
	if err == nil {
		t.Error("expected insufficient margin error")
	}

	// Test 3: Exceeds max position size
	// MaxPosition = 10000 lots (from defaults), try 15000 lots
	err = am.CheckMarginRequirement(trader1, market, 50000, 15000)
	if err == nil {
		t.Error("expected max position size error")
	}
}

// TestMarginRequirementWithExistingPosition tests margin checks with existing positions
func TestMarginRequirementWithExistingPosition(t *testing.T) {
	am := newTestAccountManager(t)
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Deposit $2000
	am.Deposit(trader1, 200000)

	// Open initial position: 100 lots @ $50 (margin: $1000)
	am.UpdatePosition(trader1, "HYPL-USDC", 100, 50000, 100000)
	am.LockCollateral(trader1, 100000)

	// Test 1: Add to position (should work - same direction, but smaller size)
	// Additional 50 lots @ $55 → notional = 2,750,000, margin @ 200bps = 55,000
	// Have $1000 available (total $2000 - $1000 locked)
	// This should work since 55,000 < 100,000 available
	err := am.CheckMarginRequirement(trader1, market, 55000, 50)
	if err != nil {
		t.Errorf("expected sufficient margin for adding to position: %v", err)
	}

	// Test 2: Reduce position (opposite direction, should always work)
	err = am.CheckMarginRequirement(trader1, market, 60000, -50)
	if err != nil {
		t.Errorf("expected no error for reducing position: %v", err)
	}

	// Test 3: Exceed total leverage across all positions
	// Try to add huge position that would exceed account leverage
	err = am.CheckMarginRequirement(trader1, market, 50000, 500) // 5 HYPL @ $50 = $250 notional
	if err == nil {
		t.Error("expected leverage limit error")
	}
}

// TestCheckLiquidation tests liquidation detection
func TestCheckLiquidation(t *testing.T) {
	am := newTestAccountManager(t)
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Deposit $1000 and open 100 lots @ $50 entry (margin: $1000)
	am.Deposit(trader1, 100000)
	am.UpdatePosition(trader1, "HYPL-USDC", 100, 50000, 100000)
	am.LockCollateral(trader1, 100000)

	markets := map[string]*core.Market{"HYPL-USDC": market}

	// Test 1: No liquidation at entry price
	markPrices := map[string]int64{"HYPL-USDC": 50000}
	shouldLiq, equity, maintMargin, err := am.CheckLiquidation(trader1, markets, markPrices)
	if err != nil {
		t.Fatalf("check liquidation failed: %v", err)
	}
	if shouldLiq {
		t.Error("should not liquidate at entry price")
	}
	t.Logf("At entry: equity=%d, maintMargin=%d", equity, maintMargin)

	// Test 2: No liquidation with small profit (price up to $55)
	markPrices["HYPL-USDC"] = 55000
	shouldLiq, equity, maintMargin, err = am.CheckLiquidation(trader1, markets, markPrices)
	if err != nil {
		t.Fatalf("check liquidation failed: %v", err)
	}
	if shouldLiq {
		t.Error("should not liquidate with profit")
	}
	// Equity = 100000 + (55000-50000)*100 = 100000 + 500000 = 600000
	expectedEquity := int64(600000)
	if equity != expectedEquity {
		t.Errorf("equity = %d, want %d", equity, expectedEquity)
	}

	// Test 3: Should liquidate when equity < maintenance margin
	// Maintenance margin @ 50 bps: notional × 0.005
	// At mark price $49, notional = 49000 × 100 = 4,900,000 → maint margin = 24,500
	// Equity = 100000 + (49000-50000)*100 = 100000 - 100000 = 0
	// 0 < 24500 → liquidate
	markPrices["HYPL-USDC"] = 49000
	shouldLiq, equity, maintMargin, err = am.CheckLiquidation(trader1, markets, markPrices)
	if err != nil {
		t.Fatalf("check liquidation failed: %v", err)
	}
	t.Logf("At $49: equity=%d, maintMargin=%d, shouldLiq=%v", equity, maintMargin, shouldLiq)
	if !shouldLiq {
		t.Error("should liquidate when equity < maintenance margin")
	}
}

// TestLiquidate tests the liquidation execution
func TestLiquidate(t *testing.T) {
	am := newTestAccountManager(t)
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Deposit $1000, open 100 lots @ $50 (margin: $1000)
	am.Deposit(trader1, 100000)
	am.UpdatePosition(trader1, "HYPL-USDC", 100, 50000, 100000)
	am.LockCollateral(trader1, 100000)

	markets := map[string]*core.Market{"HYPL-USDC": market}
	markPrices := map[string]int64{"HYPL-USDC": 45000} // Price dropped to $45

	// Verify should liquidate
	shouldLiq, _, _, _ := am.CheckLiquidation(trader1, markets, markPrices)
	if !shouldLiq {
		t.Fatal("expected liquidation needed")
	}

	// Execute liquidation
	finalBalance, deficit, err := am.Liquidate(trader1, markets, markPrices)
	if err != nil {
		t.Fatalf("liquidation failed: %v", err)
	}

	// Expected outcome:
	// Initial balance: 100000
	// Realized PnL: (45000 - 50000) × 100 = -500000
	// Final balance: 100000 - 500000 = -400000 (bankrupt)
	// Deficit: 400000 (owed to insurance fund)
	if finalBalance != 0 {
		t.Errorf("final balance = %d, want 0 (bankrupt)", finalBalance)
	}
	expectedDeficit := int64(400000)
	if deficit != expectedDeficit {
		t.Errorf("deficit = %d, want %d", deficit, expectedDeficit)
	}

	// Verify position closed
	acc := am.GetAccount(trader1)
	pos := acc.GetPosition("HYPL-USDC")
	if pos == nil || pos.Size != 0 {
		t.Error("position should be closed after liquidation")
	}

	// Verify margin unlocked
	if acc.LockedCollateral != 0 {
		t.Errorf("locked collateral = %d, should be 0 after liquidation", acc.LockedCollateral)
	}

	t.Logf("Liquidation result: finalBalance=%d, deficit=%d", finalBalance, deficit)
}

// TestLiquidateSolventAccount tests liquidation of account that stays solvent
func TestLiquidateSolventAccount(t *testing.T) {
	am := newTestAccountManager(t)
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Deposit $2000, open 100 lots @ $50 (margin: $1000, extra buffer: $1000)
	am.Deposit(trader1, 200000)
	am.UpdatePosition(trader1, "HYPL-USDC", 100, 50000, 100000)
	am.LockCollateral(trader1, 100000)

	markets := map[string]*core.Market{"HYPL-USDC": market}
	markPrices := map[string]int64{"HYPL-USDC": 48000} // Small loss: $2 per lot

	// Execute liquidation
	finalBalance, deficit, err := am.Liquidate(trader1, markets, markPrices)
	if err != nil {
		t.Fatalf("liquidation failed: %v", err)
	}

	// Expected:
	// Initial balance: 200000
	// Realized PnL: (48000 - 50000) × 100 = -200000
	// Final balance: 200000 - 200000 = 0
	// Deficit: 0 (account exactly zeroed out)
	expectedBalance := int64(0)
	if finalBalance != expectedBalance {
		t.Errorf("final balance = %d, want %d", finalBalance, expectedBalance)
	}
	if deficit != 0 {
		t.Errorf("deficit = %d, want 0 (account solvent)", deficit)
	}

	t.Logf("Solvent liquidation: finalBalance=%d, deficit=%d", finalBalance, deficit)
}

// TestLiquidationShortPosition tests liquidation of short position
func TestLiquidationShortPosition(t *testing.T) {
	am := newTestAccountManager(t)
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Deposit $1000, open short 100 lots @ $50 (margin: $1000)
	am.Deposit(trader1, 100000)
	am.UpdatePosition(trader1, "HYPL-USDC", -100, 50000, 100000) // Negative size = short
	am.LockCollateral(trader1, 100000)

	markets := map[string]*core.Market{"HYPL-USDC": market}

	// Test 1: Should liquidate when price rises (bad for shorts)
	// Mark price $55 → loss for short
	// PnL: (55000 - 50000) × (-100) = -500000 (loss)
	// Equity: 100000 - 500000 = -400000
	markPrices := map[string]int64{"HYPL-USDC": 55000}
	shouldLiq, equity, _, err := am.CheckLiquidation(trader1, markets, markPrices)
	if err != nil {
		t.Fatalf("check liquidation failed: %v", err)
	}
	if !shouldLiq {
		t.Error("short position should liquidate when price rises sharply")
	}
	t.Logf("Short @ $50, mark @ $55: equity=%d, shouldLiq=%v", equity, shouldLiq)

	// Test 2: Execute liquidation
	finalBalance, deficit, err := am.Liquidate(trader1, markets, markPrices)
	if err != nil {
		t.Fatalf("liquidation failed: %v", err)
	}
	if finalBalance != 0 || deficit != 400000 {
		t.Errorf("expected bankrupt account: balance=%d, deficit=%d", finalBalance, deficit)
	}
}

// TestMultiPositionLiquidation tests liquidation with multiple positions
func TestMultiPositionLiquidation(t *testing.T) {
	am := newTestAccountManager(t)
	hyplMarket, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")
	btcMarket, _ := core.NewMarketWithDefaults("BTC-USDC", "BTC", "USDC")

	// Deposit $5000
	am.Deposit(trader1, 500000)

	// Open two positions:
	// 1. HYPL: 100 lots @ $50 (margin: $1000)
	// 2. BTC: 10 lots @ $50000 (notional: 500000, margin @ 200bps: 10000)
	am.UpdatePosition(trader1, "HYPL-USDC", 100, 50000, 100000)
	am.UpdatePosition(trader1, "BTC-USDC", 10, 5000000, 1000000)
	am.LockCollateral(trader1, 1100000) // Total margin: $11,000

	markets := map[string]*core.Market{
		"HYPL-USDC": hyplMarket,
		"BTC-USDC":  btcMarket,
	}

	// Scenario: HYPL drops to $40, BTC drops to $45000
	markPrices := map[string]int64{
		"HYPL-USDC": 40000,
		"BTC-USDC":  4500000,
	}

	// Check if should liquidate
	shouldLiq, equity, maintMargin, err := am.CheckLiquidation(trader1, markets, markPrices)
	if err != nil {
		t.Fatalf("check liquidation failed: %v", err)
	}
	t.Logf("Multi-position: equity=%d, maintMargin=%d, shouldLiq=%v", equity, maintMargin, shouldLiq)

	// Expected:
	// HYPL PnL: (40000-50000) × 100 = -1000000
	// BTC PnL: (4500000-5000000) × 10 = -5000000
	// Total unrealized loss: -6000000
	// Equity: 500000 - 6000000 = -5500000 (deeply underwater)
	if !shouldLiq {
		t.Error("should liquidate with large losses")
	}

	// Execute liquidation
	finalBalance, deficit, err := am.Liquidate(trader1, markets, markPrices)
	if err != nil {
		t.Fatalf("liquidation failed: %v", err)
	}

	// Both positions should be closed
	acc := am.GetAccount(trader1)
	if len(acc.Positions) != 2 {
		t.Errorf("expected 2 positions (may have zero size), got %d", len(acc.Positions))
	}
	for symbol, pos := range acc.Positions {
		if pos.Size != 0 {
			t.Errorf("position %s not closed: size=%d", symbol, pos.Size)
		}
	}

	t.Logf("Multi-position liquidation: finalBalance=%d, deficit=%d", finalBalance, deficit)
}

// TestOrderbookMarkPrice tests mark price calculation from orderbook
func TestOrderbookMarkPrice(t *testing.T) {
	ob := core.NewOrderBook()
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Initially empty
	midPrice := ob.GetMidPrice()
	if midPrice != 0 {
		t.Errorf("empty orderbook should have mid price 0, got %d", midPrice)
	}

	// Add some orders
	bid1 := &core.Order{ID: "bid1", Symbol: "HYPL-USDC", Side: core.Buy, Price: 49000, Qty: 100, Type: "GTC"}
	bid2 := &core.Order{ID: "bid2", Symbol: "HYPL-USDC", Side: core.Buy, Price: 48000, Qty: 100, Type: "GTC"}
	ask1 := &core.Order{ID: "ask1", Symbol: "HYPL-USDC", Side: core.Sell, Price: 51000, Qty: 100, Type: "GTC"}
	ask2 := &core.Order{ID: "ask2", Symbol: "HYPL-USDC", Side: core.Sell, Price: 52000, Qty: 100, Type: "GTC"}

	ob.Place(bid1, market)
	ob.Place(bid2, market)
	ob.Place(ask1, market)
	ob.Place(ask2, market)

	// Best bid: 49000, Best ask: 51000
	// Mid price: (49000 + 51000) / 2 = 50000
	midPrice = ob.GetMidPrice()
	expectedMid := int64(50000)
	if midPrice != expectedMid {
		t.Errorf("mid price = %d, want %d", midPrice, expectedMid)
	}

	bestBid := ob.GetBestBid()
	bestAsk := ob.GetBestAsk()
	if bestBid != 49000 {
		t.Errorf("best bid = %d, want 49000", bestBid)
	}
	if bestAsk != 51000 {
		t.Errorf("best ask = %d, want 51000", bestAsk)
	}

	// Test last price after a trade
	taker := &core.Order{ID: "taker1", Symbol: "HYPL-USDC", Side: core.Buy, Price: 52000, Qty: 50, Type: "IOC"}
	fills, _ := ob.Place(taker, market)
	if len(fills) != 1 {
		t.Fatalf("expected 1 fill, got %d", len(fills))
	}

	lastPrice := ob.GetLastPrice()
	expectedLast := int64(51000) // Matched at ask1 price
	if lastPrice != expectedLast {
		t.Errorf("last price = %d, want %d", lastPrice, expectedLast)
	}

	t.Logf("Orderbook prices: bid=%d, ask=%d, mid=%d, last=%d", bestBid, bestAsk, midPrice, lastPrice)
}
