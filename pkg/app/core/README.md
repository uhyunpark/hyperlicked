# App Core Layer (Trading Engine)

## Overview

Perpetual futures DEX application logic - orderbook matching, account management, margin enforcement, and liquidations.

**Key Features**:
- Price-time priority orderbook matching
- EVM-compatible account system (0x addresses)
- Margin trading with leverage limits
- Liquidation engine for underwater positions
- Market parameter validation

## Architecture

```
┌───────────────────────────────────────────────────────┐
│                     Mempool                            │
│  Transaction ordering: Non-orders → Cancels → Orders  │
└────────────────┬──────────────────────────────────────┘
                 │
┌────────────────▼──────────────────────────────────────┐
│                 AccountManager                         │
│  - Deposits/Withdrawals                                │
│  - Collateral locking                                  │
│  - Position tracking                                   │
│  - Margin checks (initial/maintenance)                 │
│  - Liquidation engine                                  │
└───────┬──────────────────────────────────────┬────────┘
        │                                      │
┌───────▼─────────┐                   ┌───────▼─────────┐
│ MarketRegistry  │                   │   OrderBook     │
│ - Market params │                   │  - Bids/Asks    │
│ - Tick/Lot size │                   │  - Matching     │
│ - Fee rates     │                   │  - Cancels      │
└─────────────────┘                   └─────────────────┘
```

## Files Structure

```
pkg/app/core/
├── types.go              (18 lines)   - Order, Side enums
├── orderbook.go          (316 lines)  - Price-time matching
├── account.go            (183 lines)  - Account, Position structs
├── account_manager.go    (521 lines)  - Account operations, liquidations
├── market.go             (290 lines)  - Market struct, validation
├── market_registry.go    (153 lines)  - Market registration, lookup
├── market_params.go      (94 lines)   - Fee rates, leverage limits
└── mempool.go            (111 lines)  - Transaction ordering

Total: ~1686 lines
```

## Core Types (`types.go`)

```go
type Side int8
const (
    Buy  Side = 1   // Positive (long direction)
    Sell Side = -1  // Negative (short direction)
)

type Order struct {
    ID       string  // Unique order ID
    Symbol   string  // Market symbol (e.g., "BTC-USDT")
    Side     Side    // Buy or Sell
    Price    int64   // Price in ticks (integer)
    Qty      int64   // Quantity in lots (integer)
    Type     string  // "GTC", "IOC", "ALO"
    OwnerHex string  // EVM address (0x...)
}
```

**Why integer prices/quantities?**
- Avoid floating-point precision errors
- Deterministic arithmetic (critical for consensus)
- Example: tick=1, lot=100 → price 10000 ticks = $100.00

## OrderBook (`orderbook.go`)

### Data Structure

```go
type OrderBook struct {
    mu        sync.Mutex
    bids      map[int64][]*Order  // price → FIFO queue
    asks      map[int64][]*Order  // price → FIFO queue
    index     map[string]struct{}  // order ID existence check
    lastPrice int64                // last fill price (mark price fallback)
}
```

**Performance**:
- **Current**: O(N) scan to find best bid/ask (naive)
- **TODO**: O(log N) heap for price levels (see CLAUDE.md Phase 9)

### Matching Algorithm

**`Add(o *Order) []Fill`**

1. **IOC (Immediate-Or-Cancel)**:
   - Match against opposite side immediately
   - Unfilled quantity discarded (does NOT rest in book)

2. **GTC (Good-Til-Cancel)**:
   - Match against opposite side first
   - Remaining quantity rests in orderbook
   - Price-time priority: earlier orders at same price fill first

3. **ALO (Add-Liquidity-Only)**:
   - Only rests if it DOESN'T cross spread
   - Rejected if it would take liquidity
   - Maker-only order type

**Price-time priority**:
```
Bids (descending):  100 → 99 → 98 ...
Asks (ascending):    98 → 99 → 100 ...

Within same price level: FIFO queue
```

### Operations

- **`Add(o *Order) []Fill`**: Add order, return fills
- **`Cancel(id string) bool`**: Remove resting order
- **`GetLevels(depth int) ([]PriceLevel, []PriceLevel)`**: Orderbook snapshot
- **`GetLastPrice() int64`**: Last fill price (for mark price)

## Account System (`account.go`, `account_manager.go`)

### Account Structure

```go
type Account struct {
    Address common.Address  // EVM 20-byte address (0x...)

    // Balance (in USDC cents: 100 = $1.00)
    USDCBalance      int64  // Total deposited via bridge
    LockedCollateral int64  // Locked for orders + positions

    // Positions
    Positions map[string]*Position  // symbol → position

    // Stats
    RealizedPnL      int64  // Closed position PnL
    TotalFeesPaid    int64  // Taker fees
    TotalFeesEarned  int64  // Maker rebates
    TotalVolume      int64  // Lifetime volume
    TradeCount       int64  // Number of fills
}
```

### Position Structure

