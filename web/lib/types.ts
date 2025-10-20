// Trading types matching backend structure

export type Side = 'buy' | 'sell'
export type OrderType = 'limit' | 'market' | 'stop'
export type TimeInForce = 'GTC' | 'IOC' | 'ALO'

export interface PriceLevel {
  price: number
  size: number
  total?: number // cumulative size
}

export interface Order {
  id: string
  symbol: string
  side: Side
  type: OrderType
  price: number
  size: number
  filled: number
  remaining: number
  status: 'open' | 'filled' | 'cancelled'
  timestamp: number
}

export interface Trade {
  id: string
  symbol: string
  price: number
  size: number
  side: Side
  timestamp: number
}

export interface Position {
  symbol: string
  size: number // positive = long, negative = short
  entryPrice: number
  markPrice: number
  liquidationPrice: number
  unrealizedPnl: number
  margin: number
  leverage: number
}

export interface OrderbookData {
  symbol: string
  bids: PriceLevel[]
  asks: PriceLevel[]
  timestamp: number
}

export interface Account {
  address: string
  balance: number
  availableBalance: number
  lockedCollateral: number
  totalEquity: number
}
