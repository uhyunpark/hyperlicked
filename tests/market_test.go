package tests

import (
	"testing"
	"time"

	"github.com/uhyunpark/hyperlicked/pkg/app/core"
)

// TestMarketCreation tests basic market creation and validation
func TestMarketCreation(t *testing.T) {
	market, err := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")
	if err != nil {
		t.Fatalf("failed to create market: %v", err)
	}

	if market.Symbol != "HYPL-USDC" {
		t.Errorf("expected symbol HYPL-USDC, got %s", market.Symbol)
	}
	if market.Type != core.Perpetual {
		t.Errorf("expected Perpetual type, got %v", market.Type)
	}
	if market.Status != core.Active {
		t.Errorf("expected Active status, got %v", market.Status)
	}
}

// TestMarketValidation tests parameter validation
func TestMarketValidation(t *testing.T) {
	tests := []struct {
		name    string
		params  core.MarketParams
		wantErr bool
	}{
		{
			name:    "valid default params",
			params:  core.DefaultHYPLUSDC,
			wantErr: false,
		},
		{
			name: "negative tick size",
			params: core.MarketParams{
				Type:                 core.Perpetual,
				TickSize:             -1,
				LotSize:              100,
				MaxLeverage:          50,
				InitialMarginBps:     200,
				MaintenanceMarginBps: 50,
				FundingInterval:      time.Hour,
				MinOrderSize:         1,
				MaxOrderSize:         1000000,
				MaxPosition:          10000000,
			},
			wantErr: true,
		},
		{
			name: "maintenance margin > initial margin",
			params: core.MarketParams{
				Type:                 core.Perpetual,
				TickSize:             1,
				LotSize:              100,
				MaxLeverage:          50,
				InitialMarginBps:     100,
				MaintenanceMarginBps: 200, // Invalid: > initial
				FundingInterval:      time.Hour,
				MinOrderSize:         1,
				MaxOrderSize:         1000000,
				MaxPosition:          10000000,
			},
			wantErr: true,
		},
		{
			name: "leverage inconsistent with margin",
			params: core.MarketParams{
				Type:                 core.Perpetual,
				TickSize:             1,
				LotSize:              100,
				MaxLeverage:          1000, // 1000x leverage
				InitialMarginBps:     200,  // But 2% margin (50x)
				MaintenanceMarginBps: 50,
				FundingInterval:      time.Hour,
				MinOrderSize:         1,
				MaxOrderSize:         1000000,
				MaxPosition:          10000000,
			},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := core.NewMarket("TEST", "TEST", "USDC", tt.params)
			if (err != nil) != tt.wantErr {
				t.Errorf("wantErr=%v, got err=%v", tt.wantErr, err)
			}
		})
	}
}

// TestPriceConversions tests tick/USDC conversions
func TestPriceConversions(t *testing.T) {
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	tests := []struct {
		ticks int64
		usdc  float64
	}{
		{ticks: 1000, usdc: 1.0},     // 1000 ticks = $1.00
		{ticks: 50000, usdc: 50.0},   // 50000 ticks = $50.00
		{ticks: 123, usdc: 0.123},    // 123 ticks = $0.123
		{ticks: 1, usdc: 0.001},      // 1 tick = $0.001
		{ticks: 100000, usdc: 100.0}, // 100000 ticks = $100.00
	}

	for _, tt := range tests {
		// Test ticks → USDC
		gotUSDC := market.TicksToUSDC(tt.ticks)
		if gotUSDC != tt.usdc {
			t.Errorf("TicksToUSDC(%d) = %f, want %f", tt.ticks, gotUSDC, tt.usdc)
		}

		// Test USDC → ticks (round trip)
		gotTicks := market.USDCToTicks(tt.usdc)
		if gotTicks != tt.ticks {
			t.Errorf("USDCToTicks(%f) = %d, want %d", tt.usdc, gotTicks, tt.ticks)
		}
	}
}

// TestSizeConversions tests lot/base asset conversions
func TestSizeConversions(t *testing.T) {
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	tests := []struct {
		lots int64
		base float64
	}{
		{lots: 100, base: 1.0},    // 100 lots = 1.0 HYPL
		{lots: 1, base: 0.01},     // 1 lot = 0.01 HYPL
		{lots: 1000, base: 10.0},  // 1000 lots = 10.0 HYPL
		{lots: 50, base: 0.5},     // 50 lots = 0.5 HYPL
		{lots: 10000, base: 100.0}, // 10000 lots = 100.0 HYPL
	}

	for _, tt := range tests {
		// Test lots → base
		gotBase := market.LotsToBase(tt.lots)
		if gotBase != tt.base {
			t.Errorf("LotsToBase(%d) = %f, want %f", tt.lots, gotBase, tt.base)
		}

		// Test base → lots (round trip)
		gotLots := market.BaseToLots(tt.base)
		if gotLots != tt.lots {
			t.Errorf("BaseToLots(%f) = %d, want %d", tt.base, gotLots, tt.lots)
		}
	}
}

