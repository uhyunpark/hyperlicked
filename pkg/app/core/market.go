package core

import (
	"fmt"
	"time"
)

// MarketType defines the type of market
type MarketType int8

const (
	Perpetual MarketType = iota // No expiry, has funding
	Future                       // Has expiry date
	Spot                         // No leverage
)

func (mt MarketType) String() string {
	switch mt {
	case Perpetual:
		return "Perpetual"
	case Future:
		return "Future"
	case Spot:
		return "Spot"
	default:
		return "Unknown"
	}
}

// MarketStatus defines the trading status of a market
type MarketStatus int8

const (
	Active MarketStatus = iota // Trading enabled
	Paused                      // Trading halted (emergency)
	Settling                    // Funding/expiry in progress
	Settled                     // Market closed
)

func (ms MarketStatus) String() string {
	switch ms {
	case Active:
		return "Active"
	case Paused:
		return "Paused"
	case Settling:
		return "Settling"
	case Settled:
		return "Settled"
	default:
		return "Unknown"
	}
}

// Market defines all parameters for a trading market (e.g., HYPL-USDC perpetual)
type Market struct {
	// Identity
	Symbol     string       // "HYPL-USDC"
	BaseAsset  string       // "HYPL"
	QuoteAsset string       // "USDC"
	Type       MarketType   // Perpetual, Future, Spot
	Status     MarketStatus // Active, Paused, Settling, Settled

	// Price & Size Precision
	// TickSize: Minimum price increment (e.g., 1 tick = $0.001)
	// All prices stored as integer ticks
	TickSize int64 // 1 = $0.001, 10 = $0.01, 100 = $0.10

	// LotSize: Minimum size increment (e.g., 1 lot = 0.01 HYPL)
	// All quantities stored as integer lots
	LotSize int64 // 1 = 0.01 base asset

	// MinNotional: Minimum order value in quote asset (e.g., $10)
	// Prevents dust orders
	MinNotional int64 // In quote asset cents (1000 = $10)

	// Leverage & Margin (Perpetual/Future only)
	MaxLeverage          int64 // e.g., 50 (50x leverage)
	InitialMarginBps     int64 // Basis points, e.g., 200 bps = 2% = 50x leverage
	MaintenanceMarginBps int64 // Basis points, e.g., 50 bps = 0.5% (liquidation threshold)

	// Funding Rate (Perpetual only)
	FundingInterval   time.Duration // e.g., 1 hour (Hyperliquid standard)
	MaxFundingRateBps int64         // Maximum funding rate per interval (e.g., 1200 bps = 0.12%)

	// Position & Order Limits
	MinOrderSize int64 // Minimum order size in lots (e.g., 1 lot = 0.01 HYPL)
	MaxOrderSize int64 // Maximum single order size in lots
	MaxPosition  int64 // Maximum position size in lots (per account)

	// Fees
	MakerFeeBps int64 // Maker fee in basis points (can be negative for rebate, e.g., -2 bps)
	TakerFeeBps int64 // Taker fee in basis points (e.g., 5 bps = 0.05%)

	// Metadata
	LaunchedAt int64 // Block height when market was opened
}

// NewMarket creates a new market with validation
func NewMarket(symbol, baseAsset, quoteAsset string, params MarketParams) (*Market, error) {
	m := &Market{
		Symbol:               symbol,
		BaseAsset:            baseAsset,
		QuoteAsset:           quoteAsset,
		Type:                 params.Type,
		Status:               Active,
		TickSize:             params.TickSize,
		LotSize:              params.LotSize,
		MinNotional:          params.MinNotional,
		MaxLeverage:          params.MaxLeverage,
		InitialMarginBps:     params.InitialMarginBps,
		MaintenanceMarginBps: params.MaintenanceMarginBps,
		FundingInterval:      params.FundingInterval,
		MaxFundingRateBps:    params.MaxFundingRateBps,
		MinOrderSize:         params.MinOrderSize,
		MaxOrderSize:         params.MaxOrderSize,
		MaxPosition:          params.MaxPosition,
		MakerFeeBps:          params.MakerFeeBps,
		TakerFeeBps:          params.TakerFeeBps,
		LaunchedAt:           0, // Set when market opens
	}

	if err := m.Validate(); err != nil {
		return nil, fmt.Errorf("invalid market params: %w", err)
	}

	return m, nil
}

