import type { OrderbookData, Trade, Position, Order } from './types'

// Generate mock orderbook data
export function generateMockOrderbook(): OrderbookData {
  const basePrice = 50000
  const bids = []
  const asks = []

  // Generate 20 bid levels
  for (let i = 0; i < 20; i++) {
    const price = basePrice - (i * 50) - Math.random() * 50
    const size = Math.random() * 5 + 0.1
    bids.push({ price, size })
  }

  // Generate 20 ask levels
  for (let i = 0; i < 20; i++) {
    const price = basePrice + (i * 50) + Math.random() * 50
    const size = Math.random() * 5 + 0.1
    asks.push({ price, size })
  }

  return {
    symbol: 'BTC-USDT',
    bids,
    asks,
    timestamp: Date.now()
  }
}

// Generate mock trades
export function generateMockTrades(count = 50): Trade[] {
  const trades: Trade[] = []
  const basePrice = 50000

  for (let i = 0; i < count; i++) {
    trades.push({
      id: `trade_${i}`,
      symbol: 'BTC-USDT',
      price: basePrice + (Math.random() - 0.5) * 1000,
      size: Math.random() * 2,
      side: Math.random() > 0.5 ? 'buy' : 'sell',
      timestamp: Date.now() - (i * 1000)
    })
  }

  return trades
}

// Generate mock positions
export function generateMockPositions(): Position[] {
  return [
    {
      symbol: 'BTC-USDT',
      size: 1.5,
      entryPrice: 49500,
      markPrice: 50000,
      liquidationPrice: 45000,
      unrealizedPnl: 750,
      margin: 5000,
      leverage: 10
    }
  ]
}

// Generate mock open orders
export function generateMockOrders(): Order[] {
  return [
    {
      id: 'order_1',
      symbol: 'BTC-USDT',
      side: 'buy',
      type: 'limit',
      price: 49000,
      size: 0.5,
      filled: 0,
      remaining: 0.5,
      status: 'open',
      timestamp: Date.now() - 60000
    },
    {
      id: 'order_2',
      symbol: 'BTC-USDT',
      side: 'sell',
      type: 'limit',
      price: 51000,
      size: 0.3,
      filled: 0,
      remaining: 0.3,
      status: 'open',
      timestamp: Date.now() - 120000
    }
  ]
}
