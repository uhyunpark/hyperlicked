# API Reference

Base URL: `/api/v1` (health check at `/health`, WebSocket at `/ws`)

## Rate Limit Tiers

| Tier | Limit | Applies to |
|------|-------|------------|
| Read | 1000 req/min | Market data, account info, staking, oracle |
| Trading | 100 req/min | Orders, cancels, deposits, withdrawals, oracle submit |
| Heavy | 20 req/min | Sync, blocks, snapshots |

## REST Endpoints

### Read (1000/min)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/markets` | All market configs |
| GET | `/markets/:symbol` | Single market config |
| GET | `/markets/:symbol/orderbook` | Orderbook snapshot (bids/asks) |
| GET | `/markets/:symbol/trades` | Recent trades (`?limit=N`) |
| GET | `/markets/:symbol/candles` | OHLCV candles (`?interval=&from=&to=`) |
| GET | `/markets/:symbol/funding` | Funding rate info |
| GET | `/markets/:symbol/ctx` | Asset context (mark, oracle, OI, volume, funding) |
| GET | `/accounts/:address` | Account summary (balance, equity, margin) |
| GET | `/accounts/:address/positions` | Open positions |
| GET | `/accounts/:address/orders` | Open orders |
| GET | `/accounts/:address/nonce` | Current nonce |
| GET | `/accounts/:address/funding` | Funding payment history |
| GET | `/accounts/:address/fills` | Trade fills |
| GET | `/accounts/:address/trigger-orders` | TP/SL trigger orders |
| GET | `/transactions/:tx_hash` | Finalized transaction receipt and events |
| GET | `/chain/status` | Block height, validators, uptime |
| GET | `/chain/health` | Node health check |
| GET | `/chain/insurance-fund` | Insurance fund balance |
| GET | `/staking/validators` | All validators |
| GET | `/staking/validators/:operator` | Single validator |
| GET | `/staking/delegations/:address` | Delegations for address |
| GET | `/staking/unstakes/:address` | Pending unstakes |
| GET | `/staking/summary/:address` | Staking summary |
| GET | `/staking/epoch` | Current epoch info |
| GET | `/oracle/status` | Oracle system status |
| GET | `/oracle/:symbol` | Oracle price for symbol |
| GET | `/oracle/:symbol/sources` | Oracle price sources |
| GET | `/adl/history` | ADL event history |

### Trading (100/min) — EIP-712 signed

| Method | Path | Description |
|--------|------|-------------|
| POST | `/orders` | Place order (EIP-712 signed) |
| POST | `/orders/cancel` | Cancel order (EIP-712 signed) |
| POST | `/trigger-orders` | Place TP/SL trigger order |
| POST | `/trigger-orders/cancel` | Cancel trigger order |
| POST | `/delegations` | Register agent delegation |
| POST | `/deposit` | Deposit funds |
| POST | `/withdraw` | Withdraw funds (EIP-712 signed) |
| POST | `/oracle/submit` | Submit oracle price update |
| POST | `/oracle/enable` | Enable/disable oracle |
| POST | `/admin/add-market` | Add new market |

### Heavy (20/min)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/sync/status` | Sync status (height, hash) |
| GET | `/sync/blocks` | Blocks from height (`?from=&limit=`) |
| GET | `/sync/block/:height` | Single block by height |
| GET | `/sync/snapshot/latest` | Latest snapshot |
| GET | `/sync/snapshot/:height` | Snapshot at height |

### Legacy (deprecated)

| Method | Path | Maps to |
|--------|------|---------|
| POST | `/api/order` | → `/api/v1/orders` |
| POST | `/api/deposit` | → `/api/v1/deposit` |
| POST | `/api/withdraw` | → `/api/v1/withdraw` |
| GET | `/api/orderbook/:symbol` | → `/api/v1/markets/:symbol/orderbook` |
| GET | `/api/account/:address` | → `/api/v1/accounts/:address` |

## Response Conventions

- **Prices**: i64 cents (1 USD = 100)
- **Sizes**: i64 satoshis (1 unit = 1e8)
- **Timestamps**: u64 milliseconds since epoch
- **Errors**: `{ "error": "message" }` with appropriate HTTP status
- **Submission responses**: `{ "status": "pending", "tx_hash": "<64 lowercase hex characters>" }`; the hash is the canonical signed-envelope transaction hash used for receipt lookup.
- **Finalized transaction responses**: `GET /transactions/:tx_hash` returns `{ "status": "finalized", "tx_hash": ..., "tx_index": ..., "tx_type": ..., "receipt_status": ..., "error_code": ..., "resource_usage": ..., "events": [...], "block": { "hash": ..., "height": ... } }`; unfinalized/missing transactions return `404`.

## WebSocket

Connect to `/ws`. No rate limit (has own auth).

### Public Events (broadcast to all)

| Event type | Key fields |
|------------|------------|
| `orderbook` | symbol, bids, asks, timestamp |
| `trade` | id, symbol, price, size, side, timestamp |
| `block` | height, hash, tx_count |
| `markPrice` | symbol, mark_price, index_price, timestamp |
| `assetCtx` | symbol, markPrice, oraclePrice, midPrice, fundingRate, premium, openInterest, dayVolume, dayNotionalVolume, nextFundingTime |

### Private Events (after subscribe)

Subscribe: `{ "op": "subscribe", "address": "0x...", "signature": "0x...", "timestamp": N }`
Signature message: `"Subscribe to {address} at {timestamp}"`

| Event type | Key fields |
|------------|------------|
| `userFill` | symbol, order_id, cloid?, side, price, size, fee, is_maker |
| `orderUpdate` | order_id, symbol, status, filled_size |
| `orderClosed` | order_id, symbol, reason |
| `positionUpdate` | symbol, size, entry_price, unrealized_pnl, margin |
| `balanceUpdate` | available, total, margin_used |
| `adl` | symbol, size, price, pnl |
| `fundingPayment` | symbol, payment, rate |
| `liquidated` | symbol, size, price |
| `triggerOrderPlaced` | trigger_id, symbol, side, size, trigger_price |
| `triggerOrderTriggered` | trigger_id, symbol |
| `triggerOrderCancelled` | trigger_id, symbol |

## EIP-712 Signed Requests

Trading endpoints require EIP-712 signatures. The request includes the action payload plus `signature` and `nonce` fields. In dev mode (`DEV_MODE=true`), signature verification is bypassed. Agent delegations allow a delegated key to sign on behalf of the trader.