// TestMarginCalculations tests margin requirement calculations
func TestMarginCalculations(t *testing.T) {
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Test case: 100 HYPL (10000 lots) at $50 (50000 ticks)
	// Notional = 50000 × 10000 = 500,000,000
	price := int64(50000)  // $50
	qty := int64(10000)    // 100 HYPL
	notional := price * qty // 500,000,000

	// Initial margin: 2% = 200 bps
	expectedInitial := (notional * 200) / 10000 // 10,000,000
	gotInitial := market.RequiredInitialMargin(price, qty)
	if gotInitial != expectedInitial {
		t.Errorf("RequiredInitialMargin = %d, want %d", gotInitial, expectedInitial)
	}

	// Maintenance margin: 0.5% = 50 bps
	expectedMaint := (notional * 50) / 10000 // 2,500,000
	gotMaint := market.RequiredMaintenanceMargin(price, qty)
	if gotMaint != expectedMaint {
		t.Errorf("RequiredMaintenanceMargin = %d, want %d", gotMaint, expectedMaint)
	}

	// Leverage = Notional / Margin
	leverage := market.ComputeLeverage(notional, gotInitial)
	if leverage != 50 {
		t.Errorf("ComputeLeverage = %d, want 50", leverage)
	}
}

// TestOrderValidation tests order parameter validation
func TestOrderValidation(t *testing.T) {
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	tests := []struct {
		name    string
		price   int64
		qty     int64
		wantErr bool
	}{
		{
			name:    "valid order",
			price:   50000, // $50
			qty:     100,   // 1 HYPL
			wantErr: false,
		},
		{
			name:    "below min size",
			price:   50000,
			qty:     0, // Below min (1 lot)
			wantErr: true,
		},
		{
			name:    "above max size",
			price:   50000,
			qty:     2000000, // Above max (1,000,000 lots)
			wantErr: true,
		},
		{
			name:    "below min notional",
			price:   1,  // $0.001
			qty:     1,  // 0.01 HYPL
			wantErr: true, // Notional = 1 < 10000
		},
		{
			name:    "zero price",
			price:   0,
			qty:     100,
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := market.ValidateOrder(tt.price, tt.qty)
			if (err != nil) != tt.wantErr {
				t.Errorf("ValidateOrder() error = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}
}

// TestMarketRegistry tests multi-market management
func TestMarketRegistry(t *testing.T) {
	registry := core.NewMarketRegistry()

	// Test registration
	market1, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")
	err := registry.RegisterMarket(market1)
	if err != nil {
		t.Fatalf("failed to register market: %v", err)
	}

	// Test duplicate registration
	err = registry.RegisterMarket(market1)
	if err == nil {
		t.Error("expected error for duplicate registration, got nil")
	}

	// Test retrieval
	got, err := registry.GetMarket("HYPL-USDC")
	if err != nil {
		t.Errorf("failed to get market: %v", err)
	}
	if got.Symbol != "HYPL-USDC" {
		t.Errorf("got wrong market: %s", got.Symbol)
	}

	// Test not found
	_, err = registry.GetMarket("NONEXISTENT")
	if err == nil {
		t.Error("expected error for nonexistent market, got nil")
	}

	// Test count
	if registry.Count() != 1 {
		t.Errorf("expected count=1, got %d", registry.Count())
	}

	// Test list
	markets := registry.ListMarkets()
	if len(markets) != 1 {
		t.Errorf("expected 1 market, got %d", len(markets))
	}
}

// TestMarketStatusTransitions tests status updates
func TestMarketStatusTransitions(t *testing.T) {
	registry := core.NewMarketRegistry()
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")
	registry.RegisterMarket(market)

	// Active → Paused
	err := registry.UpdateMarketStatus("HYPL-USDC", core.Paused)
	if err != nil {
		t.Errorf("Active → Paused failed: %v", err)
	}

	// Paused → Active
	err = registry.UpdateMarketStatus("HYPL-USDC", core.Active)
	if err != nil {
		t.Errorf("Paused → Active failed: %v", err)
	}

	// Active → Settling
	err = registry.UpdateMarketStatus("HYPL-USDC", core.Settling)
	if err != nil {
		t.Errorf("Active → Settling failed: %v", err)
	}

	// Settling → Settled
	err = registry.UpdateMarketStatus("HYPL-USDC", core.Settled)
	if err != nil {
		t.Errorf("Settling → Settled failed: %v", err)
	}

	// Settled → * (should fail)
	err = registry.UpdateMarketStatus("HYPL-USDC", core.Active)
	if err == nil {
		t.Error("expected error for Settled → Active, got nil")
	}
}

// TestCustomPerpetual tests custom market creation
func TestCustomPerpetual(t *testing.T) {
	// Create 20x leverage market (5% initial margin)
	params := core.CustomPerpetual(1, 100, 20)

	if params.MaxLeverage != 20 {
		t.Errorf("expected leverage=20, got %d", params.MaxLeverage)
	}

	expectedInitial := int64(10000 / 20) // 500 bps = 5%
	if params.InitialMarginBps != expectedInitial {
		t.Errorf("expected initial margin=%d, got %d", expectedInitial, params.InitialMarginBps)
	}

	expectedMaint := expectedInitial / 4 // 125 bps = 1.25%
	if params.MaintenanceMarginBps != expectedMaint {
		t.Errorf("expected maintenance margin=%d, got %d", expectedMaint, params.MaintenanceMarginBps)
	}
}
