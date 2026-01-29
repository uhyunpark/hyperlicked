# State Management

Reference for Zustand stores and state patterns.

## Table of Contents

- [Store Architecture](#store-architecture)
- [WalletStore](#walletstore)
- [TradingStore](#tradingstore)
- [Toast Store](#toast-store)
- [Usage Patterns](#usage-patterns)

---

## Store Architecture

### Two Stores Pattern

| Store | Purpose | Persistence |
|-------|---------|-------------|
| WalletStore | Connection state | LocalStorage (address) |
| TradingStore | Market data, user data | Memory only |

### Why Zustand?

- Minimal boilerplate (no Redux ceremony)
- Direct state mutation in `set()`
- Built-in TypeScript support
- Small bundle size (~2.6kB)

---

## WalletStore

### Interface

```typescript
interface WalletStoreState {
  // Connection
  isConnected: boolean
  address: string | null
  isRabby: boolean
  chainId: number | null

  // Agent keys
  tradingEnabled: boolean
  agentAddress: string | null
  delegationExpiry: string | null

  // UI state
  error: string | null
  needsReconnect: boolean

  // Actions
  setConnected: (address: string, isRabby: boolean, chainId: number) => void
  setDisconnected: () => void
  setTradingEnabled: (enabled: boolean, agentAddress?: string) => void
  setError: (error: string | null) => void
  clearError: () => void
}
```

### Implementation

```typescript
import { create } from 'zustand'

export const useWalletStore = create<WalletStoreState>((set) => ({
  // Initial state
  isConnected: false,
  address: null,
  isRabby: false,
  chainId: null,
  tradingEnabled: false,
  agentAddress: null,
  delegationExpiry: null,
  error: null,
  needsReconnect: false,

  // Actions
  setConnected: (address, isRabby, chainId) => set({
    isConnected: true,
    address,
    isRabby,
    chainId,
    error: null,
  }),

  setDisconnected: () => set({
    isConnected: false,
    address: null,
    tradingEnabled: false,
    agentAddress: null,
    delegationExpiry: null,
  }),

  setTradingEnabled: (enabled, agentAddress) => set({
    tradingEnabled: enabled,
    agentAddress: agentAddress ?? null,
  }),

  setError: (error) => set({ error }),
  clearError: () => set({ error: null }),
}))
```

---

## TradingStore

### Interface

```typescript
interface TradingState {
  // Market data
  orderbook: OrderbookData
  trades: Trade[]
  currentPrice: number

  // User data
  positions: Position[]
  openOrders: Order[]

  // UI state
  selectedSymbol: string
  isConnected: boolean  // WebSocket connection
  balanceRefreshTrigger: number

  // Actions
  updateOrderbook: (data: OrderbookData) => void
  addTrade: (trade: Trade) => void
  setPositions: (positions: Position[]) => void
  setOpenOrders: (orders: Order[]) => void
  setSelectedSymbol: (symbol: string) => void
  setConnected: (connected: boolean) => void
  triggerBalanceRefresh: () => void
  clearUserData: () => void
}
```

### Implementation

```typescript
import { create } from 'zustand'

export const useTradingStore = create<TradingState>((set) => ({
  // Initial state
  orderbook: { bids: [], asks: [] },
  trades: [],
  currentPrice: 0,
  positions: [],
  openOrders: [],
  selectedSymbol: 'BTC-USDT',
  isConnected: false,
  balanceRefreshTrigger: 0,

  // Actions
  updateOrderbook: (data) => set({ orderbook: data }),

  addTrade: (trade) => set((state) => ({
    trades: [trade, ...state.trades].slice(0, 100),  // Keep last 100
    currentPrice: trade.price,
  })),

  setPositions: (positions) => set({ positions }),
  setOpenOrders: (orders) => set({ openOrders: orders }),
  setSelectedSymbol: (symbol) => set({ selectedSymbol: symbol }),
  setConnected: (connected) => set({ isConnected: connected }),

  triggerBalanceRefresh: () => set((state) => ({
    balanceRefreshTrigger: state.balanceRefreshTrigger + 1,
  })),

  clearUserData: () => set({
    positions: [],
    openOrders: [],
  }),
}))
```

---

## Toast Store

### Interface

```typescript
interface Toast {
  id: string
  type: 'success' | 'error' | 'warning' | 'info'
  title: string
  message?: string
}

interface ToastStore {
  toasts: Toast[]
  addToast: (toast: Omit<Toast, 'id'>) => void
  removeToast: (id: string) => void
}
```

### Implementation

```typescript
export const useToastStore = create<ToastStore>((set) => ({
  toasts: [],

  addToast: (toast) => {
    const id = Math.random().toString(36).slice(2)
    set((state) => ({
      toasts: [...state.toasts, { ...toast, id }],
    }))

    // Auto-dismiss after 4 seconds
    setTimeout(() => {
      set((state) => ({
        toasts: state.toasts.filter((t) => t.id !== id),
      }))
    }, 4000)
  },

  removeToast: (id) => set((state) => ({
    toasts: state.toasts.filter((t) => t.id !== id),
  })),
}))

// Convenience methods
export const toast = {
  success: (title: string, message?: string) =>
    useToastStore.getState().addToast({ type: 'success', title, message }),
  error: (title: string, message?: string) =>
    useToastStore.getState().addToast({ type: 'error', title, message }),
  warning: (title: string, message?: string) =>
    useToastStore.getState().addToast({ type: 'warning', title, message }),
  info: (title: string, message?: string) =>
    useToastStore.getState().addToast({ type: 'info', title, message }),
}
```

---

## Usage Patterns

### Reading State

```typescript
// Get single value
const address = useWalletStore(state => state.address)

// Get multiple values
const { isConnected, address } = useWalletStore()

// Get action
const setConnected = useWalletStore(state => state.setConnected)
```

### Updating State

```typescript
// Via action
useWalletStore.getState().setConnected(address, isRabby, chainId)

// Direct set (internal only)
set({ isConnected: true })

// Functional update
set((state) => ({
  trades: [newTrade, ...state.trades].slice(0, 100)
}))
```

### Outside React

```typescript
// Access store outside components
const state = useTradingStore.getState()
console.log('Current price:', state.currentPrice)

// Update from anywhere
useTradingStore.getState().addTrade(trade)
```

### Subscriptions

```typescript
// Subscribe to changes
const unsubscribe = useTradingStore.subscribe(
  (state) => state.currentPrice,
  (price) => console.log('Price changed:', price)
)

// Cleanup
unsubscribe()
```

### Selective Re-renders

```typescript
// Bad - re-renders on any state change
const state = useTradingStore()

// Good - re-renders only when address changes
const address = useWalletStore(state => state.address)

// Good - shallow compare for object
const orderbook = useTradingStore(
  state => state.orderbook,
  shallow
)
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [COMPONENTS.md](COMPONENTS.md) - Using stores in components
- [API.md](API.md) - Updating stores from API
