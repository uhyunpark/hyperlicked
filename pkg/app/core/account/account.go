package account

import (
	"fmt"

	"github.com/ethereum/go-ethereum/common"
)

// Account represents a user account with EVM-compatible address
// Tracks USDC balance, collateral, and open positions
type Account struct {
	Address common.Address // EVM 20-byte address (0x...)
	Nonce   uint64         // Transaction nonce for replay protection (Ethereum-compatible)

	// Balance tracking (all values in USDC cents: 100 = $1.00)
	USDCBalance      int64 // Total USDC deposited via bridge
	LockedCollateral int64 // Collateral locked for open orders + positions

	// Open positions (per symbol)
	Positions map[string]*Position // symbol → position (e.g., "HYPL-USDC" → long 100 HYPL)

	// Cumulative statistics
	RealizedPnL      int64 // Total realized profit/loss (closed positions)
	TotalFeesPaid    int64 // Cumulative taker fees paid
	TotalFeesEarned  int64 // Cumulative maker rebates earned
	TotalVolume      int64 // Lifetime trading volume (in USDC cents)
	TradeCount       int64 // Total number of trades executed
}

// Position represents an open perpetual futures position
type Position struct {
	Symbol string // Market symbol (e.g., "HYPL-USDC")

	// Position size (+ve = long, -ve = short, in lots)
	// Example: 100 lots = 1 HYPL for LotSize=100
	Size int64

	// Volume-weighted average entry price (in ticks)
	// Updated on each fill: newEntry = (oldEntry × oldSize + fillPrice × fillSize) / newSize
	EntryPrice int64

	// Collateral locked for this position (initial margin)
	// Dynamically adjusted based on position size and leverage
	Margin int64

	// Unrealized PnL (computed from mark price, not stored)
	// For display only - calculated as: (markPrice - entryPrice) × size
	// Positive = profit, negative = loss
}

// NewAccount creates a new account with zero balance
func NewAccount(addr common.Address) *Account {
	return &Account{
		Address:   addr,
		Positions: make(map[string]*Position),
	}
}

// AvailableBalance returns balance available for new orders
// Formula: Total - Locked
func (a *Account) AvailableBalance() int64 {
	return a.USDCBalance - a.LockedCollateral
}

// TotalEquity returns total account value including unrealized PnL
// Formula: Balance + UnrealizedPnL across all positions
// Note: Requires mark prices to compute unrealized PnL
func (a *Account) TotalEquity(markPrices map[string]int64) int64 {
	equity := a.USDCBalance
	for symbol, pos := range a.Positions {
		markPrice, ok := markPrices[symbol]
		if !ok || pos.Size == 0 {
			continue
		}
		// Unrealized PnL = (markPrice - entryPrice) × size
		// For shorts (negative size), PnL is reversed: profit when price drops
		unrealizedPnL := (markPrice - pos.EntryPrice) * pos.Size
		equity += unrealizedPnL
	}
	return equity
}

// GetPosition returns position for a symbol, or nil if no position
func (a *Account) GetPosition(symbol string) *Position {
	return a.Positions[symbol]
}

// HasPosition returns true if account has an open position in symbol
func (a *Account) HasPosition(symbol string) bool {
	pos, ok := a.Positions[symbol]
	return ok && pos.Size != 0
}

// TotalPositionMargin returns sum of all position margins
func (a *Account) TotalPositionMargin() int64 {
	total := int64(0)
	for _, pos := range a.Positions {
		total += pos.Margin
	}
	return total
}

// Validate checks account invariants
func (a *Account) Validate() error {
	if a.USDCBalance < 0 {
		return fmt.Errorf("negative balance: %d", a.USDCBalance)
	}
	if a.LockedCollateral < 0 {
		return fmt.Errorf("negative locked collateral: %d", a.LockedCollateral)
	}
	if a.LockedCollateral > a.USDCBalance {
		return fmt.Errorf("locked collateral (%d) exceeds balance (%d)", a.LockedCollateral, a.USDCBalance)
	}

	// Validate all positions
	totalMargin := int64(0)
	for symbol, pos := range a.Positions {
		if pos.Symbol != symbol {
			return fmt.Errorf("position symbol mismatch: map key=%s, pos.Symbol=%s", symbol, pos.Symbol)
		}
		if pos.Margin < 0 {
			return fmt.Errorf("negative margin for %s: %d", symbol, pos.Margin)
		}
		totalMargin += pos.Margin
	}

	// Total position margins should not exceed locked collateral
	// (locked collateral = position margins + open order margins)
	if totalMargin > a.LockedCollateral {
		return fmt.Errorf("total position margin (%d) exceeds locked collateral (%d)", totalMargin, a.LockedCollateral)
	}

	return nil
}

