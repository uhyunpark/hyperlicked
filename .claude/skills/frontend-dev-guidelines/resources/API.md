# API Integration

Reference for REST client and WebSocket patterns.

## Table of Contents

- [REST Client](#rest-client)
- [Unit Conversion](#unit-conversion)
- [WebSocket Integration](#websocket-integration)
- [Error Handling](#error-handling)

---

## REST Client

### Structure

```typescript
// lib/api.ts
const API_BASE = process.env.NEXT_PUBLIC_API_URL || 'http://localhost:8080/api/v1'

export async function getAccount(address: string): Promise<Account> {
  const res = await fetch(`${API_BASE}/accounts/${address}`)
  if (!res.ok) throw new Error(`Failed to fetch account: ${res.statusText}`)
  return res.json()
}
```

### Common Endpoints

```typescript
// Markets
export async function getMarkets(): Promise<Market[]>
export async function getOrderbook(symbol: string): Promise<Orderbook>
export async function getTrades(symbol: string): Promise<Trade[]>

// Account
export async function getAccount(address: string): Promise<Account>
export async function getPositions(address: string): Promise<Position[]>
export async function getOpenOrders(address: string): Promise<Order[]>
export async function getOrderHistory(address: string): Promise<Order[]>

// Trading
export async function submitOrder(order: SignedOrder): Promise<OrderResult>
export async function cancelOrder(cancel: SignedCancel): Promise<CancelResult>

// Info
export async function getFunding(symbol: string): Promise<FundingInfo>
export async function getInsuranceFund(): Promise<InsuranceFundInfo>
```

### Request Pattern

```typescript
export async function submitOrder(order: SignedOrder): Promise<OrderResult> {
  const res = await fetch(`${API_BASE}/orders`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(order),
  })

  if (!res.ok) {
    const error = await res.json().catch(() => ({ message: res.statusText }))
    throw new Error(error.message || 'Failed to submit order')
  }

  return res.json()
}
```

---

## Unit Conversion

### API ↔ Frontend

```typescript
// lib/api.ts

// API uses integers for determinism
// Frontend displays user-friendly units

// Price: cents → dollars
export const convertPrice = (cents: number): number => cents / 100
export const convertToApiPrice = (dollars: number): number =>
  Math.round(dollars * 100)

// Size: satoshis → BTC
export const convertSize = (sats: number): number => sats / 100_000_000
export const convertToApiSize = (btc: number): number =>
  Math.round(btc * 100_000_000)
```

### Usage

```typescript
// Reading from API
const position = await getPosition(address, symbol)
const displaySize = convertSize(position.size)  // e.g., 1.5 BTC
const displayPrice = convertPrice(position.entry_price)  // e.g., 50000.00

// Sending to API
const order = {
  price: convertToApiPrice(priceInput),  // User types 50000 → 5000000
  size: convertToApiSize(sizeInput),      // User types 1.5 → 150000000
}
```

### Formatting

```typescript
// lib/utils.ts

export function formatPrice(dollars: number): string {
  return dollars.toLocaleString('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })
}

export function formatSize(btc: number): string {
  return btc.toLocaleString('en-US', {
    minimumFractionDigits: 4,
    maximumFractionDigits: 8,
  })
}

export function formatUsd(cents: number): string {
  return formatPrice(convertPrice(cents))
}
```

---

## WebSocket Integration

### Connection

```typescript
// lib/useWebSocket.ts

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null)
  const reconnectTimeoutRef = useRef<NodeJS.Timeout>()

  useEffect(() => {
    const connect = () => {
      const ws = new WebSocket(config.api.wsUrl)
      wsRef.current = ws

      ws.onopen = () => {
        console.log('[ws] Connected')
        useTradingStore.getState().setConnected(true)
        subscribeToMarkets(ws)
      }

      ws.onmessage = handleMessage

      ws.onclose = () => {
        console.log('[ws] Disconnected')
        useTradingStore.getState().setConnected(false)
        // Reconnect after 3 seconds
        reconnectTimeoutRef.current = setTimeout(connect, 3000)
      }

      ws.onerror = (error) => {
        console.error('[ws] Error:', error)
      }
    }

    connect()

    return () => {
      if (reconnectTimeoutRef.current) {
        clearTimeout(reconnectTimeoutRef.current)
      }
      wsRef.current?.close()
    }
  }, [])

  return wsRef.current
}
```

### Message Handling

```typescript
const handleMessage = (event: MessageEvent) => {
  const data = JSON.parse(event.data)
  const store = useTradingStore.getState()

  switch (data.type) {
    case 'orderbook':
      store.updateOrderbook({
        bids: data.bids.map(([price, size]) => ({
          price: convertPrice(price),
          size: convertSize(size),
        })),
        asks: data.asks.map(([price, size]) => ({
          price: convertPrice(price),
          size: convertSize(size),
        })),
      })
      break

    case 'trade':
      store.addTrade({
        id: data.id,
        price: convertPrice(data.price),
        size: convertSize(data.size),
        side: data.side,
        timestamp: data.timestamp,
      })
      break

    case 'userFill':
      toast.info('Order Filled', `${formatSize(convertSize(data.size))} @ ${formatPrice(convertPrice(data.price))}`)
      store.triggerBalanceRefresh()
      break

    case 'orderUpdate':
      // Refresh orders
      break
  }
}
```

### Subscriptions

```typescript
function subscribeToMarkets(ws: WebSocket) {
  const symbol = useTradingStore.getState().selectedSymbol

  // Public channels
  ws.send(JSON.stringify({
    type: 'subscribe',
    channels: [`orderbook:${symbol}`, `trades:${symbol}`],
  }))
}

function subscribeToUser(ws: WebSocket, address: string) {
  ws.send(JSON.stringify({
    type: 'subscribe',
    channels: [`user:${address}`],
  }))
}
```

---

## Error Handling

### API Errors

```typescript
try {
  const result = await submitOrder(order)
  toast.success('Order Submitted')
} catch (error) {
  toast.error('Order Failed', error.message)
}
```

### Graceful Degradation

```typescript
// Fetch with fallback
const [data, setData] = useState<Data | null>(null)

useEffect(() => {
  const fetch = async () => {
    try {
      const result = await getData()
      setData(result)
    } catch (error) {
      // Silent fail for non-critical data
      console.warn('Failed to fetch data:', error)
    }
  }
  fetch()
}, [])
```

### Polling Fallback

```typescript
// When WebSocket is down, poll REST API
useEffect(() => {
  if (isWsConnected) return  // Skip if WS working

  const poll = async () => {
    try {
      const orderbook = await getOrderbook(symbol)
      store.updateOrderbook(orderbook)
    } catch (error) {
      // Silent fail
    }
  }

  const interval = setInterval(poll, 2000)
  return () => clearInterval(interval)
}, [isWsConnected, symbol])
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [STATE.md](STATE.md) - Store updates
- [WALLET.md](WALLET.md) - Signed requests
