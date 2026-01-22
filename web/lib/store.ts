import { create } from 'zustand'
import type { OrderbookData, Trade, Position, Order, UserFill, TriggerOrder } from './types'

// =============================================================================
// Wallet Store (shared across all components)
// =============================================================================

interface WalletStoreState {
  isConnected: boolean
  address: string | null
  isRabby: boolean
  chainId: number | null
  tradingEnabled: boolean
  agentAddress: string | null
  delegationExpiry: string | null
  error: string | null
  needsReconnect: boolean

  // Actions
  setConnected: (address: string, chainId: number, isRabby: boolean) => void
  setDisconnected: () => void
  setTradingEnabled: (enabled: boolean, agentAddress?: string | null, expiry?: string | null) => void
  setError: (error: string | null, needsReconnect?: boolean) => void
  setChainId: (chainId: number) => void
}

export const useWalletStore = create<WalletStoreState>((set) => ({
  isConnected: false,
  address: null,
  isRabby: false,
  chainId: null,
  tradingEnabled: false,
  agentAddress: null,
  delegationExpiry: null,
  error: null,
  needsReconnect: false,

  setConnected: (address, chainId, isRabby) => set({
    isConnected: true,
    address,
    chainId,
    isRabby,
    error: null,
    needsReconnect: false
  }),

  setDisconnected: () => set({
    isConnected: false,
    address: null,
    chainId: null,
    tradingEnabled: false,
    agentAddress: null,
    delegationExpiry: null,
    error: null,
    needsReconnect: false
  }),

  setTradingEnabled: (enabled, agentAddress = null, expiry = null) => set({
    tradingEnabled: enabled,
    agentAddress: enabled ? agentAddress : null,
    delegationExpiry: enabled ? expiry : null
  }),

  setError: (error, needsReconnect = false) => set({
    error,
    needsReconnect
  }),

  setChainId: (chainId) => set({ chainId })
}))

// =============================================================================
// Trading Store
// =============================================================================

interface AssetCtx {
  markPrice: number           // dollars
  oraclePrice: number | null  // dollars (null if oracle disabled)
  midPrice: number            // dollars
  fundingRate: number         // decimal (0.0001 = 0.01%)
  premium: number             // decimal
  openInterest: number        // BTC (base units)
  prevDayPrice: number        // dollars
  dayVolume: number           // BTC (base units)
  dayNotionalVolume: number   // dollars
  nextFundingTime: number     // ms timestamp
}

interface TradingState {
  // Market data
  orderbook: OrderbookData
  trades: Trade[]
  currentPrice: number
  markPrices: Record<string, number>
  assetCtx: AssetCtx | null   // Market stats from activeAssetCtx channel

  // User data
  positions: Position[]
  openOrders: Order[]
  userFills: UserFill[]
  triggerOrders: TriggerOrder[]  // Trigger orders (TP/SL)

  // UI state
  selectedSymbol: string
  isConnected: boolean // WebSocket connection status
  balanceRefreshTrigger: number // Increments when balance update received

  // Actions
  updateOrderbook: (orderbook: OrderbookData) => void
  addTrade: (trade: Trade) => void
  setPositions: (positions: Position[]) => void
  setOpenOrders: (orders: Order[]) => void
  setUserFills: (fills: UserFill[]) => void
  addUserFill: (fill: UserFill) => void
  clearUserFills: () => void
  setSelectedSymbol: (symbol: string) => void
  setWsConnected: (connected: boolean) => void
  triggerBalanceRefresh: () => void
  updateMarkPrice: (symbol: string, price: number) => void
  updateAssetCtx: (ctx: AssetCtx) => void
  // Trigger order actions
  setTriggerOrders: (orders: TriggerOrder[]) => void
  addTriggerOrder: (order: TriggerOrder) => void
  removeTriggerOrder: (id: string) => void
  clearTriggerOrders: () => void
}

export const useTradingStore = create<TradingState>((set) => ({
  // Initial state (empty, will be populated by WebSocket)
  orderbook: {
    symbol: 'BTC-USDT',
    bids: [],
    asks: [],
    timestamp: Date.now()
  },
  trades: [],
  currentPrice: 0,
  markPrices: {},
  assetCtx: null,
  positions: [],
  openOrders: [],
  userFills: [],
  triggerOrders: [],
  selectedSymbol: 'BTC-USDT',
  isConnected: false,
  balanceRefreshTrigger: 0,

  // Actions
  updateOrderbook: (orderbook) => set({
    orderbook,
    currentPrice: orderbook.asks[0]?.price || orderbook.bids[0]?.price || 0
  }),

  addTrade: (trade) => set((state) => {
    // Deduplicate by ID
    if (state.trades.some(t => t.id === trade.id)) return state
    return { trades: [trade, ...state.trades].slice(0, 100) } // Keep last 100 trades
  }),

  setPositions: (positions) => set({ positions }),
  setOpenOrders: (orders) => set({ openOrders: orders }),
  setUserFills: (fills) => set({ userFills: fills }),
  addUserFill: (fill) => set((state) => {
    // Deduplicate by ID and prepend new fill
    if (state.userFills.some(f => f.id === fill.id)) return state
    return { userFills: [fill, ...state.userFills].slice(0, 100) } // Keep last 100 fills
  }),
  clearUserFills: () => set({ userFills: [] }),
  setSelectedSymbol: (symbol) => set({ selectedSymbol: symbol }),
  setWsConnected: (connected) => set({ isConnected: connected }),
  triggerBalanceRefresh: () => set((state) => ({ balanceRefreshTrigger: state.balanceRefreshTrigger + 1 })),
  updateMarkPrice: (symbol, price) => set((state) => ({
    markPrices: { ...state.markPrices, [symbol]: price }
  })),
  updateAssetCtx: (ctx) => set({ assetCtx: ctx }),
  // Trigger order actions
  setTriggerOrders: (orders) => set({ triggerOrders: orders }),
  addTriggerOrder: (order) => set((state) => {
    // Avoid duplicates
    if (state.triggerOrders.some(o => o.id === order.id)) return state
    return { triggerOrders: [...state.triggerOrders, order] }
  }),
  removeTriggerOrder: (id) => set((state) => ({
    triggerOrders: state.triggerOrders.filter(o => o.id !== id)
  })),
  clearTriggerOrders: () => set({ triggerOrders: [] })
}))
