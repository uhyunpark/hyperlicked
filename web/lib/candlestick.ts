// Candlestick aggregator for real-time trade data
import { CandlestickData, Time } from 'lightweight-charts'

export interface Trade {
  price: number
  size: number
  side: 'buy' | 'sell'
  timestamp: number // Unix milliseconds
}

export type Interval = '1m' | '5m' | '15m' | '1h' | '4h' | '1d'

// Interval durations in milliseconds
const INTERVAL_MS: Record<Interval, number> = {
  '1m': 60 * 1000,
  '5m': 5 * 60 * 1000,
  '15m': 15 * 60 * 1000,
  '1h': 60 * 60 * 1000,
  '4h': 4 * 60 * 60 * 1000,
  '1d': 24 * 60 * 60 * 1000,
}

// Get bucket start time (align to interval)
function getBucketTime(timestamp: number, interval: Interval): number {
  const ms = INTERVAL_MS[interval]
  return Math.floor(timestamp / ms) * ms
}

export class CandlestickAggregator {
  private candles: Map<number, CandlestickData> = new Map()
  private interval: Interval

  constructor(interval: Interval = '1m') {
    this.interval = interval
  }

  // Add a trade and update the corresponding candle
  addTrade(trade: Trade): CandlestickData | null {
    const bucketTime = getBucketTime(trade.timestamp, this.interval)
    const timeInSeconds = Math.floor(bucketTime / 1000) as Time

    let candle = this.candles.get(bucketTime)

    if (!candle) {
      // First trade in this bucket - create new candle
      candle = {
        time: timeInSeconds,
        open: trade.price,
        high: trade.price,
        low: trade.price,
        close: trade.price,
      }
      this.candles.set(bucketTime, candle)
    } else {
      // Update existing candle
      candle.high = Math.max(candle.high, trade.price)
      candle.low = Math.min(candle.low, trade.price)
      candle.close = trade.price // Most recent trade is close
    }

    return candle
  }

  // Get all candles sorted by time (for initial chart load)
  getCandles(): CandlestickData[] {
    return Array.from(this.candles.values()).sort((a, b) =>
      (a.time as number) - (b.time as number)
    )
  }

  // Get the latest candle (for real-time updates)
  getLatestCandle(): CandlestickData | null {
    const candles = this.getCandles()
    return candles.length > 0 ? candles[candles.length - 1] : null
  }

  // Change interval and rebuild candles from trades
  setInterval(interval: Interval) {
    this.interval = interval
    // Note: You'd need to rebuild from stored trades
    // For now, just clear and start fresh
    this.candles.clear()
  }

  // Clear all candles (e.g., when switching symbols)
  clear() {
    this.candles.clear()
  }
}
