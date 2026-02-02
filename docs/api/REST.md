# REST API Reference

Base URL: `http://localhost:8080/api/v1`

## Rate Limits

| Endpoint Type | Limit | Description |
|---------------|-------|-------------|
| Trading | 100 req/min | Orders, cancels, deposits, withdrawals |
| Read | 1000 req/min | Orderbook, accounts, market data |
| Heavy | 20 req/min | Sync, snapshots |

Rate limit headers:
- `X-RateLimit-Limit`: Total allowed requests
- `X-RateLimit-Remaining`: Remaining requests in window
- `X-RateLimit-Reset`: Window reset time (Unix timestamp)

---

## Health Check

### `GET /health`

Check if the server is running.

**Rate Limit:** None

**Response:**
```json
{"status": "ok"}
```

---

## Market Endpoints (Read - 1000 req/min)

### `GET /api/v1/markets`

List all available markets.

**Response:**
```json
[
  {
    "symbol": "BTC-USDT",
    "baseAsset": "BTC",
    "quoteAsset": "USDT",
    "type": "perp",
    "status": "active",
    "tickSize": 1,
    "lotSize": 1,
    "maxLeverage": 50,
    "takerFeeBps": 5,
    "makerFeeBps": 2
  }
]
```

### `GET /api/v1/markets/:symbol`

Get single market details.

**Path Parameters:**
- `symbol`: Market symbol (e.g., "BTC-USDT")

### `GET /api/v1/markets/:symbol/orderbook`

Get orderbook snapshot.

**Path Parameters:**
- `symbol`: Market symbol

**Query Parameters:**
- `depth` (optional): Number of levels (default: 20)

**Response:**
```json
{
  "symbol": "BTC-USDT",
  "bids": [[5000000, 10000000]],
  "asks": [[5001000, 5000000]],
  "timestamp": 1706800000000
}
```

Prices are in cents (1 USD = 100). Sizes are in satoshis (1 BTC = 100,000,000).

### `GET /api/v1/markets/:symbol/trades`

Get recent trades.

**Path Parameters:**
- `symbol`: Market symbol

**Query Parameters:**
- `limit` (optional): Number of trades (default: 100, max: 1000)

**Response:**
```json
[
  {
    "id": "1706800000-5000000-100000000-bid",
    "price": 5000000,
    "size": 100000000,
    "side": "bid",
    "timestamp": 1706800000000
  }
]
```

### `GET /api/v1/markets/:symbol/candles`

Get OHLCV candles.

**Path Parameters:**
- `symbol`: Market symbol

**Query Parameters:**
- `interval`: Candle interval (1m, 5m, 15m, 1h, 4h, 1d)
- `from` (optional): Start time (Unix ms)
- `to` (optional): End time (Unix ms)
- `limit` (optional): Number of candles (default: 100, max: 500)

**Response:**
```json
[
  {
    "t": 1706800000000,
    "o": 5000000,
    "h": 5010000,
    "l": 4990000,
    "c": 5005000,
    "v": 100000000,
    "n": 25
  }
]
```

### `GET /api/v1/markets/:symbol/funding`

Get funding rate info.

**Response:**
```json
{
  "symbol": "BTC-USDT",
  "fundingRate": 0.0001,
  "fundingRateBps": 1,
  "nextFundingTime": 1706803600000,
  "lastFundingTime": 1706800000000
}
```

### `GET /api/v1/markets/:symbol/ctx`

