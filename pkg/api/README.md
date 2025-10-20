# API Server Implementation

REST + WebSocket API for HyperLicked. Integrated into consensus node for Phase 1.

## Quick Reference

### Start Server
```bash
# From project root
go run cmd/node/main.go

# Server starts on :8080 (configurable via API_ADDR env var)
```

### Test Endpoints
```bash
# Health check
curl http://localhost:8080/health

# Get orderbook
curl http://localhost:8080/api/v1/markets/BTC-USDT/orderbook | jq .

# List markets
curl http://localhost:8080/api/v1/markets | jq .

# Submit order
curl -X POST http://localhost:8080/api/v1/orders \
  -H "Content-Type: application/json" \
  -d '{
    "address": "0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0",
    "symbol": "BTC-USDT",
    "side": "BUY",
    "type": "GTC",
    "price": 5000000,
    "size": 100000000,
    "leverage": 10
  }'
```

## Architecture

```
pkg/api/
├── types.go       # Request/response types, WebSocket messages
├── server.go      # REST handlers, CORS, startup
└── websocket.go   # WebSocket hub, client management, broadcasting
```

**Integration point:** `cmd/node/main.go`
```go
apiServer := api.NewServer(app)
go apiServer.Start(":8080")

// Hook consensus to broadcast on block commit
engine.OnBlockCommit = func(height consensus.Height) {
    apiServer.BroadcastOrderbook("BTC-USDT", int64(height))
}
```

## REST Endpoints

### Query Endpoints
```
GET  /health                          → {"status":"ok"}
GET  /api/v1/markets                  → List all markets
GET  /api/v1/markets/:symbol          → Market info
GET  /api/v1/markets/:symbol/orderbook → Orderbook snapshot
GET  /api/v1/accounts/:address        → Account balances
GET  /api/v1/accounts/:address/positions → Open positions
GET  /api/v1/accounts/:address/orders → Open orders
GET  /api/v1/info                     → Node info (height, mempool size)
```

### Write Endpoints
```
POST /api/v1/orders                   → Submit order
POST /api/v1/orders/cancel            → Cancel order
```

## WebSocket Protocol

### Connect
```javascript
const ws = new WebSocket('ws://localhost:8080/ws')
```

### Subscribe
```javascript
ws.send(JSON.stringify({
  op: 'subscribe',
  channels: ['orderbook:BTC-USDT', 'trades:BTC-USDT']
}))
```

### Unsubscribe
```javascript
ws.send(JSON.stringify({
  op: 'unsubscribe',
  channels: ['orderbook:BTC-USDT']
}))
```

### Receive Updates
```javascript
ws.onmessage = (event) => {
  const data = JSON.parse(event.data)

  switch (data.type) {
    case 'orderbook':
      console.log('Orderbook update:', data.symbol, data.bids, data.asks)
      break
    case 'trade':
      console.log('Trade:', data.symbol, data.side, data.price, data.size)
      break
  }
}
```

## Data Format (CRITICAL!)

### Unit Conversion
**API uses integers only** to avoid floating-point non-determinism.

| Type | Display | API (int64) | Conversion |
|------|---------|-------------|------------|
| **Price (USDT)** | $50,000.00 | 5000000 | `price_cents = dollars * 100` |
| **Size (BTC)** | 1.0 BTC | 100000000 | `size_sats = btc * 100000000` |
| **Leverage** | 10x | 10 | (same) |

**Example:**
```json
{
  "price": 5000000,     // $50,000.00
  "size": 100000000,    // 1.0 BTC
  "leverage": 10        // 10x
}
```

### Request Types

**SubmitOrderRequest** (`types.go:92`)
```go
type SubmitOrderRequest struct {
    Address  string `json:"address"`  // "0x742d..." (Ethereum address format)
    Symbol   string `json:"symbol"`   // "BTC-USDT"
    Side     string `json:"side"`     // "BUY" or "SELL"
    Type     string `json:"type"`     // "GTC", "IOC", "ALO"
    Price    int64  `json:"price"`    // USDT cents
    Size     int64  `json:"size"`     // Satoshis
    Leverage int    `json:"leverage"` // 1-50
}
```

**CancelOrderRequest** (`types.go:102`)
```go
type CancelOrderRequest struct {
    Symbol  string `json:"symbol"`   // "BTC-USDT"
    OrderID string `json:"order_id"` // "0x1234..."
}
```

### Response Types

**OrderbookSnapshot** (`types.go:25`)
```json
{
  "symbol": "BTC-USDT",
  "bids": [
    {"price": 5000000, "size": 100000000},
    {"price": 4999900, "size": 50000000}
  ],
  "asks": [
    {"price": 5000100, "size": 75000000},
    {"price": 5000200, "size": 120000000}
  ],
  "timestamp": 1234567890
}
```

**OrderResponse** (`types.go:111`)
```json
{
  "status": "submitted",
  "order_id": "0x1234567890abcdef",
  "timestamp": 1234567890
}
```

## Broadcasting Flow

