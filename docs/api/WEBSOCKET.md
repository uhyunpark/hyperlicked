# WebSocket API Reference

Connection URL: `ws://localhost:8080/ws`

## Overview

The WebSocket API provides real-time streaming of:
- **Public channels**: Orderbook updates, trades, blocks, market stats
- **Private channels**: User fills, positions, orders (requires authentication)

## Connection

```javascript
const ws = new WebSocket('ws://localhost:8080/ws');

ws.onopen = () => {
  console.log('Connected');
};

ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  console.log(data);
};
```

---

## Public Channels (No Authentication)

Public events are automatically broadcast to all connected clients.

### OrderbookUpdate

Orderbook changes after each trade.

```json
{
  "orderbook": {
    "symbol": "BTC-USDT",
    "bids": [[5000000, 10000000], [4999000, 5000000]],
    "asks": [[5001000, 8000000], [5002000, 3000000]],
    "timestamp": 1706800000000
  }
}
```

Format: `[[price, size], ...]` where price is in cents and size in satoshis.

### Trade

Trade executed on the exchange.

```json
{
  "Trade": {
    "id": "1706800000-5000000-100000000-bid",
    "symbol": "BTC-USDT",
    "price": 5000000,
    "size": 100000000,
    "side": "bid",
    "timestamp": 1706800000000
  }
}
```

The `id` is deterministic based on trade content for deduplication.

### BlockCommitted

Block finalized by consensus.

```json
{
  "block": {
    "height": 12345,
    "hash": "abc123...",
    "tx_count": 42
  }
}
```

### MarkPriceUpdate

Mark price updated (broadcast every block with trades).

```json
{
  "markPrice": {
    "symbol": "BTC-USDT",
    "mark_price": 5000000,
    "index_price": 4999000,
    "timestamp": 1706800000000
  }
}
```

### AssetCtx

