package core

import "time"

// MarketParams is a helper struct for creating markets with all parameters
// This separates config from the runtime Market struct
type MarketParams struct {
	Type                 MarketType
	TickSize             int64
	LotSize              int64
	MinNotional          int64
	MaxLeverage          int64
	InitialMarginBps     int64
	MaintenanceMarginBps int64
	FundingInterval      time.Duration
	MaxFundingRateBps    int64
	MinOrderSize         int64
	MaxOrderSize         int64
	MaxPosition          int64
	MakerFeeBps          int64
	TakerFeeBps          int64
}

// DefaultHYPLUSDC returns default parameters for HYPL-USDC perpetual futures
// Based on typical Hyperliquid market parameters
var DefaultHYPLUSDC = MarketParams{
	Type: Perpetual,

	// Price & Size Precision
	// TickSize: 1 = $0.001 (tenth of a cent precision)
	// Example: price=50000 ticks = $50.00
	TickSize: 1,

	// LotSize: 100 = 1 HYPL (0.01 HYPL per lot)
	// Example: qty=100 lots = 1.00 HYPL
	LotSize: 100,

	// MinNotional: $10 minimum order value (10,000 ticks × lots)
	MinNotional: 10000,

	// Leverage & Margin
	// 50x max leverage → 2% initial margin (200 bps)
	MaxLeverage:          50,
	InitialMarginBps:     200, // 2% = 50x leverage
	MaintenanceMarginBps: 50,  // 0.5% = liquidation at ~200x leverage

	// Funding Rate (Perpetual only)
	// 1 hour intervals (Hyperliquid standard)
	// Max 0.12% per hour = ±1200 bps cap
	FundingInterval:   1 * time.Hour,
	MaxFundingRateBps: 1200, // 0.12% max

	// Order Limits
	// Min: 0.01 HYPL = 1 lot
	// Max: 10,000 HYPL = 1,000,000 lots (single order)
	// Max Position: 100,000 HYPL = 10,000,000 lots
	MinOrderSize: 1,
	MaxOrderSize: 1000000,
	MaxPosition:  10000000,

	// Fees (basis points)
	// Maker: -0.02% (rebate) = -2 bps
	// Taker: 0.05% = 5 bps
	MakerFeeBps: -2, // Rebate to makers
	TakerFeeBps: 5,  // Fee from takers
}

// NewMarketWithDefaults creates a market using default HYPL-USDC parameters
func NewMarketWithDefaults(symbol, baseAsset, quoteAsset string) (*Market, error) {
	return NewMarket(symbol, baseAsset, quoteAsset, DefaultHYPLUSDC)
}

// CustomPerpetual returns a customizable perpetual market template
func CustomPerpetual(tickSize, lotSize, leverage int64) MarketParams {
	initialMargin := 10000 / leverage // e.g., 50x → 200 bps
	maintMargin := initialMargin / 4   // 1/4 of initial = 4x buffer

	return MarketParams{
		Type:                 Perpetual,
		TickSize:             tickSize,
		LotSize:              lotSize,
		MinNotional:          10000,
		MaxLeverage:          leverage,
		InitialMarginBps:     initialMargin,
		MaintenanceMarginBps: maintMargin,
		FundingInterval:      1 * time.Hour,
		MaxFundingRateBps:    1200,
		MinOrderSize:         1,
		MaxOrderSize:         1000000,
		MaxPosition:          10000000,
		MakerFeeBps:          -2,
		TakerFeeBps:          5,
	}
}