```go
type Position struct {
    Symbol     string  // Market symbol
    Size       int64   // +ve = long, -ve = short (in lots)
    EntryPrice int64   // Volume-weighted average entry (in ticks)
    Margin     int64   // Collateral locked (initial margin)
}
```

**Example Position**:
```
Symbol: "BTC-USDT"
Size: 100 lots (= 0.01 BTC if lotSize=100)
EntryPrice: 50000 ticks (= $500.00 if tickSize=1)
Margin: 10000 cents (= $100.00 collateral at 5x leverage)
```

### AccountManager Operations

**Balance Management**:
- **`Deposit(addr, amount)`**: Add USDC (from bridge)
- **`Withdraw(addr, amount)`**: Remove USDC (to bridge)
- **`LockCollateral(addr, amount)`**: Reserve for orders/positions
- **`UnlockCollateral(addr, amount)`**: Release after cancel/close

**Position Management**:
- **`UpdatePosition(addr, symbol, fill)`**: Update position after fill
  - Long fill: increase size, adjust entry price
  - Short fill: decrease size (if closing), calculate realized PnL
  - Entry price: volume-weighted average

**Margin Checks**:
- **`CheckMarginRequirement(addr, symbol, orderSide, orderSize, orderPrice)`**
  - Pre-trade check: can user open this position?
  - Validates: available balance, max leverage, position size limits

- **`CheckLiquidation(addr, symbol, markPrice)`**
  - Post-trade check: is position underwater?
  - Liquidates if: equity < maintenance margin

**Liquidation**:
- **`Liquidate(addr, symbol, markPrice)`**
  - Closes entire position at mark price
  - Releases locked margin
  - Insurance fund absorbs deficit if equity < 0

## Market System (`market.go`, `market_registry.go`, `market_params.go`)

### Market Structure

```go
type Market struct {
    Symbol    string        // "BTC-USDT"
    BaseAsset string        // "BTC"
    QuoteAsset string       // "USDT"
    TickSize  int64         // Minimum price increment (ticks)
    LotSize   int64         // Minimum quantity increment (lots)
    Params    MarketParams  // Fee rates, leverage, limits
}
```

### Market Parameters

```go
type MarketParams struct {
    // Fee rates (in basis points: 100 = 1%)
    MakerFeeRate int64  // Negative = rebate
    TakerFeeRate int64  // Positive = fee

    // Leverage limits
    MaxLeverage        int64  // e.g., 50 = 50x leverage
    MaintenanceMargin  int64  // % of position value required (basis points)
    InitialMargin      int64  // % of position value for new orders

    // Position limits
    MaxPositionSize    int64  // Max size per user (in lots)
    MaxNotionalValue   int64  // Max position value (in USDC cents)

    // Oracle config (future)
    OracleSource string  // "CHAINLINK" or "INTERNAL"
}
```

**Default values** (see `market_params.go:DefaultMarketParams()`):
```
MakerFeeRate: -2 bps (-0.02% rebate)
TakerFeeRate: 5 bps (0.05% fee)
MaxLeverage: 50x
MaintenanceMargin: 200 bps (2%)
InitialMargin: 1000 bps (10%)
MaxPositionSize: 1,000,000 lots
MaxNotionalValue: 10,000,000 cents ($100k)
```

### MarketRegistry

**`RegisterMarket(m *Market)`**: Add new market
**`GetMarket(symbol string)`**: Lookup market by symbol
**`ListMarkets()`**: Get all registered symbols
**`ValidateOrder(symbol, order)`**: Pre-execution validation

## Mempool (`mempool.go`)

### Transaction Ordering

**3-bucket priority**:
1. **Non-orders** (deposits, withdrawals, params) - highest priority
2. **Cancels** - medium priority
3. **Orders** (GTC/IOC/ALO) - lowest priority

**Within each bucket**: FIFO (preserves submission order)

**Why this ordering?**
- Cancels before orders: prevent self-trading
- Non-orders before trades: ensure balance available
- Deterministic: same txs → same execution order

### Operations

- **`PushTx(tx []byte)`**: Add transaction to mempool
- **`PopBatch(max int) [][]byte`**: Get next N transactions for block
- **`Clear()`**: Empty mempool (after block execution)

## Margin & Risk (`account_manager.go`)

### Margin Calculation

**Initial Margin** (for new orders):
```go
requiredMargin = (orderPrice × orderSize) / maxLeverage
```

Example: Open 1 BTC at $50k with 10x leverage
```
requiredMargin = ($50,000 × 1) / 10 = $5,000
```

**Maintenance Margin** (for liquidation):
```go
maintenanceMargin = (markPrice × positionSize) × (maintenanceMarginRate / 10000)
```

Example: 1 BTC position at $50k with 2% maintenance
```
maintenanceMargin = ($50,000 × 1) × 0.02 = $1,000
```

### Liquidation Logic

**Trigger**: `equity < maintenanceMargin`

**Equity calculation**:
```go
equity = USDCBalance + UnrealizedPnL
unrealizedPnL = (markPrice - entryPrice) × size
```

