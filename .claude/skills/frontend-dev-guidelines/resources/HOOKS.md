# React Hook Patterns

Reference for useCallback, useEffect, useMemo, and useRef patterns.

## Table of Contents

- [useMemo](#usememo)
- [useCallback](#usecallback)
- [useEffect](#useeffect)
- [useRef](#useref)
- [Custom Hooks](#custom-hooks)

---

## useMemo

### When to Use

- Expensive calculations (O(N) or higher)
- Derived data from props/state
- Reference equality for memo() children
- Data transformations on high-frequency updates

### Expensive Calculations

```typescript
// Bad: O(N²) runs on every render
const bidsWithTotal = orderbook.bids.map((bid, i) => ({
  ...bid,
  total: orderbook.bids.slice(0, i + 1).reduce((sum, b) => sum + b.size, 0)
}))

// Good: O(N) single pass, only recalculates when bids change
const bidsWithTotal = useMemo(() => {
  let cumulative = 0
  return orderbook.bids.map(bid => {
    cumulative += bid.size
    return { ...bid, total: cumulative }
  })
}, [orderbook.bids])
```

### Derived Values

```typescript
// Max value for visualization
const maxSize = useMemo(() => {
  const maxBid = Math.max(...bids.map(b => b.size), 1)
  const maxAsk = Math.max(...asks.map(a => a.size), 1)
  return Math.max(maxBid, maxAsk)
}, [bids, asks])

// Spread calculation
const spreadInfo = useMemo(() => {
  const bestBid = bids[0]?.price ?? null
  const bestAsk = asks[0]?.price ?? null
  const hasSpread = bestBid !== null && bestAsk !== null
  const spread = hasSpread ? bestAsk - bestBid : 0
  return { bestBid, bestAsk, hasSpread, spread }
}, [bids, asks])
```

### Grouping Data

```typescript
// Group trigger orders by symbol
const triggersBySymbol = useMemo(() => {
  const bySymbol: Record<string, Triggers> = {}
  for (const trigger of triggerOrders) {
    if (trigger.status !== 'pending') continue
    if (!bySymbol[trigger.symbol]) {
      bySymbol[trigger.symbol] = {}
    }
    bySymbol[trigger.symbol][trigger.triggerType] = trigger
  }
  return bySymbol
}, [triggerOrders])
```

### When NOT to Use

```typescript
// Don't memoize simple values
const isLong = position.size > 0  // No useMemo needed

// Don't memoize if deps change frequently
const formatted = useMemo(() => price.toFixed(2), [price])  // Overkill
```

---

## useCallback

### When to Use

- Async operations (API calls)
- Event handlers passed to children
- Callbacks used as effect dependencies

### Basic Pattern

```typescript
const handleSubmit = useCallback(async () => {
  setIsLoading(true)
  try {
    await submitOrder(order)
    toast.success('Order submitted')
  } catch (error) {
    toast.error('Failed', error.message)
  } finally {
    setIsLoading(false)
  }
}, [order])  // Dependencies
```

### With Dependencies

```typescript
// Good: all used variables in deps
const fetchData = useCallback(async () => {
  const data = await getAccount(address)
  setAccount(data)
}, [address])

// Bad: missing dependency
const fetchData = useCallback(async () => {
  const data = await getAccount(address)  // address used but not in deps
  setAccount(data)
}, [])  // Will use stale address!
```

### Event Handlers

```typescript
// Without deps - stable reference
const handleClick = useCallback(() => {
  console.log('clicked')
}, [])

// With state - updates when state changes
const handleClick = useCallback(() => {
  console.log('count:', count)
}, [count])
```

---

## useEffect

### One-Time Effects

```typescript
// Runs once on mount
useEffect(() => {
  autoConnect()
}, [])  // Empty deps
```

### Dependency-Based

```typescript
// Runs when address changes
useEffect(() => {
  if (address) {
    fetchAccount(address)
  }
}, [address])
```

### With Cleanup

```typescript
useEffect(() => {
  const ws = new WebSocket(url)

  ws.onmessage = handleMessage

  // Cleanup function
  return () => {
    ws.close()
  }
}, [url])
```

### Interval Pattern

```typescript
useEffect(() => {
  // Initial fetch
  fetchData()

  // Set up interval
  const interval = setInterval(fetchData, 5000)

  // Cleanup
  return () => clearInterval(interval)
}, [fetchData])
```

### Event Listeners

```typescript
useEffect(() => {
  const handleResize = () => {
    setWidth(window.innerWidth)
  }

  window.addEventListener('resize', handleResize)

  return () => {
    window.removeEventListener('resize', handleResize)
  }
}, [])
```

### Conditional Effects

```typescript
useEffect(() => {
  // Skip if not connected
  if (!isConnected) return

  fetchUserData()
}, [isConnected])
```

---

## useRef

### DOM References

```typescript
const inputRef = useRef<HTMLInputElement>(null)

const focusInput = () => {
  inputRef.current?.focus()
}

return <input ref={inputRef} />
```

### Mutable Values

```typescript
// Track WebSocket
const wsRef = useRef<WebSocket | null>(null)

useEffect(() => {
  wsRef.current = new WebSocket(url)
  return () => wsRef.current?.close()
}, [url])

// Access current value
const sendMessage = () => {
  wsRef.current?.send(message)
}
```

### Timeout/Interval IDs

```typescript
const timeoutRef = useRef<NodeJS.Timeout>()

const debouncedSearch = (query: string) => {
  // Clear previous timeout
  if (timeoutRef.current) {
    clearTimeout(timeoutRef.current)
  }

  // Set new timeout
  timeoutRef.current = setTimeout(() => {
    search(query)
  }, 300)
}

// Cleanup on unmount
useEffect(() => {
  return () => {
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current)
    }
  }
}, [])
```

### Sync Ref with State

```typescript
// Keep ref in sync with state for callbacks
const agentWalletRef = useRef(agentWallet)

useEffect(() => {
  agentWalletRef.current = agentWallet
}, [agentWallet])

// Use in callback (avoids stale closure)
const signOrder = useCallback(async (order) => {
  const wallet = agentWalletRef.current
  if (wallet) {
    return wallet.signTypedData(...)
  }
}, [])  // No deps needed - uses ref
```

### Previous Value

```typescript
const usePrevious = <T>(value: T): T | undefined => {
  const ref = useRef<T>()

  useEffect(() => {
    ref.current = value
  }, [value])

  return ref.current
}

// Usage
const prevAddress = usePrevious(address)

useEffect(() => {
  if (prevAddress && prevAddress !== address) {
    // Address changed
    clearUserData()
  }
}, [address, prevAddress])
```

---

## Custom Hooks

### Pattern

```typescript
function useMyHook(param: string) {
  const [state, setState] = useState(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    setLoading(true)
    try {
      const data = await fetchData(param)
      setState(data)
    } catch (err) {
      setError(err.message)
    } finally {
      setLoading(false)
    }
  }, [param])

  useEffect(() => {
    refresh()
  }, [refresh])

  return { state, loading, error, refresh }
}
```

### useDebounce

```typescript
function useDebounce<T>(value: T, delay: number): T {
  const [debouncedValue, setDebouncedValue] = useState(value)

  useEffect(() => {
    const timer = setTimeout(() => {
      setDebouncedValue(value)
    }, delay)

    return () => clearTimeout(timer)
  }, [value, delay])

  return debouncedValue
}

// Usage
const debouncedSearch = useDebounce(searchTerm, 300)
```

### useLocalStorage

```typescript
function useLocalStorage<T>(key: string, initialValue: T) {
  const [value, setValue] = useState<T>(() => {
    if (typeof window === 'undefined') return initialValue

    const stored = localStorage.getItem(key)
    return stored ? JSON.parse(stored) : initialValue
  })

  useEffect(() => {
    localStorage.setItem(key, JSON.stringify(value))
  }, [key, value])

  return [value, setValue] as const
}
```

---

## Common Pitfalls

### Stale Closures

```typescript
// Bad: uses stale count
const handleClick = useCallback(() => {
  setCount(count + 1)
}, [])  // Missing count dependency

// Good: functional update
const handleClick = useCallback(() => {
  setCount(c => c + 1)
}, [])
```

### Infinite Loops

```typescript
// Bad: object in deps recreated every render
useEffect(() => {
  fetch(options)
}, [{ url, method }])  // New object every time!

// Good: primitive deps
useEffect(() => {
  fetch({ url, method })
}, [url, method])
```

### Missing Cleanup

```typescript
// Bad: no cleanup
useEffect(() => {
  const interval = setInterval(tick, 1000)
}, [])  // Interval never cleared!

// Good: cleanup
useEffect(() => {
  const interval = setInterval(tick, 1000)
  return () => clearInterval(interval)
}, [])
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [COMPONENTS.md](COMPONENTS.md) - Using hooks in components
- [STATE.md](STATE.md) - Store hooks
