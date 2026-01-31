---
name: frontend-dev-guidelines
description: Next.js 15 / Tailwind / Zustand frontend development patterns for hyperlicked trading UI. Covers component patterns, state management with Zustand stores, Tailwind styling, WebSocket integration, wallet connection with EIP-712 signing, agent keys, useCallback/useEffect patterns, and API integration. Use when working on components, pages, hooks, wallet, toast, or any web/*.tsx files.
---

# Frontend Development Guidelines

## Purpose

Comprehensive patterns and conventions for the hyperlicked Next.js frontend. This is a trading UI built with Next.js 15, Tailwind CSS, and Zustand for state management.

## When to Use This Skill

Automatically activates when you mention:
- Components, pages, hooks
- Zustand stores, state management
- Tailwind styling, theme colors
- WebSocket integration
- Wallet connection, EIP-712
- Agent keys, signing
- Trading UI, orderbook UI
- Toast notifications

---

## Project Structure

```
web/
├── app/                    # Next.js App Router
│   ├── layout.tsx          # Root layout with metadata
│   ├── page.tsx            # Main trading page
│   └── globals.css         # Tailwind + custom theme
├── components/
│   ├── Providers.tsx       # App-wide providers
│   ├── trading/            # Trading UI components
│   │   ├── Header.tsx
│   │   ├── TradePanel.tsx
│   │   ├── Orderbook.tsx
│   │   ├── OpenOrders.tsx
│   │   └── Positions.tsx
│   └── ui/                 # Reusable UI components
│       └── Toast.tsx
├── lib/                    # Utilities and hooks
│   ├── store.ts            # Zustand stores
│   ├── types.ts            # Type definitions
│   ├── api.ts              # REST API client
│   ├── useWebSocket.ts     # WebSocket hook
│   ├── useWallet.ts        # Wallet integration hook
│   ├── config.ts           # Environment config
│   └── utils.ts            # Formatting utilities
└── public/                 # Static assets
```

---

## Core Principles

### 1. Client Components

All interactive components start with:
```typescript
'use client'
```

### 2. Two Zustand Stores

**WalletStore** - Connection state (serializable)
```typescript
interface WalletStoreState {
  isConnected: boolean
  address: string | null
  tradingEnabled: boolean        // Agent key enabled
  agentAddress: string | null
}
```

**TradingStore** - Market & user data (real-time)
```typescript
interface TradingState {
  orderbook: OrderbookData
  trades: Trade[]
  positions: Position[]
  openOrders: Order[]
  selectedSymbol: string
  isConnected: boolean           // WebSocket status
}
```

### 3. Unit Conversion

API uses integers; frontend uses user-friendly units:
```typescript
export const convertPrice = (cents: number): number => cents / 100
export const convertSize = (sats: number): number => sats / 100_000_000
export const convertToApiPrice = (dollars: number): number => Math.round(dollars * 100)
export const convertToApiSize = (btc: number): number => Math.round(btc * 100_000_000)
```

---

## Key Patterns

### Component Structure

```typescript
'use client'

import { useWallet } from '@/lib/useWallet'
import { useTradingStore } from '@/lib/store'
import { cn } from '@/lib/utils'

export function MyComponent() {
  const { address, isConnected } = useWallet()
  const orderbook = useTradingStore(state => state.orderbook)

  // Local state for form fields
  const [price, setPrice] = useState('')

  // Callbacks with dependencies
  const handleSubmit = useCallback(async () => {
    // ...
  }, [address, price])

  return (
    <div className={cn('flex flex-col', isConnected && 'bg-secondary')}>
      {/* ... */}
    </div>
  )
}
```

### Tailwind Styling

```typescript
// Theme colors
className="bg-primary"      // #0d0d14 (darkest)
className="bg-secondary"    // #1a1a24
className="bg-tertiary"     // lighter
className="text-green-buy"  // green for buys
className="text-red-sell"   // red for sells
className="border-border"   // #2d2d3d

