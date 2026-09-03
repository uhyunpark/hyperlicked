# App (Exchange Logic) Reference

## Module Map

| Module | Description |
|--------|-------------|
| `orderbook/` | BTreeMap orderbook, price-time priority matching |
| `staking/` | Validator registration, delegation, epochs, slashing |
| `oracle/` | Multi-source oracle, median aggregation, staleness checks |
| `market_maker/` | Automated market maker for liquidity |
| `state/` | AppState: consensus execution, authenticated schema-v3 state root, and Commitment v2 artifacts |
| `accounts.rs` | Account balances, nonces, margin calculations |
| `positions.rs` | Position tracking, PnL, margin requirements |
| `funding.rs` | Funding rate calculation and payments |
| `liquidation.rs` | Liquidation engine (partial → full) |
| `liquidation_queue.rs` | Priority queue for liquidation processing |
| `adl.rs` | Auto-deleveraging when insurance fund insufficient |
| `mempool.rs` | 3-bucket mempool with priority ordering |
| `candles.rs` | OHLCV candle aggregation |
| `trigger.rs` | TP/SL trigger orders |
| `mod.rs` | Transaction enum, MarketConfig, error types |

## MarketConfig Defaults (BTC-USDT)

| Field | Default | Unit |
|-------|---------|------|
| `symbol` | "BTC-USDT" | — |
| `tick_size` | 100 | cents ($1.00) |
| `lot_size` | 100_000 | satoshis (0.001 BTC) |
| `min_notional` | 1_000 | cents ($10.00) |
| `maker_fee` | 2 | bps (0.02%) |
| `taker_fee` | 5 | bps (0.05%) |
| `funding_interval_ms` | 3_600_000 | ms (1 hour) |
| `interest_rate_bps` | 1 | bps |
| `max_funding_rate_bps` | 100 | bps (1%) |
| `max_order_size` | 1_000_000_000_000 | satoshis (10,000 BTC) |
| `max_position_size` | 10_000_000_000_000 | satoshis (100,000 BTC) |
| `max_open_orders` | 200 | count |
| `max_price_levels` | 1000 | count |
| `ema_alpha_bps` | 1000 | bps (10%) |

## Transaction Enum Variants

| Variant | Mempool Bucket | Description |
|---------|---------------|-------------|
| `Deposit` | 0 (highest) | Deposit funds |
| `Withdraw` | 0 | Withdraw funds |
| `RegisterValidator` | 0 | Register as validator |
| `Delegate` / `Undelegate` | 0 | Stake delegation |
| `ClaimUnstaked` / `ClaimRewards` | 0 | Claim staking rewards |
| `Unjail` / `SubmitEvidence` | 0 | Validator management |
| `OraclePriceUpdate` | 0 | Submit oracle prices |
| `AddMarket` | 0 | Add new market |
| `CancelOrder` | 1 (medium) | Cancel open order |
| `CancelTriggerOrder` / `CancelTriggerOrderByCloid` | 1 | Cancel trigger order |
| `PlaceOrder` | 2 (lower) | Place limit/market order |
| `PlaceTriggerOrder` | 2 | Place TP/SL trigger order |

## Key Constants

| Constant | Value | Location |
|----------|-------|----------|
| `MAINTENANCE_MARGIN_BPS` | 500 (5%) | `state/mod.rs` |
| `MAX_NONCE_GAP` | 10 | `accounts.rs` |
| `INSURANCE_FUND_WARNING_THRESHOLD` | 100_000_000 ($1M) | `state/mod.rs` |

## Insurance Fund

- Receives: liquidation remainders, position profit from liquidated accounts, slashing penalties
- Pays: underwater account losses during liquidation
- Floored at 0 — if insufficient, ADL (auto-deleveraging) triggers against profitable traders
- Warning logged when balance drops below $1M threshold
