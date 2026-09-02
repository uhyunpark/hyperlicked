# Chart data architecture

This document describes the chart implementation that exists in the current Rust `hl-node`
runtime. It is not a future Go implementation plan.

## Current data flow

```text
signed order
    -> hl-node mempool
    -> HotStuff-2 block execution and finalization
    -> canonical AppState trade/candle state
    -> REST candle history + WebSocket trade event
    -> Next.js Chart
```

The chart does not generate its own random candle history. On mount, symbol change, interval
change, or WebSocket reconnect, `web/components/trading/Chart.tsx` requests a bounded committed
window from:

```http
GET /api/v1/markets/:symbol/candles?interval=1m&limit=2000
```

It then applies newly finalized `trade` WebSocket events to the current candle through the
frontend `CandlestickAggregator`.

Relevant implementation files:

- `web/components/trading/Chart.tsx` — REST loading, reconnect refresh, and live updates
- `web/lib/candlestickAggregator.ts` — bounded frontend OHLC aggregation
- `src/api/routes/market.rs` — candle and trade REST endpoints
- `src/app/candles.rs` — deterministic integer candle aggregation
- `src/app/state/execution.rs` — records executed fills in trade and candle state
- `src/api/websocket.rs` — WebSocket trade messages

## Timestamp and numeric rules

Trades use the timestamp established by deterministic block execution. All validators therefore
derive the same candle buckets from the same finalized order execution.

Consensus and application state use integer units:

- price: integer cents (`5_000_000` = `$50,000.00`)
- size and volume: integer base units (`100_000_000` = `1 BTC` in the current UI convention)
- time: Unix milliseconds

The frontend converts integers only at the display boundary. Floating-point chart values are UI
representations and are not consensus state.

Supported intervals are `1m`, `5m`, `15m`, `1h`, `4h`, and `1d`. The node retains at most 10,000
candles per symbol and interval; the current chart requests at most 2,000 at a time.

## Local verification

Terminal 1, from the repository root:

```bash
./scripts/local-node
```

Terminal 2:

```bash
cd web
bun install
bun run dev
```

Open `http://localhost:3000`. If that port is occupied, Next.js prints the alternate port it
selected.

To produce chart data:

1. Connect Rabby or MetaMask.
2. Use the development-only `Get Test USDC ($100k)` action.
3. Submit crossing buy and sell orders from suitable test accounts.
4. Inspect `/api/v1/markets/BTC-USDT/candles?interval=1m&limit=100` or the browser WebSocket
   messages.

The faucet is simulated local collateral. It does not mint HYCK, send ETH, or exercise a bridge.

## Recovery behavior

`hl-node` replays finalized blocks into the canonical application during startup. Candle and trade
state are rebuilt through that same deterministic execution path rather than trusted from an
unauthenticated frontend cache. After a WebSocket reconnect, the frontend fetches REST candles
again before applying new live trades.

## Remaining production work

The node's bounded candle history is sufficient for the local showcase and recent charts. It is
not a long-range market-data archive. A production deployment still needs an external indexer fed
from finalized transaction/event artifacts, plus a bounded historical query API, retention policy,
and operational monitoring.

Until that work exists, do not describe the current candle endpoint as unlimited historical data
or as a production indexer.