**Example liquidation**:
```
Position: Long 1 BTC @ $50k entry, $45k mark price
Unrealized PnL: ($45k - $50k) × 1 = -$5,000
USDC Balance: $6,000
Equity: $6,000 - $5,000 = $1,000
Maintenance Margin: $45k × 0.02 = $900
Status: $1,000 > $900 → Safe (no liquidation)

If mark price drops to $44k:
Unrealized PnL: -$6,000
Equity: $6,000 - $6,000 = $0
Maintenance Margin: $880
Status: $0 < $880 → LIQUIDATE
```

**Liquidation process**:
1. Close position at mark price
2. Release locked margin
3. Deduct losses from balance
4. If deficit (negative equity): insurance fund absorbs loss

## Fee System (`account_manager.go`)

### Fee Rates

- **Maker**: -0.02% (rebate for providing liquidity)
- **Taker**: +0.05% (fee for taking liquidity)

### Fee Calculation

```go
notionalValue = fillPrice × fillQty
fee = notionalValue × feeRate / 10000
```

**Example**:
```
Fill: 1 BTC @ $50,000
Notional: $50,000

Taker fee: $50,000 × 0.0005 = $25
Maker rebate: $50,000 × -0.0002 = -$10 (earns $10)
```

### Fee Distribution

**Current**: Fees deducted from trader balance
**TODO** (Phase 5):
- Insurance fund: 50% of fees
- Validators: 30% of fees
- Protocol treasury: 20% of fees

## State Hash (`app.go`)

### AppHash Computation

```go
func computeStateHash(height, timestamp int64, books map[string]*OrderBook) Hash {
    h := sha256.New()

    // Height + Timestamp (determinism)
    write(h, height)
    write(h, timestamp)

    // Orderbook state (sorted by symbol)
    for _, symbol := range sorted(symbols) {
        write(h, symbol)

        // Bids: high to low price, FIFO within level
        for _, level := range bids {
            write(h, level.Price)
            write(h, level.Qty)
        }

        // Asks: low to high price, FIFO within level
        for _, level := range asks {
            write(h, level.Price)
            write(h, level.Qty)
        }
    }

    return sha256(h)
}
```

**TODO**: Add account balances, positions, funding rates to hash

## Transaction Format

### Order Transaction
```
Format: O:TYPE:SYMBOL:SIDE:price=X:qty=Y:id=Z
Example: O:GTC:BTC-USDT:BUY:price=50000:qty=100:id=alice_o1
```

### Cancel Transaction
```
Format: C:SYMBOL:ORDER_ID
Example: C:BTC-USDT:alice_o1
```

### Non-Order Transactions
```
N:param-update
D:ADDRESS:AMOUNT    (deposit)
W:ADDRESS:AMOUNT    (withdraw)
```

## Key Invariants

1. **Balance Conservation**:
   ```
   TotalBalance = USDCBalance + LockedCollateral
   ```

2. **Available Balance**:
   ```
   AvailableBalance = USDCBalance - LockedCollateral ≥ 0
   ```

3. **Position Margin**:
   ```
   Equity ≥ MaintenanceMargin (else liquidate)
   ```

4. **Orderbook Integrity**:
   ```
   BestBid < BestAsk (no crossed spread)
   ```

5. **Deterministic Execution**:
   - Same transactions → same state
   - No floating-point
   - Sorted map iteration
   - No system clock (use block timestamp)

## Performance Bottlenecks

**Current**:
- Orderbook: O(N) scan for best bid/ask
- Cancel: O(N) linear search
- Full state rehashing every block

**Optimization Plan** (see CLAUDE.md Phase 9):
1. Heap-based price levels: O(log N) best price
2. Order ID index: O(1) cancel lookup
3. Incremental state hash: Only rehash changed data
4. Parallel matching: Per-symbol goroutines

## Testing

**Unit tests needed**:
- [ ] Orderbook matching (GTC/IOC/ALO)
- [ ] Margin calculation (initial/maintenance)
- [ ] Liquidation trigger conditions
- [ ] Fee calculations (maker/taker)
- [ ] Position PnL (long/short scenarios)

**Integration tests needed**:
- [ ] Multi-user trading scenarios
- [ ] Liquidation cascade (one liquidation triggers another)
- [ ] Mempool ordering determinism
- [ ] State hash consistency across validators

## Future Work

1. **Oracle integration** (Phase 6): Replace lastPrice with real oracle
2. **Funding rates** (Phase 6): 8-hour payment between longs/shorts
3. **Insurance fund** (Phase 5): Track balance, ADL system
4. **Cross-margin** (Phase 5): Share collateral across positions
5. **Rate limiting** (Phase 4): Max orders per user per block
6. **Gas system** (Phase 4): Fee token, free trading

## Dependencies

- `github.com/ethereum/go-ethereum/common`: EVM address type
- Standard library: `sync`, `sort`, `crypto/sha256`

## Key Files to Review

**For orderbook logic**: `orderbook.go` (316 lines)
**For account/margin**: `account_manager.go` (521 lines)
**For market config**: `market.go` + `market_params.go` (384 lines total)