Get asset context (market stats like Hyperliquid's activeAssetCtx).

**Response:**
```json
{
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
```

---

## Account Endpoints (Read - 1000 req/min)

### `GET /api/v1/accounts/:address`

Get account info.

**Path Parameters:**
- `address`: Ethereum address (0x...)

**Response:**
```json
{
  "address": "0x1234...",
  "balance": 10000000,
  "lockedCollateral": 5000000,
  "availableBalance": 5000000,
  "unrealizedPnL": 100000,
  "totalEquity": 10100000
}
```

### `GET /api/v1/accounts/:address/positions`

Get account positions.

**Response:**
```json
[
  {
    "symbol": "BTC-USDT",
    "size": 100000000,
    "entryPrice": 5000000,
    "markPrice": 5010000,
    "liquidationPrice": 4000000,
    "unrealizedPnl": 100000,
    "margin": 1000000,
    "leverage": 5.0
  }
]
```

### `GET /api/v1/accounts/:address/orders`

Get open orders.

**Response:**
```json
[
  {
    "id": "abc123",
    "symbol": "BTC-USDT",
    "side": "bid",
    "type": "gtc",
    "price": 4990000,
    "size": 50000000,
    "filled": 0,
    "status": "open",
    "timestamp": 1706800000000
  }
]
```

### `GET /api/v1/accounts/:address/nonce`

Get account nonce for signing transactions.

**Response:**
```json
{"nonce": 5}
```

### `GET /api/v1/accounts/:address/funding`

Get funding payment history.

**Response:**
```json
[
  {
    "symbol": "BTC-USDT",
    "payment": -1000,
    "paymentUsd": -10.0,
    "fundingRate": 1,
    "timestamp": 1706800000000
  }
]
```

### `GET /api/v1/accounts/:address/fills`

Get trade fill history.

**Response:**
```json
[
  {
    "id": "fill123",
    "symbol": "BTC-USDT",
    "side": "bid",
    "price": 5000000,
    "size": 10000000,
    "fee": 250,
    "isMaker": true,
    "timestamp": 1706800000000
  }
]
```

### `GET /api/v1/accounts/:address/trigger-orders`

Get trigger orders (stop-loss/take-profit).

**Response:**
```json
[
  {
    "id": "trigger123",
    "cloid": "client123",
    "symbol": "BTC-USDT",
    "side": "ask",
    "triggerType": "sl",
    "triggerPrice": 4800000,
    "size": 50000000,
    "limitPrice": null,
    "status": "pending",
    "timestamp": 1706800000000
  }
]
```

---

## Order Endpoints (Trading - 100 req/min)

### `POST /api/v1/orders`

Submit a signed order.

**Request Body:**
```json
{
  "type": "order",
  "order": {
    "symbol": "BTC-USDT",
    "side": 0,
    "type": 0,
    "price": "5000000",
    "qty": "100000000",
    "nonce": "1",
    "deadline": "1706900000",
    "leverage": 5,
    "owner": "0x1234...",
    "reduce_only": false
  },
  "signature": "0x...",
  "agent_mode": false,
  "delegation_id": null
}
```

**Side values:** 0 = bid, 1 = ask

**Type values:** 0 = GTC, 1 = IOC, 2 = ALO (Add Liquidity Only)

**Response:**
```json
{
  "status": "ok",
  "orderId": "abc123",
  "message": null
}
```

### `POST /api/v1/orders/cancel`

Cancel an order.

**Request Body:**
```json
{
  "type": "cancel",
  "cancel": {
    "order_id": "abc123",
    "symbol": "BTC-USDT",
    "nonce": "2",
    "owner": "0x1234..."
  },
  "signature": "0x..."
}
```

---

## Trigger Order Endpoints (Trading - 100 req/min)

### `POST /api/v1/trigger-orders`

Place a trigger order (stop-loss or take-profit).

**Request Body:**
```json
{
  "trader": "0x1234...",
  "symbol": "BTC-USDT",
  "triggerType": "sl",
  "triggerPrice": 4800000,
  "size": 50000000,
  "limitPrice": null,
  "cloid": "client123"
}
```

**triggerType values:** "sl" (stop-loss), "tp" (take-profit)

**Response:**
```json
{
  "status": "ok",
  "triggerOrderId": "trigger123"
}
```

### `DELETE /api/v1/trigger-orders/:id`

Cancel a trigger order by ID.

### `POST /api/v1/trigger-orders/cancel`

Cancel a trigger order (by ID or cloid).

**Request Body:**
```json
{
  "trader": "0x1234...",
  "triggerOrderId": "trigger123",
  "symbol": null,
  "cloid": null
}
```

---

## Deposit/Withdraw Endpoints (Trading - 100 req/min)

### `POST /api/v1/deposit`

Deposit collateral (dev mode: simulated, production: bridge verification).

**Request Body:**
```json
{
  "trader": "0x1234...",
  "amount": 10000000
}
```

### `POST /api/v1/withdraw`

Withdraw collateral.

**Request Body:**
```json
{
  "trader": "0x1234...",
  "amount": 5000000
}
```

---

## Agent Delegation Endpoints (Trading - 100 req/min)

### `POST /api/v1/delegations`

Register an agent key delegation.

**Request Body:**
```json
{
  "wallet": "0x1234...",
  "agent": "0x5678...",
  "expiration": "1707000000000",
  "nonce": "1",
  "signature": "0x..."
}
```

**Response:**
```json
{
  "status": "ok",
  "delegationId": "0x1234-1",
  "message": "Delegation registered"
}
```

---

## Chain Status Endpoints (Read - 1000 req/min)

### `GET /api/v1/chain/status`

Get chain status.

**Response:**
```json
{
  "height": 12345,
  "view": 12350,
  "avgBlockTime": 0.1,
  "mempoolSize": 42,
  "validators": 3
}
```

### `GET /api/v1/chain/health`

Get detailed node health status.

**Response:**
```json
{
  "status": "healthy",
  "height": 12345,
  "view": 12350,
  "mempool_size": 42,
  "persistence": true,
  "validators": 3,
  "active_validators": 3,
  "insurance_fund": 100000000,
  "timestamp": 1706800000000
}
```

**Status values:**
- `healthy`: Node is operating normally
- `degraded`: Node has issues (future: will be set when problems detected)

### `GET /api/v1/chain/insurance-fund`

Get insurance fund balance.

**Response:**
```json
{"insuranceFund": 100000000}
```

---

## Staking Endpoints (Read - 1000 req/min)

### `GET /api/v1/staking/validators`

List all validators.

### `GET /api/v1/staking/validators/:operator`

Get single validator info.

### `GET /api/v1/staking/delegations/:address`

Get delegations for an address.

### `GET /api/v1/staking/epoch`

Get current epoch info.

---

## Oracle Endpoints

### `GET /api/v1/oracle/status` (Read)

Get oracle system status.

### `GET /api/v1/oracle/:symbol` (Read)

Get oracle price for a symbol.

### `GET /api/v1/oracle/:symbol/sources` (Read)

Get price sources for a symbol.

### `POST /api/v1/oracle/submit` (Trading)

Submit oracle price update (authorized validators only).

### `POST /api/v1/oracle/enable` (Trading)

Enable/disable oracle system.

---

## ADL Endpoints (Read - 1000 req/min)

### `GET /api/v1/adl/history`

Get auto-deleverage event history.

---

## Sync Endpoints (Heavy - 20 req/min)

### `GET /api/v1/sync/status`

Get sync status for RPC nodes.

**Response:**
```json
{
  "height": 12345,
  "view": 12350,
  "committedHash": "0xabc...",
  "stateHash": "0xdef...",
  "timestamp": 1706800000000,
  "latestSnapshotHeight": 12000,
  "isPersistent": true
}
```

### `GET /api/v1/sync/blocks`

Get block range for sync.

**Query Parameters:**
- `from`: Start height
- `to` (optional): End height
- `limit` (optional): Max blocks (default: 100)
- `includePayload` (optional): Include transaction payload

**Response:**
```json
{
  "blocks": [
    {
      "height": 12345,
      "view": 12350,
      "hash": "0x...",
      "parentHash": "0x...",
      "appHash": "0x...",
      "proposer": "0x...",
      "timestamp": 1706800000000,
      "payloadSize": 1024,
      "payload": "base64...",
      "justify": {
        "view": 12344,
        "blockHash": "0x...",
        "appHash": "0x...",
        "voters": ["0x..."],
        "blsPubkeys": ["0x..."],
        "aggSignature": "0x..."
      }
    }
  ],
  "nextHeight": 12346,
  "totalAvailable": 12345
}
```

### `GET /api/v1/sync/block/:height`

Get single block by height.

### `GET /api/v1/sync/snapshot/latest`

Get latest snapshot metadata.

**Response:**
```json
{
  "height": 12000,
  "timestamp": 1706790000000,
  "stateHash": "0x...",
  "sizeBytes": 1024000,
  "accountCount": 100,
  "marketCount": 1
}
```

### `GET /api/v1/sync/snapshot/:height`

Get snapshot at specific height.

**Response:**
```json
{
  "metadata": { ... },
  "data": "base64..."
}
```

---

## Legacy Endpoints

These endpoints are deprecated but still supported:

- `POST /api/order` → Use `/api/v1/orders`
- `POST /api/deposit` → Use `/api/v1/deposit`
- `POST /api/withdraw` → Use `/api/v1/withdraw`
- `GET /api/orderbook/:symbol` → Use `/api/v1/markets/:symbol/orderbook`
- `GET /api/account/:address` → Use `/api/v1/accounts/:address`

---

## Error Responses

All endpoints return errors in this format:

```json
{
  "error": "Error message",
  "code": "ERROR_CODE"
}
```

Common error codes:
- `RATE_LIMITED`: Too many requests
- `NOT_FOUND`: Resource not found
- `INVALID_REQUEST`: Malformed request
- `INSUFFICIENT_FUNDS`: Not enough balance
- `INVALID_SIGNATURE`: EIP-712 signature verification failed
- `NONCE_MISMATCH`: Transaction nonce already used

---

## Price and Size Units

- **Prices**: In cents (1 USD = 100 cents)
- **Sizes**: In satoshis (1 BTC = 100,000,000 satoshis)
- **Fees**: In basis points (1 bp = 0.01%)
- **Funding rates**: In millionths (1 = 0.0001%)
