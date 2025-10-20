package api

// API response types for REST endpoints and WebSocket messages

// ==============================
// REST Response Types
// ==============================

// MarketInfo represents a market's static configuration
type MarketInfo struct {
	Symbol        string  `json:"symbol"`         // e.g., "BTC-USDT"
	BaseAsset     string  `json:"baseAsset"`      // e.g., "BTC"
	QuoteAsset    string  `json:"quoteAsset"`     // e.g., "USDT"
	Type          string  `json:"type"`           // "Perpetual", "Future", "Spot"
	Status        string  `json:"status"`         // "Active", "Paused", "Settled"
	TickSize      int64   `json:"tickSize"`       // Minimum price increment
	LotSize       int64   `json:"lotSize"`        // Minimum size increment
	MaxLeverage   int     `json:"maxLeverage"`    // Maximum leverage allowed
	TakerFeeBps   int64   `json:"takerFeeBps"`    // Taker fee in basis points
	MakerFeeBps   int64   `json:"makerFeeBps"`    // Maker fee in basis points (can be negative for rebates)
	MaintenanceMarginBps int64 `json:"maintenanceMarginBps"` // Maintenance margin %
}

// OrderbookSnapshot represents current orderbook state
type OrderbookSnapshot struct {
	Symbol    string        `json:"symbol"`
	Bids      []PriceLevel  `json:"bids"` // Sorted high to low
	Asks      []PriceLevel  `json:"asks"` // Sorted low to high
	Timestamp int64         `json:"timestamp"` // Unix milliseconds
}

// PriceLevel represents [price, size] tuple
type PriceLevel struct {
	Price int64 `json:"price"` // Price in quote asset (cents for USDT)
	Size  int64 `json:"size"`  // Size in base asset (sats for BTC)
}

// TradeInfo represents a recent trade
type TradeInfo struct {
	ID        string `json:"id"`
	Symbol    string `json:"symbol"`
	Price     int64  `json:"price"`
	Size      int64  `json:"size"`
	Side      string `json:"side"`      // "buy" or "sell"
	Timestamp int64  `json:"timestamp"` // Unix milliseconds
}

// AccountInfo represents account balance and equity
type AccountInfo struct {
	Address           string `json:"address"`
	Balance           int64  `json:"balance"`           // Total balance (USDT cents)
	LockedCollateral  int64  `json:"lockedCollateral"`  // Margin locked in orders
	AvailableBalance  int64  `json:"availableBalance"`  // Available for trading
	UnrealizedPnL     int64  `json:"unrealizedPnL"`     // Unrealized P&L from positions
	TotalEquity       int64  `json:"totalEquity"`       // Balance + UnrealizedPnL
}

// PositionInfo represents an open position
type PositionInfo struct {
	Symbol           string  `json:"symbol"`
	Size             int64   `json:"size"`             // +ve = long, -ve = short
	EntryPrice       int64   `json:"entryPrice"`       // Average entry price
	MarkPrice        int64   `json:"markPrice"`        // Current mark price
	LiquidationPrice int64   `json:"liquidationPrice"` // Price at which liquidation occurs
	UnrealizedPnL    int64   `json:"unrealizedPnl"`    // Current unrealized P&L
	Margin           int64   `json:"margin"`           // Margin committed
	Leverage         float64 `json:"leverage"`         // Effective leverage
}

// OrderInfo represents an order (open or historical)
type OrderInfo struct {
	ID            string `json:"id"`
	Symbol        string `json:"symbol"`
	Side          string `json:"side"`          // "buy" or "sell"
	Type          string `json:"type"`          // "GTC", "IOC", "ALO"
	Price         int64  `json:"price"`
	Size          int64  `json:"size"`
	Filled        int64  `json:"filled"`
	Remaining     int64  `json:"remaining"`
	Status        string `json:"status"`        // "open", "filled", "cancelled", "partially_filled"
	Timestamp     int64  `json:"timestamp"`     // Unix milliseconds
}

// ChainStatus represents consensus layer status
type ChainStatus struct {
	Height        int64   `json:"height"`        // Current block height
	View          int64   `json:"view"`          // Current consensus view
	AvgBlockTime  float64 `json:"avgBlockTime"`  // Average block time (ms)
	MempoolSize   int     `json:"mempoolSize"`   // Pending transactions
	Validators    int     `json:"validators"`    // Active validator count
}

// ==============================
// WebSocket Message Types
// ==============================

// WSMessage is the base structure for all WebSocket messages
type WSMessage struct {
	Type string      `json:"type"` // "orderbook", "trade", "position", "order", "account"
	Data interface{} `json:"data"` // Type-specific payload
}

// WSSubscribeRequest is sent by client to subscribe to channels
type WSSubscribeRequest struct {
	Op       string   `json:"op"`       // "subscribe" or "unsubscribe"
	Channels []string `json:"channels"` // e.g., ["orderbook:BTC-USDT", "trades:BTC-USDT", "account:0x..."]
}

// OrderbookUpdate is broadcast on every block
type OrderbookUpdate struct {
	Type      string       `json:"type"`      // "orderbook"
	Symbol    string       `json:"symbol"`
	Bids      []PriceLevel `json:"bids"`
	Asks      []PriceLevel `json:"asks"`
	Timestamp int64        `json:"timestamp"`
	Height    int64        `json:"height"`
}

// TradeUpdate is broadcast when a trade executes
type TradeUpdate struct {
	Type      string `json:"type"` // "trade"
	Symbol    string `json:"symbol"`
	Price     int64  `json:"price"`
	Size      int64  `json:"size"`
	Side      string `json:"side"`
	Timestamp int64  `json:"timestamp"`
	Height    int64  `json:"height"`
}

// PositionUpdate is broadcast when a position changes
type PositionUpdate struct {
	Type             string `json:"type"` // "position"
	Address          string `json:"address"`
	Symbol           string `json:"symbol"`
	Size             int64  `json:"size"`
	EntryPrice       int64  `json:"entryPrice"`
	MarkPrice        int64  `json:"markPrice"`
	UnrealizedPnL    int64  `json:"unrealizedPnl"`
	LiquidationPrice int64  `json:"liquidationPrice"`
	Margin           int64  `json:"margin"`
	Leverage         float64 `json:"leverage"`
}

// OrderUpdate is broadcast when an order status changes
type OrderUpdate struct {
	Type      string `json:"type"` // "order"
	OrderID   string `json:"orderId"`
	Status    string `json:"status"` // "open" | "filled" | "cancelled" | "partially_filled"
	Filled    int64  `json:"filled"`
	Remaining int64  `json:"remaining"`
}

// ==============================
// REST Request Types
// ==============================

// NOTE: Order submissions now use signed JSON transactions (EIP-712 format).
// See pkg/app/core/transaction/types.go for SignedTransaction structure.
// Legacy SubmitOrderRequest removed - all orders must be cryptographically signed.

// CancelOrderRequest is the payload for POST /api/v1/orders/cancel
// TODO: Replace with signed cancel transaction (Phase 2)
type CancelOrderRequest struct {
	Address   string `json:"address"`   // User's Ethereum address
	OrderID   string `json:"orderId"`   // Order ID to cancel
	Signature string `json:"signature"` // EIP-712 signature (Phase 2)
}

// SubmitOrderResponse is the response from order submission
type SubmitOrderResponse struct {
	Status  string `json:"status"`  // "submitted", "rejected"
	OrderID string `json:"orderId"` // Assigned order ID
	Message string `json:"message,omitempty"` // Error message if rejected
}

// ErrorResponse is returned for all errors
type ErrorResponse struct {
	Error   string `json:"error"`
	Message string `json:"message"`
}
