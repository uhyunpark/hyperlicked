'use client'

import { useEffect, useRef, useState } from 'react'
import { createChart, ColorType, IChartApi, CandlestickData, Time, CandlestickSeries } from 'lightweight-charts'
import { useTradingStore } from '@/lib/store'
import { CandlestickAggregator, Interval } from '@/lib/candlestickAggregator'
import { convertPrice, convertSize } from '@/lib/api'

export function Chart() {
  const { selectedSymbol, currentPrice, trades } = useTradingStore()
  const chartContainerRef = useRef<HTMLDivElement>(null)
  const chartRef = useRef<IChartApi | null>(null)
  const candleSeriesRef = useRef<any>(null)
  const aggregatorRef = useRef<CandlestickAggregator>(new CandlestickAggregator('1m'))
  const [interval, setInterval] = useState<Interval>('1m')
  const [useRealData, setUseRealData] = useState(true) // Now defaulting to real data!

  // Initialize chart
  useEffect(() => {
    if (!chartContainerRef.current) return

    const chart = createChart(chartContainerRef.current, {
      layout: {
        background: { type: ColorType.Solid, color: '#0a0a0f' },
        textColor: '#9ca3af',
      },
      grid: {
        vertLines: { color: '#1f1f29' },
        horzLines: { color: '#1f1f29' },
      },
      width: chartContainerRef.current.clientWidth,
      height: chartContainerRef.current.clientHeight,
      timeScale: {
        borderColor: '#2e2e3e',
        timeVisible: true,
        secondsVisible: false,
        tickMarkMaxCharacterLength: 12,
      },
      localization: {
        timeFormatter: (time: number) => {
          const date = new Date(time * 1000)
          const hours = String(date.getHours()).padStart(2, '0')
          const minutes = String(date.getMinutes()).padStart(2, '0')
          return `${hours}:${minutes}`
        },
      },
      rightPriceScale: {
        borderColor: '#2e2e3e',
      },
    })

    const candleSeries = chart.addSeries(CandlestickSeries, {
      upColor: '#a855f7',
      downColor: '#ef4444',
      borderUpColor: '#a855f7',
      borderDownColor: '#ef4444',
      wickUpColor: '#a855f7',
      wickDownColor: '#ef4444',
    })

    // Initialize with existing candles (if any)
    const initialCandles = aggregatorRef.current.getCandles()
    if (initialCandles.length > 0) {
      candleSeries.setData(initialCandles)
    } else {
      // If no real data yet, show sample data as placeholder
      const sampleData = generateSampleData(currentPrice)
      candleSeries.setData(sampleData)
    }

    chartRef.current = chart
    candleSeriesRef.current = candleSeries

    // Handle resize
    const handleResize = () => {
      if (chartContainerRef.current && chartRef.current) {
        chartRef.current.applyOptions({
          width: chartContainerRef.current.clientWidth,
          height: chartContainerRef.current.clientHeight,
        })
      }
    }

    window.addEventListener('resize', handleResize)

    return () => {
      window.removeEventListener('resize', handleResize)
      chart.remove()
    }
  }, [currentPrice])

  // Update with new trades (real data)
  useEffect(() => {
    if (!candleSeriesRef.current || trades.length === 0) return

    const lastTrade = trades[0] // Most recent trade (trades are prepended)

    // Add trade to aggregator and get updated candle
    const updatedCandle = aggregatorRef.current.addTrade({
      price: lastTrade.price,    // Already in cents from WebSocket
      size: lastTrade.size,      // Already in sats from WebSocket
      timestamp: lastTrade.timestamp,
      side: lastTrade.side,
    })

    if (updatedCandle) {
      // Update chart with the modified candle
      candleSeriesRef.current.update({
        time: updatedCandle.time,
        open: updatedCandle.open,
        high: updatedCandle.high,
        low: updatedCandle.low,
        close: updatedCandle.close,
      })
    }
  }, [trades])

  // Handle interval changes
  useEffect(() => {
    if (!candleSeriesRef.current) return

    // Update aggregator interval
    aggregatorRef.current.setInterval(interval)

    // Re-render chart with new interval data
    const candles = aggregatorRef.current.getCandles()
    if (candles.length > 0) {
      candleSeriesRef.current.setData(candles)
    }
  }, [interval])

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Chart header */}
      <div className="flex items-center justify-between border-b border-border px-4 py-2">
        <div className="flex items-center gap-4">
          <h3 className="text-sm font-semibold text-text-primary">{selectedSymbol}</h3>
          <div className="flex gap-1">
            {(['1m', '5m', '15m', '1h', '4h', '1d'] as Interval[]).map((int) => (
              <button
                key={int}
                onClick={() => setInterval(int)}
                className={`rounded px-2 py-1 text-xs transition-colors ${
                  interval === int
                    ? 'bg-accent text-white'
                    : 'text-text-muted hover:bg-bg-tertiary hover:text-text-primary'
                }`}
              >
                {int}
              </button>
            ))}
          </div>
        </div>
        <div className="text-xs text-text-muted">
          Candlestick
        </div>
      </div>

      {/* Chart */}
      <div ref={chartContainerRef} className="flex-1" />

      {/* Chart footer - funding rate */}
      <div className="border-t border-border bg-bg-primary px-4 py-2">
        <div className="flex items-center justify-between text-xs">
          <div className="flex items-center gap-4">
            <div>
              <span className="text-text-muted">Funding Rate: </span>
              <span className="font-mono text-green-buy">+0.0100%</span>
            </div>
            <div>
              <span className="text-text-muted">Next Funding: </span>
              <span className="font-mono text-text-primary">7h 32m</span>
            </div>
          </div>
          <div>
            <span className="text-text-muted">24h Volume: </span>
            <span className="font-mono text-text-primary">$1,234,567,890</span>
          </div>
        </div>
      </div>
    </div>
  )
}

// Generate sample candlestick data for display
function generateSampleData(basePrice: number): CandlestickData[] {
  const data: CandlestickData[] = []

  // Get current time aligned to 1-minute intervals
  const now = new Date()
  now.setSeconds(0)
  now.setMilliseconds(0)
  const currentMinute = Math.floor(now.getTime() / 1000)
  const oneMinute = 60

  // Generate 100 candles (100 minutes of historical data)
  for (let i = 100; i >= 0; i--) {
    const time = (currentMinute - i * oneMinute) as any
    const randomWalk = (Math.random() - 0.5) * basePrice * 0.02 // ±2% random walk
    const open = basePrice + randomWalk
    const close = open + (Math.random() - 0.5) * basePrice * 0.01
    const high = Math.max(open, close) + Math.random() * basePrice * 0.005
    const low = Math.min(open, close) - Math.random() * basePrice * 0.005

    data.push({
      time,
      open: Math.round(open * 100) / 100,
      high: Math.round(high * 100) / 100,
      low: Math.round(low * 100) / 100,
      close: Math.round(close * 100) / 100,
    })

    // Update basePrice for next candle (random walk)
    basePrice = close
  }

  return data
}
