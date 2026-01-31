# Component Patterns

Reference for React component structure and patterns.

## Table of Contents

- [Client Directive](#client-directive)
- [Component Structure](#component-structure)
- [Common Patterns](#common-patterns)
- [Conditional Rendering](#conditional-rendering)

---

## Client Directive

All interactive components must use the client directive:

```typescript
'use client'

import { useState } from 'react'

export function MyComponent() {
  const [count, setCount] = useState(0)
  return <button onClick={() => setCount(c => c + 1)}>{count}</button>
}
```

### When to Use

| Use Case | Directive |
|----------|-----------|
| Interactive UI | `'use client'` |
| State management | `'use client'` |
| Event handlers | `'use client'` |
| Browser APIs | `'use client'` |
| Static content | None (server) |
| Metadata | None (server) |

---

## Component Structure

### Standard Template

```typescript
'use client'

import { useState, useCallback, useEffect } from 'react'
import { useWallet } from '@/lib/useWallet'
import { useTradingStore } from '@/lib/store'
import { cn } from '@/lib/utils'
import { toast } from '@/lib/store'

interface MyComponentProps {
  title: string
  onAction?: () => void
}

export function MyComponent({ title, onAction }: MyComponentProps) {
  // 1. Hooks first
  const { address, isConnected } = useWallet()
  const orderbook = useTradingStore(state => state.orderbook)

  // 2. Local state
  const [isLoading, setIsLoading] = useState(false)
  const [data, setData] = useState<Data | null>(null)

  // 3. Callbacks
  const handleClick = useCallback(async () => {
    setIsLoading(true)
    try {
      // Action
      onAction?.()
      toast.success('Success', 'Action completed')
    } catch (error) {
      toast.error('Error', error.message)
    } finally {
      setIsLoading(false)
    }
  }, [onAction])

  // 4. Effects
  useEffect(() => {
    if (isConnected) {
      // Fetch data
    }
  }, [isConnected])

  // 5. Render
  return (
    <div className={cn('flex flex-col', isConnected && 'bg-secondary')}>
      <h2 className="text-lg font-semibold">{title}</h2>
      <button
        onClick={handleClick}
        disabled={isLoading}
        className="btn-primary"
      >
        {isLoading ? 'Loading...' : 'Action'}
      </button>
    </div>
  )
}
```

### File Organization

```
components/
├── trading/              # Domain components
│   ├── Header.tsx
│   ├── TradePanel.tsx
│   ├── Orderbook.tsx
│   ├── OpenOrders.tsx
│   └── Positions.tsx
├── ui/                   # Reusable components
│   ├── Toast.tsx
│   ├── Button.tsx
│   └── Modal.tsx
└── Providers.tsx         # App providers
```

---

## Common Patterns

### Loading State

```typescript
const [isLoading, setIsLoading] = useState(false)

const handleSubmit = useCallback(async () => {
  setIsLoading(true)
  try {
    await submitOrder()
    toast.success('Order Submitted')
  } catch (error) {
    toast.error('Failed', error.message)
  } finally {
    setIsLoading(false)
  }
}, [])

return (
  <button disabled={isLoading}>
    {isLoading ? 'Submitting...' : 'Submit'}
  </button>
)
```

### Data Fetching

```typescript
const [data, setData] = useState<Data | null>(null)
const [error, setError] = useState<string | null>(null)

useEffect(() => {
  const fetchData = async () => {
    try {
      const result = await getAccount(address)
      setData(result)
    } catch (err) {
      setError(err.message)
    }
  }

  if (address) {
    fetchData()
  }
}, [address])
```

### Polling Fallback

```typescript
useEffect(() => {
  // Initial fetch
  fetchData()

  // Poll every 5 seconds as WebSocket fallback
  const interval = setInterval(fetchData, 5000)

  return () => clearInterval(interval)
}, [fetchData])
```

### Form State

```typescript
const [price, setPrice] = useState('')
const [size, setSize] = useState('')
const [orderType, setOrderType] = useState<'gtc' | 'ioc' | 'alo'>('gtc')

const handleSubmit = useCallback(async () => {
  const order = {
    price: parseFloat(price),
    size: parseFloat(size),
    orderType,
  }
  await submitOrder(order)
}, [price, size, orderType])
```

---

## Conditional Rendering

### Connection State

```typescript
if (!isConnected) {
  return (
    <div className="text-center text-muted">
      Connect wallet to view
    </div>
  )
}

return <ConnectedContent />
```

### Empty State

```typescript
if (orders.length === 0) {
  return (
    <div className="text-center text-muted py-4">
      No open orders
    </div>
  )
}

return <OrderList orders={orders} />
```

### Loading State

```typescript
if (isLoading) {
  return <div className="animate-pulse">Loading...</div>
}

return <Content data={data} />
```

### Dev Mode Features

```typescript
import { isDevelopment } from '@/lib/config'

return (
  <div>
    <MainContent />
    {isDevelopment && <FaucetButton />}
  </div>
)
```

### Inline Conditionals

```typescript
<div className={cn(
  'flex items-center',
  isActive && 'bg-accent',
  disabled && 'opacity-50 cursor-not-allowed'
)}>
  {isLoading ? 'Loading...' : 'Ready'}
</div>
```

---

## Props Patterns

### Optional Callbacks

```typescript
interface Props {
  onSubmit?: (data: Data) => void
  onCancel?: () => void
}

// Use optional chaining
onSubmit?.(data)
```

### Children

```typescript
interface Props {
  children: React.ReactNode
}

export function Card({ children }: Props) {
  return <div className="card">{children}</div>
}
```

### Readonly Props

```typescript
export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
  return <html>{children}</html>
}
```

---

## Performance Patterns

### React.memo for High-Frequency Updates

Trading components receive 10+ updates/sec. Use `memo()` to prevent unnecessary re-renders:

```typescript
import { memo, useCallback, useMemo } from 'react'

// Wrap component to prevent parent re-renders from propagating
const OrderbookRow = memo(function OrderbookRow({
  level,
  side,
  maxSize,
  onClick
}: Props) {
  // Stable callback references
  const handleClick = useCallback(() => {
    onClick?.(level.price)
  }, [onClick, level.price])

  return (
    <div onClick={handleClick}>
      {level.price.toFixed(2)}
    </div>
  )
})

// Export memoized component
export const Orderbook = memo(OrderbookInner)
```

### useMemo for Expensive Calculations

```typescript
// Bad: O(N²) - recalculates every render
const bidsWithTotal = orderbook.bids.map((bid, i) => ({
  ...bid,
  total: orderbook.bids.slice(0, i + 1).reduce((sum, b) => sum + b.size, 0)
}))

// Good: O(N) - memoized with single pass
const bidsWithTotal = useMemo(() => {
  let cumulative = 0
  return orderbook.bids.map(bid => {
    cumulative += bid.size
    return { ...bid, total: cumulative }
  })
}, [orderbook.bids])
```

### When to Use memo()

| Component | Use memo? | Why |
|-----------|-----------|-----|
| OrderbookRow | Yes | Renders 30+ times, high frequency |
| TradeRow | Yes | Renders in list, frequent updates |
| Positions | Yes | Prevents parent re-renders |
| OpenOrders | Yes | Prevents parent re-renders |
| TradePanel | No | User input, needs re-renders |
| Header | No | Low update frequency |

### Memoization Checklist

1. **Wrap list item components** with `memo()`
2. **Use `useMemo`** for derived data (totals, max values)
3. **Use `useCallback`** for handlers passed to memoized children
4. **Extract sub-components** when only part needs to update

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [STATE.md](STATE.md) - Zustand integration
- [STYLING.md](STYLING.md) - Tailwind patterns
- [HOOKS.md](HOOKS.md) - Hook patterns including useMemo
