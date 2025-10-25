/**
 * CandlestickAggregator - Aggregates trade data into OHLCV candlesticks
 *
 * Usage:
 *   const aggregator = new CandlestickAggregator('1m')
 *   aggregator.addTrade({ price, size, timestamp })
 *   const candles = aggregator.getCandles()
 */

import { CandlestickData, Time } from 'lightweight-charts'
import { convertPrice } from './api'

export type Interval = '1m' | '5m' | '15m' | '1h' | '4h' | '1d'

export interface Trade {
  price: number    // USDT cents (5000000 = $50,000.00)
  size: number     // Satoshis (100000000 = 1 BTC)
  timestamp: number // Unix milliseconds
  side: string     // "buy" or "sell" (taker side)
}

interface Candle {
  time: Time
  open: number
  high: number
  low: number
  close: number
  volume: number  // Total volume in BTC
}

export class CandlestickAggregator {
  private interval: Interval
  private candles: Map<number, Candle> = new Map()
  private intervalMs: number
  private maxCandles: number = 500 // Keep last 500 candles in memory

  constructor(interval: Interval = '1m', maxCandles: number = 500) {
    this.interval = interval
    this.maxCandles = maxCandles
    this.intervalMs = this.getIntervalMs(interval)
  }

  /**
   * Convert interval string to milliseconds
   */
  private getIntervalMs(interval: Interval): number {
    const intervals: Record<Interval, number> = {
      '1m': 60 * 1000,
      '5m': 5 * 60 * 1000,
      '15m': 15 * 60 * 1000,
      '1h': 60 * 60 * 1000,
      '4h': 4 * 60 * 60 * 1000,
      '1d': 24 * 60 * 60 * 1000,
    }
    return intervals[interval]
  }

  /**
   * Get bucket time (start of candle period) for a given timestamp
   */
  private getBucketTime(timestamp: number): number {
    return Math.floor(timestamp / this.intervalMs) * this.intervalMs
  }

  /**
   * Add a trade and update the corresponding candle
   */
  addTrade(trade: Trade): Candle | null {
    const bucketTime = this.getBucketTime(trade.timestamp)
    const timeInSeconds = Math.floor(bucketTime / 1000) as Time
    const priceInDollars = convertPrice(trade.price) // Convert cents to dollars
    const sizeInBTC = trade.size / 100000000 // Convert sats to BTC

    let candle = this.candles.get(bucketTime)

    if (!candle) {
      // Create new candle
      candle = {
        time: timeInSeconds,
        open: priceInDollars,
        high: priceInDollars,
        low: priceInDollars,
        close: priceInDollars,
        volume: sizeInBTC,
      }
      this.candles.set(bucketTime, candle)

      // Evict old candles if we exceed maxCandles
      this.evictOldCandles()
    } else {
      // Update existing candle
      candle.high = Math.max(candle.high, priceInDollars)
      candle.low = Math.min(candle.low, priceInDollars)
      candle.close = priceInDollars // Last trade price
      candle.volume += sizeInBTC
    }

    return candle
  }

  /**
   * Remove old candles to keep memory usage bounded
   */
  private evictOldCandles() {
    if (this.candles.size <= this.maxCandles) return

    // Get sorted bucket times
    const bucketTimes = Array.from(this.candles.keys()).sort((a, b) => a - b)

    // Remove oldest candles
    const toRemove = bucketTimes.slice(0, bucketTimes.length - this.maxCandles)
    toRemove.forEach(time => this.candles.delete(time))
  }

  /**
   * Get all candles sorted by time (for chart rendering)
   */
  getCandles(): CandlestickData[] {
    const bucketTimes = Array.from(this.candles.keys()).sort((a, b) => a - b)
    return bucketTimes.map(bucketTime => {
      const candle = this.candles.get(bucketTime)!
      return {
        time: candle.time,
        open: candle.open,
        high: candle.high,
        low: candle.low,
        close: candle.close,
      }
    })
  }

  /**
   * Get the most recent candle (for live updates)
   */
  getLatestCandle(): Candle | null {
    if (this.candles.size === 0) return null

    const latestBucketTime = Math.max(...Array.from(this.candles.keys()))
    return this.candles.get(latestBucketTime) || null
  }

  /**
   * Change interval and re-aggregate candles
   */
  setInterval(interval: Interval) {
    if (this.interval === interval) return

    this.interval = interval
    this.intervalMs = this.getIntervalMs(interval)

    // Re-aggregate existing candles into new interval
    // (For simplicity, we'll just clear and let new trades rebuild)
    // In production, you might want to preserve and re-bucket existing data
    this.candles.clear()
  }

  /**
   * Clear all candles
   */
  clear() {
    this.candles.clear()
  }

  /**
   * Get current interval
   */
  getInterval(): Interval {
    return this.interval
  }

  /**
   * Get number of candles in memory
   */
  size(): number {
    return this.candles.size
  }
}