// UnrealizedPnL computes unrealized profit/loss for a position
// Formula: (markPrice - entryPrice) × size
// Positive = profit, negative = loss
func (p *Position) UnrealizedPnL(markPrice int64) int64 {
	if p.Size == 0 {
		return 0
	}
	return (markPrice - p.EntryPrice) * p.Size
}

// IsLong returns true if position is long (size > 0)
func (p *Position) IsLong() bool {
	return p.Size > 0
}

// IsShort returns true if position is short (size < 0)
func (p *Position) IsShort() bool {
	return p.Size < 0
}

// Notional returns position notional value at given price
// Formula: |size| × price
func (p *Position) Notional(price int64) int64 {
	size := p.Size
	if size < 0 {
		size = -size
	}
	return size * price
}

// Leverage returns effective leverage of position
// Formula: Notional / Margin
func (p *Position) Leverage(price int64) int64 {
	if p.Margin == 0 {
		return 0
	}
	return p.Notional(price) / p.Margin
}

// MarginRatio returns margin as percentage of notional
// Formula: Margin / Notional × 10000 (in basis points)
// Example: 200 bps = 2% = 50x leverage
func (p *Position) MarginRatio(price int64) int64 {
	notional := p.Notional(price)
	if notional == 0 {
		return 0
	}
	return (p.Margin * 10000) / notional
}

// OrderStatus represents the lifecycle state of an order
type OrderStatus int8

const (
	OrderOpen OrderStatus = iota
	OrderPartiallyFilled
	OrderFilled
	OrderCancelled
	OrderRejected
)

func (s OrderStatus) String() string {
	switch s {
	case OrderOpen:
		return "open"
	case OrderPartiallyFilled:
		return "partially_filled"
	case OrderFilled:
		return "filled"
	case OrderCancelled:
		return "cancelled"
	case OrderRejected:
		return "rejected"
	default:
		return "unknown"
	}
}

// Order represents a resting order on the orderbook (tracked at account level)
type Order struct {
	ID     string         // Unique order ID (format: {address}-ord-{nonce}-{timestamp})
	Owner  common.Address // Account that owns this order
	Symbol string         // Market symbol (e.g., "HYPL-USDC")
	Side   string         // "buy" or "sell"
	Type   string         // "GTC" or "IOC"

	// Order details
	Price  int64 // Limit price (in ticks)
	Qty    int64 // Total quantity (in lots)
	Filled int64 // Quantity filled so far (in lots)

	// Status
	Status OrderStatus

	// Margin
	LockedMargin int64 // Collateral locked for this order

	// Timestamps (Unix milliseconds)
	CreatedAt int64
	UpdatedAt int64
}

// Remaining returns unfilled quantity
func (o *Order) Remaining() int64 {
	return o.Qty - o.Filled
}

// IsClosed returns true if order is no longer active
func (o *Order) IsClosed() bool {
	return o.Status == OrderFilled || o.Status == OrderCancelled || o.Status == OrderRejected
}

// Trade represents a completed fill between taker and maker (for history tracking)
type Trade struct {
	ID        string         // Unique trade ID
	Symbol    string         // Market symbol
	Price     int64          // Execution price (in ticks)
	Qty       int64          // Filled quantity (in lots)
	Side      string         // Taker side ("buy" or "sell")
	TakerID   string         // Taker order ID
	MakerID   string         // Maker order ID
	TakerAddr common.Address // Taker account address
	MakerAddr common.Address // Maker account address
	Timestamp int64          // Execution time (Unix milliseconds)
}
