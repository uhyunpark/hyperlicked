import { create } from 'zustand'
import type { OrderbookData, Trade, Position, Order } from './types'

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
  isConnected: boolean // WebSocket connection status

  // Actions
  updateOrderbook: (orderbook: OrderbookData) => void
  addTrade: (trade: Trade) => void
  setPositions: (positions: Position[]) => void
  setOpenOrders: (orders: Order[]) => void
  setSelectedSymbol: (symbol: string) => void
  setWsConnected: (connected: boolean) => void
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
  positions: [],
  openOrders: [],
  selectedSymbol: 'BTC-USDT',
  isConnected: false,

  // Actions
  updateOrderbook: (orderbook) => set({
    orderbook,
    currentPrice: orderbook.asks[0]?.price || orderbook.bids[0]?.price || 0
  }),

  addTrade: (trade) => set((state) => ({
    trades: [trade, ...state.trades].slice(0, 100) // Keep last 100 trades
  })),

  setPositions: (positions) => set({ positions }),
  setOpenOrders: (orders) => set({ openOrders: orders }),
  setSelectedSymbol: (symbol) => set({ selectedSymbol: symbol }),
  setWsConnected: (connected) => set({ isConnected: connected })
}))
