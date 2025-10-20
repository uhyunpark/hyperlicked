# HyperLicked Frontend

Next.js 15.5.5 trading interface with purple theme, real-time WebSocket updates, and Hyperliquid-style UI.

## Quick Start

### 1. Configure Environment

```bash
cp .env.example .env.local
```

**Use Ethereum Mainnet (recommended):**
Your `.env.local` should have:
```bash
NEXT_PUBLIC_CHAIN_ID=1
NEXT_PUBLIC_RPC_URL=https://eth.llamarpc.com
```

### 2. Install & Start

```bash
# Install dependencies
bun install

# Start backend (separate terminal)
cd ..
go run cmd/node/main.go

# Start frontend
cd web
bun run dev

# Open http://localhost:3000
```

### 3. Connect Wallet

1. Install [Rabby Wallet](https://rabby.io/) or use MetaMask
2. Switch to "Ethereum Mainnet"
3. Click "Connect Wallet"
4. Sign orders with EIP-712 (no gas fees!)

**See [docs/WALLET_NETWORK_SETUP.md](../docs/WALLET_NETWORK_SETUP.md) for network configuration.**

## Tech Stack

- **Next.js 15.5.5** - App Router, Turbopack, React Server Components
- **TypeScript** - Strict mode
- **Tailwind CSS v4** - Custom purple theme (#a855f7)
- **Zustand** - State management (no Redux)
- **WebSocket** - Real-time orderbook updates
- **TradingView Lightweight Charts** - Price charts (planned)

## Architecture

```
app/page.tsx (Main Trading Page)
├─ Header.tsx          → Market selector, price ticker, wallet connect
├─ Orderbook.tsx       → Live bid/ask ladder with depth bars
├─ Chart.tsx           → TradingView chart (placeholder)
├─ TradePanel.tsx      → Order form (buy/sell, limit/market, leverage)
└─ BottomTabs.tsx      → Tab container
   ├─ RecentTrades     → Trade feed
   ├─ OpenOrders       → Active orders table
   └─ Positions        → Current positions, PnL
```

## Key Features

### Real-Time Updates
- WebSocket connection to `ws://localhost:8080/ws`
- Orderbook updates every ~100ms (every block)
- Auto-reconnect with 3-second delay
- Channel subscriptions: `orderbook:BTC-USDT`, `trades:BTC-USDT`

### Unit Conversion
API uses integer units (cents, satoshis) to avoid floating-point non-determinism.

```typescript
// lib/api.ts
convertPrice(5000000)        // API → Display: 50000.00
convertSize(100000000)       // API → Display: 1.0 BTC
convertToApiPrice(50000)     // Display → API: 5000000
convertToApiSize(0.01)       // Display → API: 1000000
```

### Custom Purple Theme
**Brand differentiation:** Uses purple (#a855f7) instead of green for buys/longs.

```css
/* app/globals.css */
--green-buy: #a855f7;    /* Bright purple */
--red-sell: #ef4444;     /* Red */
--accent: #a855f7;       /* Purple accent */
```

## State Management (Zustand)

**No mocks!** All data comes from WebSocket.

```typescript
// lib/store.ts
const useTradingStore = create<TradingState>((set) => ({
  orderbook: { bids: [], asks: [] },
  trades: [],
  currentPrice: 0,

  updateOrderbook: (orderbook) => set({ orderbook }),
  addTrade: (trade) => set((state) => ({
    trades: [trade, ...state.trades].slice(0, 50)
  }))
}))
```

## Order Submission

**Phase 1:** Uses mock address (MetaMask integration in Phase 2).

```typescript
// components/trading/TradePanel.tsx
const handleSubmit = async () => {
  const userAddress = '0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0' // Mock

  const order = {
    address: userAddress,
    symbol: 'BTC-USDT',
    side: 'BUY',
    type: 'GTC',
    price: convertToApiPrice(parseFloat(price)),
    size: convertToApiSize(parseFloat(size)),
    leverage: 10
  }

  const response = await submitOrder(order)
  alert(`Order submitted! ID: ${response.orderId}`)
}
```

**Flow:** POST → API server → Mempool → Block (~100ms) → Matching → WebSocket update

## Troubleshooting

### "Connecting to blockchain..." never disappears
- ❌ Backend not running: `go run cmd/node/main.go`
- ❌ Wrong API URL: Check `web/.env.local` has `NEXT_PUBLIC_API_URL=http://localhost:8080/api/v1`
- ❌ CORS issue: Ensure backend allows `localhost:3000`

### Orderbook not updating
- Check DevTools Console for `[ws] Connected!`
- Check DevTools Network tab → WS filter → Messages
- Verify backend logs show block commits

### Order submission fails
- Check price/size conversion (integers only!)
- Verify request format matches `pkg/api/types.go`
- Check backend logs for transaction errors

## Phase 2: Wallet Integration

```typescript
// Planned for Phase 2
import { ethers } from 'ethers'

async function connectWallet() {
  const provider = new ethers.BrowserProvider(window.ethereum)
  const signer = await provider.getSigner()
  const address = await signer.getAddress()
  return { provider, signer, address }
}

// EIP-712 order signing
const signature = await signer.signTypedData(domain, types, order)
```

## File Structure

```
web/
├── app/
│   ├── page.tsx                    # Main trading page
│   ├── layout.tsx                  # Root layout
│   └── globals.css                 # Purple theme
├── components/trading/             # UI components
│   ├── Header.tsx
│   ├── Orderbook.tsx
│   ├── Chart.tsx
│   ├── TradePanel.tsx
│   ├── RecentTrades.tsx
│   ├── OpenOrders.tsx
│   ├── Positions.tsx
│   └── BottomTabs.tsx
├── lib/
│   ├── api.ts                      # REST client + unit converters
│   ├── useWebSocket.ts             # WebSocket hook
│   └── store.ts                    # Zustand state
└── package.json
```

## Environment Variables

Create `web/.env.local`:

```bash
NEXT_PUBLIC_API_URL=http://localhost:8080/api/v1
NEXT_PUBLIC_WS_URL=ws://localhost:8080/ws
```

## Development Commands

```bash
bun run dev        # Start dev server (hot reload)
bun run build      # Build for production
bun run start      # Start production server
bun run lint       # Run linter
```

## Next Steps

- [ ] MetaMask wallet integration
- [ ] EIP-712 order signing
- [ ] TradingView chart integration
- [ ] Order history table
- [ ] Position management UI
- [ ] Mobile responsive design

See [CLAUDE.md](../CLAUDE.md) for full project context.