// Validate checks market parameter sanity
func (m *Market) Validate() error {
	if m.Symbol == "" {
		return fmt.Errorf("symbol cannot be empty")
	}
	if m.BaseAsset == "" || m.QuoteAsset == "" {
		return fmt.Errorf("base and quote assets must be specified")
	}
	if m.TickSize <= 0 {
		return fmt.Errorf("tick size must be positive")
	}
	if m.LotSize <= 0 {
		return fmt.Errorf("lot size must be positive")
	}
	if m.MinNotional < 0 {
		return fmt.Errorf("min notional cannot be negative")
	}

	// Leverage/Margin validation (for Perpetual/Future)
	if m.Type != Spot {
		if m.MaxLeverage <= 0 {
			return fmt.Errorf("max leverage must be positive")
		}
		if m.InitialMarginBps <= 0 {
			return fmt.Errorf("initial margin must be positive")
		}
		if m.MaintenanceMarginBps <= 0 {
			return fmt.Errorf("maintenance margin must be positive")
		}
		if m.MaintenanceMarginBps > m.InitialMarginBps {
			return fmt.Errorf("maintenance margin cannot exceed initial margin")
		}

		// Check leverage consistency: MaxLeverage ≈ 10000 / InitialMarginBps
		expectedLeverage := 10000 / m.InitialMarginBps
		if m.MaxLeverage > expectedLeverage*2 || m.MaxLeverage < expectedLeverage/2 {
			return fmt.Errorf("max leverage (%d) inconsistent with initial margin (%d bps)", m.MaxLeverage, m.InitialMarginBps)
		}
	}

	// Funding validation (for Perpetual)
	if m.Type == Perpetual {
		if m.FundingInterval <= 0 {
			return fmt.Errorf("funding interval must be positive")
		}
		if m.MaxFundingRateBps < 0 {
			return fmt.Errorf("max funding rate cannot be negative")
		}
	}

	// Order limits
	if m.MinOrderSize <= 0 {
		return fmt.Errorf("min order size must be positive")
	}
	if m.MaxOrderSize <= 0 {
		return fmt.Errorf("max order size must be positive")
	}
	if m.MinOrderSize > m.MaxOrderSize {
		return fmt.Errorf("min order size cannot exceed max order size")
	}
	if m.MaxPosition < m.MaxOrderSize {
		return fmt.Errorf("max position should be >= max order size")
	}

	// Fees (can be negative for maker rebates)
	if m.TakerFeeBps < 0 {
		return fmt.Errorf("taker fee cannot be negative")
	}

	return nil
}

// TicksToUSDC converts integer ticks to USDC dollars (float64)
// Example: 1234 ticks with TickSize=1 (0.001) → $1.234
func (m *Market) TicksToUSDC(ticks int64) float64 {
	return float64(ticks) * float64(m.TickSize) / 1000.0
}

// USDCToTicks converts USDC dollars to integer ticks
// Example: $1.234 with TickSize=1 (0.001) → 1234 ticks
func (m *Market) USDCToTicks(usdc float64) int64 {
	return int64(usdc * 1000.0 / float64(m.TickSize))
}

// LotsToBase converts integer lots to base asset quantity (float64)
// LotSize=100 means 1 lot = 0.01 base asset (100 lots = 1 base)
// Example: 100 lots with LotSize=100 → 1.0 HYPL
func (m *Market) LotsToBase(lots int64) float64 {
	return float64(lots) / float64(m.LotSize)
}

// BaseToLots converts base asset quantity to integer lots
// Example: 1.0 HYPL with LotSize=100 → 100 lots
func (m *Market) BaseToLots(base float64) int64 {
	return int64(base * float64(m.LotSize))
}

// RequiredInitialMargin calculates initial margin needed to open a position
// Returns margin in quote asset cents (USDC cents)
// Formula: Notional × InitialMarginBps / 10000
func (m *Market) RequiredInitialMargin(price, qty int64) int64 {
	notional := price * qty // In tick×lots
	return (notional * m.InitialMarginBps) / 10000
}

// RequiredMaintenanceMargin calculates maintenance margin to avoid liquidation
// Returns margin in quote asset cents
// Formula: Notional × MaintenanceMarginBps / 10000
func (m *Market) RequiredMaintenanceMargin(price, qty int64) int64 {
	notional := price * qty
	return (notional * m.MaintenanceMarginBps) / 10000
}

// ComputeLeverage calculates effective leverage from position value and margin
// Formula: Leverage = PositionValue / Margin
func (m *Market) ComputeLeverage(positionValue, margin int64) int64 {
	if margin == 0 {
		return 0
	}
	return positionValue / margin
}

// ValidateOrderSize checks if order size is within limits
func (m *Market) ValidateOrderSize(qty int64) error {
	if qty < m.MinOrderSize {
		return fmt.Errorf("order size %d below minimum %d", qty, m.MinOrderSize)
	}
	if qty > m.MaxOrderSize {
		return fmt.Errorf("order size %d exceeds maximum %d", qty, m.MaxOrderSize)
	}
	return nil
}

// ValidateOrderNotional checks if order value meets minimum
func (m *Market) ValidateOrderNotional(price, qty int64) error {
	notional := price * qty
	if notional < m.MinNotional {
		return fmt.Errorf("order notional %d below minimum %d", notional, m.MinNotional)
	}
	return nil
}

// ValidateOrder performs all order validations
func (m *Market) ValidateOrder(price, qty int64) error {
	if m.Status != Active {
		return fmt.Errorf("market %s is not active (status: %s)", m.Symbol, m.Status)
	}
	if price <= 0 {
		return fmt.Errorf("price must be positive")
	}
	if qty <= 0 {
		return fmt.Errorf("quantity must be positive")
	}
	if err := m.ValidateOrderSize(qty); err != nil {
		return err
	}
	if err := m.ValidateOrderNotional(price, qty); err != nil {
		return err
	}
	return nil
}
