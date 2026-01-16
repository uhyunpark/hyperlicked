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

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [STATE.md](STATE.md) - Zustand integration
- [STYLING.md](STYLING.md) - Tailwind patterns