**Problem Solved:** Frontend showed "Connecting..." because consensus never triggered broadcasts.

**Solution:**

1. **Consensus Hook** (`pkg/consensus/engine.go:252-255`)
```go
// After 2-chain commit
if e.OnBlockCommit != nil {
    e.OnBlockCommit(e.State.Height)
}
```

2. **Wire in main.go** (`cmd/node/main.go:170-173`)
```go
engine.OnBlockCommit = func(height consensus.Height) {
    apiServer.BroadcastOrderbook("BTC-USDT", int64(height))
}
```

3. **API Server Broadcasts** (`server.go:171-199`)
```go
func (s *Server) BroadcastOrderbook(symbol string, height int64) {
    book := s.app.GetOrderbook(symbol)

    update := OrderbookUpdate{
        Type:      "orderbook",
        Symbol:    symbol,
        Bids:      convertBids(book.GetBidLevels()),
        Asks:      convertAsks(book.GetAskLevels()),
        Height:    height,
        Timestamp: time.Now().Unix(),
    }

    s.hub.BroadcastToChannel("orderbook:"+symbol, update)
}
```

**Timing:** Broadcasts occur every ~100ms (every block commit).

## CORS Configuration

**Development** (`server.go:40-44`)
```go
c := cors.New(cors.Options{
    AllowedOrigins: []string{"http://localhost:3000", "http://localhost:3001"},
    AllowedMethods: []string{"GET", "POST", "OPTIONS"},
    AllowedHeaders: []string{"Content-Type", "Authorization"},
})
```

**Production:** Use reverse proxy (nginx, Caddy) with proper origin whitelist.

## Order Submission Flow

```
1. Frontend: POST /api/v1/orders
   ↓
2. API Server: Validate, generate order ID
   ↓
3. Format transaction: "O:GTC:BTC-USDT:BUY:price=5000000:qty=100000000:id=0x1234:owner=0x742d..."
   ↓
4. Push to mempool: app.PushTx(txBytes)
   ↓
5. Consensus: Include in next block (~100ms)
   ↓
6. App Layer: Parse, validate, match order
   ↓
7. Consensus: Commit block, fire OnBlockCommit
   ↓
8. API Server: BroadcastOrderbook() via WebSocket
   ↓
9. Frontend: Receive update, render UI
```

**Latency:** ~50-200ms from submission to UI update.

## Error Handling

### HTTP Errors
- `400 Bad Request` - Invalid request format
- `404 Not Found` - Symbol/address not found
- `500 Internal Server Error` - Unexpected error

### WebSocket Errors
- `{"error": "invalid operation"}` - Unknown `op` field
- `{"error": "invalid channel format"}` - Bad channel name

### Order Errors
- `"insufficient balance"` - Not enough USDT
- `"invalid price"` - Price outside allowed range
- `"invalid leverage"` - Leverage not in 1-50

## Phase 2 Improvements

### Rate Limiting
```go
// Per IP
readLimiter := rate.NewLimiter(rate.Limit(100.0/60.0), 100) // 100 req/min

// Per address
writeLimiter := rate.NewLimiter(rate.Limit(10.0/60.0), 10) // 10 req/min
```

### Pagination
```go
type PaginationParams struct {
    Limit  int    `json:"limit"`  // Default 50, max 500
    Offset int    `json:"offset"` // Skip N items
    Cursor string `json:"cursor"` // Opaque cursor
}
```

### Transaction Status
```
POST /api/v1/orders → {"tx_id": "0x1234...", "order_id": "0x5678..."}
GET  /api/v1/tx/0x1234 → {"status": "pending|included|committed|failed"}
```

## Testing

### Manual Testing
```bash
# Terminal 1: Start node
go run cmd/node/main.go

# Terminal 2: Test WebSocket
open test-websocket.html  # Browser-based test page
```

### Unit Tests (Planned)
```go
// pkg/api/server_test.go
func TestGetOrderbook(t *testing.T)
func TestSubmitOrder(t *testing.T)
func TestWebSocketBroadcast(t *testing.T)
```

## Debugging

### Enable Verbose Logs
```bash
VERBOSE=true go run cmd/node/main.go
```

### Check WebSocket Clients
```go
// In Hub.Run()
log.Printf("[ws] Active clients: %d", len(h.clients))
```

### Check Broadcast Activity
```go
// In cmd/node/main.go
engine.OnBlockCommit = func(height consensus.Height) {
    log.Printf("[debug] Broadcasting at height %d", height)
    apiServer.BroadcastOrderbook("BTC-USDT", int64(height))
}
```

## Dependencies

```bash
go get github.com/gorilla/mux        # REST routing
go get github.com/gorilla/websocket  # WebSocket
go get github.com/rs/cors            # CORS middleware
```

## See Also

- [CLAUDE.md](../../CLAUDE.md) - Full API architecture docs
- [web/README.md](../../web/README.md) - Frontend integration guide
- [START_TRADING.md](../../START_TRADING.md) - Quick start guide