// Utility function
import { cn } from '@/lib/utils'

cn('flex items-center', isActive && 'bg-accent', disabled && 'opacity-50')
```

### WebSocket Integration

```typescript
const wsRef = useRef<WebSocket | null>(null)

useEffect(() => {
  const ws = new WebSocket(config.api.wsUrl)
  wsRef.current = ws

  ws.onmessage = (event) => {
    const data = JSON.parse(event.data)
    useTradingStore.getState().updateOrderbook(data)
  }

  return () => ws.close()
}, [])
```

### Wallet Signing

```typescript
// Smart signing: agent key first, fallback to MetaMask
const signature = await wallet.signOrderSmart(order)

// Direct EIP-712 signing
const signature = await signer.signTypedData(
  EIP712_DOMAIN,
  EIP712_ORDER_TYPES,
  order
)
```

---

## Quick Reference

### Performance Patterns (High-Frequency Updates)
- `memo()` wrap components receiving 10+ updates/sec
- `useMemo` for expensive calculations (O(N) cumulative totals)
- `useCallback` for handlers passed to memoized children
- Extract list items as separate memoized components

### Hook Patterns
- `useMemo` for derived data and expensive calculations
- `useCallback` for async operations with dependencies
- `useEffect` for side effects (fetch, subscriptions)
- `useRef` for WebSocket connections, timeout IDs

### Styling Conventions
- Container: `flex h-full flex-col`
- Header: `border-b border-border px-4 py-2`
- Scrollable: `flex-1 overflow-y-auto`
- Tables: `text-xs`, `px-4 py-2`
- Prices: `font-mono`

### Toast Notifications
```typescript
import { toast } from '@/lib/store'

toast.success('Order Placed', 'Your order has been submitted')
toast.error('Error', 'Failed to place order')
```

---

## Reference Files

For detailed information on specific topics, see:

### [COMPONENTS.md](resources/COMPONENTS.md)
Component patterns:
- Client directive usage
- Component structure
- Prop patterns
- Conditional rendering

### [STATE.md](resources/STATE.md)
State management:
- Zustand store setup
- WalletStore patterns
- TradingStore patterns
- Store subscriptions

### [STYLING.md](resources/STYLING.md)
Tailwind conventions:
- Theme colors
- cn() utility
- Component styling
- Responsive patterns

### [API.md](resources/API.md)
API integration:
- REST client patterns
- Unit conversion
- WebSocket messages
- Error handling

### [WALLET.md](resources/WALLET.md)
Wallet integration:
- useWallet hook
- Agent key system
- EIP-712 signing
- Connection flow

### [HOOKS.md](resources/HOOKS.md)
React hook patterns:
- useCallback usage
- useEffect patterns
- useRef for refs
- Custom hook design

---

## Common Pitfalls

1. **Missing 'use client'** - Interactive components need client directive
2. **Stale closures** - Use useCallback with proper dependencies
3. **Unit conversion** - API returns cents/satoshis, display in dollars/BTC
4. **WebSocket cleanup** - Always return cleanup function from useEffect
5. **Toast spam** - Debounce repeated notifications
6. **Agent key expiry** - Check and refresh before signing
7. **O(N²) calculations** - Use useMemo with O(N) algorithms for cumulative totals
8. **Missing memo()** - High-frequency components need memo() to prevent re-renders

---

## Development Mode

```typescript
import { isDevelopment } from '@/lib/config'

{isDevelopment && <FaucetButton />}
```

Shows:
- Faucet button for test funds
- Dev banner in header
- Relaxed validation

---

## Related Documentation

- `docs/frontend/` - Frontend architecture docs
- `lib/config.ts` - Environment configuration
- `CLAUDE.md` - Project overview

---

**Line Count**: < 500 (following 500-line rule)
**Progressive Disclosure**: Reference files for detailed information