Market statistics (like Hyperliquid's activeAssetCtx). Broadcast every block.

```json
{
  "assetCtx": {
    "symbol": "BTC-USDT",
    "markPrice": 5000000,
    "oraclePrice": 4999000,
    "midPrice": 5000500,
    "fundingRate": 1,
    "premium": 200,
    "openInterest": 1000000000,
    "prevDayPrice": 4950000,
    "dayVolume": 50000000000,
    "dayNotionalVolume": 25000000000,
    "nextFundingTime": 1706803600000,
    "timestamp": 1706800000000
  }
}
```

---

## Private Channels (Authentication Required)

Private channels stream user-specific data. Authentication is required in non-dev mode.

### Subscribing to Private Channels

Send a subscribe message with signature:

```json
{
  "op": "subscribe",
  "address": "0x1234567890abcdef...",
  "signature": "0xabcdef...",
  "timestamp": 1706800000
}
```

**Signed message format**: `"Subscribe to {address} at {timestamp}"`

The message must be signed using EIP-191 personal_sign format:
```
\x19Ethereum Signed Message:\n{length}Subscribe to {address} at {timestamp}
```

#### Agent Key Support

If using a delegated agent key, include the agent address:

```json
{
  "op": "subscribe",
  "address": "0x1234...",
  "signature": "0xabcdef...",
  "timestamp": 1706800000,
  "agent": "0x5678..."
}
```

The system will verify that the agent is delegated for the wallet address.

### Subscription Response

**Success:**
```json
{
  "type": "subscribed",
  "channel": "user",
  "address": "0x1234..."
}
```

**Error:**
```json
{
  "type": "error",
  "code": "AUTH_REQUIRED",
  "message": "User subscription requires signature (wallet or agent key)"
}
```

### Unsubscribing

```json
{
  "op": "unsubscribe",
  "address": "0x1234..."
}
```

---

## Private Event Types

### UserFill

Order filled (partial or full).

```json
{
  "userFill": {
    "symbol": "BTC-USDT",
    "order_id": "abc123",
    "cloid": "client123",
    "side": "bid",
    "price": 5000000,
    "size": 10000000,
    "fee": 250,
    "is_maker": true,
    "timestamp": 1706800000000
  }
}
```

The `cloid` field is included if the order was placed with a client order ID.

### OrderUpdate

Order status changed.

```json
{
  "orderUpdate": {
    "order_id": "abc123",
    "symbol": "BTC-USDT",
    "status": "partial",
    "filled": 5000000,
    "remaining": 5000000,
    "timestamp": 1706800000000
  }
}
```

**Status values:**
- `open`: Order placed, no fills yet
- `partial`: Partially filled
- `filled`: Fully filled
- `cancelled`: Cancelled by user

### OrderClosed

Order fully filled or cancelled (for order history).

```json
{
  "orderClosed": {
    "order_id": "abc123",
    "symbol": "BTC-USDT",
    "side": "bid",
    "price": 5000000,
    "size": 10000000,
    "filled": 10000000,
    "status": "filled",
    "timestamp": 1706800000000
  }
}
```

### PositionUpdate

Position changed after a trade.

```json
{
  "positionUpdate": {
    "symbol": "BTC-USDT",
    "size": 100000000,
    "entry_price": 5000000,
    "mark_price": 5010000,
    "unrealized_pnl": 100000,
    "liquidation_price": 4000000,
    "margin": 1000000,
    "leverage": 5,
    "timestamp": 1706800000000
  }
}
```

Negative `size` indicates a short position.

### BalanceUpdate

Account balance changed.

```json
{
  "balanceUpdate": {
    "balance": 10000000,
    "locked": 5000000,
    "available": 5000000,
    "timestamp": 1706800000000
  }
}
```

### FundingPayment

Funding payment applied to position.

```json
{
  "fundingPayment": {
    "symbol": "BTC-USDT",
    "payment": -1000,
    "funding_rate": 1,
    "timestamp": 1706800000000
  }
}
```

Negative `payment` means you paid funding; positive means you received.

### LiquidationEvent

Position was liquidated.

```json
{
  "liquidation": {
    "symbol": "BTC-USDT",
    "side": "long",
    "size": 100000000,
    "mark_price": 4000000,
    "timestamp": 1706800000000
  }
}
```

### TriggerPlaced

Trigger order was placed.

```json
{
  "triggerPlaced": {
    "id": "trigger123",
    "cloid": "client123",
    "symbol": "BTC-USDT",
    "trigger_type": "sl",
    "trigger_price": 4800000,
    "size": 50000000,
    "timestamp": 1706800000000
  }
}
```

### TriggerTriggered

Trigger order was activated and converted to market order.

```json
{
  "triggerTriggered": {
    "id": "trigger123",
    "symbol": "BTC-USDT",
    "trigger_price": 4800000,
    "mark_price": 4795000,
    "timestamp": 1706800000000
  }
}
```

### TriggerCancelled

Trigger order was cancelled.

```json
{
  "triggerCancelled": {
    "id": "trigger123",
    "symbol": "BTC-USDT",
    "timestamp": 1706800000000
  }
}
```

### ADLEvent

Auto-deleverage occurred.

```json
{
  "adl": {
    "symbol": "BTC-USDT",
    "size": 10000000,
    "price": 5000000,
    "realized_pnl": -50000,
    "is_profit_side": true,
    "timestamp": 1706800000000
  }
}
```

---

## Dev Mode

In dev mode (`MODE=dev`), authentication is not required for private channels. This allows easy testing without signature infrastructure.

```json
{
  "op": "subscribe",
  "address": "0x1234..."
}
```

---

## Reconnection

The WebSocket does not automatically reconnect. Implement reconnection logic in your client:

```javascript
function connect() {
  const ws = new WebSocket('ws://localhost:8080/ws');

  ws.onclose = () => {
    console.log('Disconnected, reconnecting in 1s...');
    setTimeout(connect, 1000);
  };

  ws.onopen = () => {
    // Resubscribe to private channels
    ws.send(JSON.stringify({
      op: 'subscribe',
      address: myAddress,
      signature: mySignature,
      timestamp: Date.now() / 1000
    }));
  };
}
```

---

## Message Flow Example

1. Connect to WebSocket
2. Start receiving public events (orderbook, trades, blocks)
3. Subscribe to private channel with signature
4. Receive confirmation
5. Start receiving user-specific events

```
Client                           Server
   |                                |
   |---- Connect ------------------->|
   |<--- Public events (orderbook) --|
   |<--- Public events (trades) -----|
   |                                |
   |---- Subscribe (with sig) ------>|
   |<--- {"type":"subscribed"} ------|
   |                                |
   |<--- userFill -------------------|
   |<--- positionUpdate -------------|
   |<--- orderUpdate ----------------|
```

---

## Rate Limits

WebSocket connections have no explicit rate limit, but:
- There's a practical limit on how fast you can send subscribe/unsubscribe messages
- The server may close connections that send malformed messages repeatedly
- Public event broadcasts are shared across all clients (efficient)
