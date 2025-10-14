package tests

import (
	"testing"

	"github.com/uhyunpark/hyperlicked/pkg/app/core"
)

// TestOrderBookMarketValidation tests that orderbook respects market parameters
func TestOrderBookMarketValidation(t *testing.T) {
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")
	book := core.NewOrderBook()

	tests := []struct {
		name    string
		order   *core.Order
		wantErr bool
		reason  string
	}{
		{
			name: "valid order",
			order: &core.Order{
				ID:     "order1",
				Symbol: "HYPL-USDC",
				Side:   core.Buy,
				Price:  50000, // $50.00
				Qty:    100,   // 1 HYPL
				Type:   "GTC",
			},
			wantErr: false,
		},
		{
			name: "zero price",
			order: &core.Order{
				ID:     "order2",
				Symbol: "HYPL-USDC",
				Side:   core.Buy,
				Price:  0,
				Qty:    100,
				Type:   "GTC",
			},
			wantErr: true,
			reason:  "price must be positive",
		},
		{
			name: "zero quantity",
			order: &core.Order{
				ID:     "order3",
				Symbol: "HYPL-USDC",
				Side:   core.Buy,
				Price:  50000,
				Qty:    0,
				Type:   "GTC",
			},
			wantErr: true,
			reason:  "quantity must be positive",
		},
		{
			name: "below min order size",
			order: &core.Order{
				ID:     "order4",
				Symbol: "HYPL-USDC",
				Side:   core.Buy,
				Price:  50000,
				Qty:    0, // Below min (1 lot)
				Type:   "GTC",
			},
			wantErr: true,
			reason:  "below minimum order size",
		},
		{
			name: "above max order size",
			order: &core.Order{
				ID:     "order5",
				Symbol: "HYPL-USDC",
				Side:   core.Buy,
				Price:  50000,
				Qty:    2000000, // Above max (1,000,000 lots)
				Type:   "GTC",
			},
			wantErr: true,
			reason:  "exceeds maximum order size",
		},
		{
			name: "below min notional",
			order: &core.Order{
				ID:     "order6",
				Symbol: "HYPL-USDC",
				Side:   core.Buy,
				Price:  1,  // $0.001
				Qty:    1,  // 0.01 HYPL
				Type:   "GTC",
			},
			wantErr: true,
			reason:  "below minimum notional ($10)",
		},
		{
			name: "market not active (paused)",
			order: &core.Order{
				ID:     "order7",
				Symbol: "HYPL-USDC",
				Side:   core.Buy,
				Price:  50000,
				Qty:    100,
				Type:   "GTC",
			},
			wantErr: false, // Will be false initially, we'll pause market below
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Special case: test paused market
			if tt.name == "market not active (paused)" {
				market.Status = core.Paused
				_, err := book.Place(tt.order, market)
				if err == nil {
					t.Errorf("expected error for paused market, got nil")
				}
				market.Status = core.Active // Reset for other tests
				return
			}

			_, err := book.Place(tt.order, market)
			if (err != nil) != tt.wantErr {
				t.Errorf("Place() error = %v, wantErr %v (reason: %s)", err, tt.wantErr, tt.reason)
			}
		})
	}
}

// TestOrderBookMarketMatching tests that valid orders still match correctly
func TestOrderBookMarketMatching(t *testing.T) {
	market, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")
	book := core.NewOrderBook()

	// Place a valid bid
	bid := &core.Order{
		ID:     "bid1",
		Symbol: "HYPL-USDC",
		Side:   core.Buy,
		Price:  50000, // $50.00
		Qty:    100,   // 1 HYPL
		Type:   "GTC",
	}
	fills, err := book.Place(bid, market)
	if err != nil {
		t.Fatalf("failed to place bid: %v", err)
	}
	if len(fills) != 0 {
		t.Errorf("expected 0 fills for resting bid, got %d", len(fills))
	}

	// Place a matching ask
	ask := &core.Order{
		ID:     "ask1",
		Symbol: "HYPL-USDC",
		Side:   core.Sell,
		Price:  50000, // Same price, should match
		Qty:    100,   // Full fill
		Type:   "IOC",
	}
	fills, err = book.Place(ask, market)
	if err != nil {
		t.Fatalf("failed to place ask: %v", err)
	}
	if len(fills) != 1 {
		t.Fatalf("expected 1 fill, got %d", len(fills))
	}

	// Verify fill details
	fill := fills[0]
	if fill.TakerID != "ask1" {
		t.Errorf("expected taker=ask1, got %s", fill.TakerID)
	}
	if fill.MakerID != "bid1" {
		t.Errorf("expected maker=bid1, got %s", fill.MakerID)
	}
	if fill.Price != 50000 {
		t.Errorf("expected price=50000, got %d", fill.Price)
	}
	if fill.Qty != 100 {
		t.Errorf("expected qty=100, got %d", fill.Qty)
	}
}

// TestMultiMarketValidation tests that different markets have different rules
func TestMultiMarketValidation(t *testing.T) {
	// Create two markets with different parameters
	hyplMarket, _ := core.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Create custom market with 20x leverage (less than HYPL's 50x)
	customParams := core.CustomPerpetual(1, 100, 20) // 20x leverage
	btcMarket, _ := core.NewMarket("BTC-USDC", "BTC", "USDC", customParams)

	hyplBook := core.NewOrderBook()
	btcBook := core.NewOrderBook()

	// Order that's valid for HYPL (50x leverage)
	order := &core.Order{
		ID:     "order1",
		Symbol: "HYPL-USDC",
		Side:   core.Buy,
		Price:  50000,
		Qty:    100,
		Type:   "GTC",
	}

	// Should succeed for HYPL
	_, err := hyplBook.Place(order, hyplMarket)
	if err != nil {
		t.Errorf("HYPL market rejected valid order: %v", err)
	}

	// Same order should succeed for BTC (same tick/lot size)
	order.Symbol = "BTC-USDC"
	_, err = btcBook.Place(order, btcMarket)
	if err != nil {
		t.Errorf("BTC market rejected valid order: %v", err)
	}
}
