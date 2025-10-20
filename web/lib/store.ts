import { create } from 'zustand'
import type { OrderbookData, Trade, Position, Order } from './types'

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

  // Actions
  updateOrderbook: (orderbook: OrderbookData) => void
  addTrade: (trade: Trade) => void
  setPositions: (positions: Position[]) => void
  setOpenOrders: (orders: Order[]) => void
  setSelectedSymbol: (symbol: string) => void
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
  setSelectedSymbol: (symbol) => set({ selectedSymbol: symbol })
}))
