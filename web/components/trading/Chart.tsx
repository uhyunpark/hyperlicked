'use client'

import { useEffect, useRef, useState, useCallback } from 'react'
import { createChart, ColorType, IChartApi, CandlestickData, Time, CandlestickSeries } from 'lightweight-charts'
import { useTradingStore } from '@/lib/store'
import { getCandles, convertPrice, convertSize } from '@/lib/api'
import { CandlestickAggregator, Interval } from '@/lib/candlestickAggregator'
import type { CandleInterval } from '@/lib/api'

export function Chart() {
  const selectedSymbol = useTradingStore((s) => s.selectedSymbol)
  const trades = useTradingStore((s) => s.trades)
  const isConnected = useTradingStore((s) => s.isConnected)
  const chartContainerRef = useRef<HTMLDivElement>(null)
  const chartRef = useRef<IChartApi | null>(null)
  const candleSeriesRef = useRef<any>(null)
  const aggregatorRef = useRef<CandlestickAggregator | null>(null)
  const [interval, setInterval] = useState<CandleInterval>('1m')
  const [loading, setLoading] = useState(true)
  const lastProcessedTradeRef = useRef<string | null>(null)
  const lastCandleTimeRef = useRef<number | null>(null)
  const abortRef = useRef<AbortController | null>(null)

  // Fetch candles from API
  const fetchCandles = useCallback(async () => {
    abortRef.current?.abort()
    abortRef.current = new AbortController()
    try {
      setLoading(true)
      const candles = await getCandles(selectedSymbol, interval, 2000, abortRef.current.signal)

      // Create new aggregator for this interval
      aggregatorRef.current = new CandlestickAggregator(interval as Interval, 2000)
      lastProcessedTradeRef.current = null

      if (candleSeriesRef.current) {
        // Convert API candles to chart format (can be empty array)
        const chartData: CandlestickData[] = candles.map(c => ({
          time: Math.floor(c.t / 1000) as Time, // Convert ms to seconds
          open: convertPrice(c.o),
          high: convertPrice(c.h),
          low: convertPrice(c.l),
          close: convertPrice(c.c),
        }))

        // Always call setData to initialize series (even empty)
        candleSeriesRef.current.setData(chartData)
        lastCandleTimeRef.current = chartData.length > 0
          ? (chartData[chartData.length - 1].time as number)
          : null
      }
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') return
      console.error('[chart] Failed to fetch candles:', error)
    } finally {
      setLoading(false)
    }
  }, [selectedSymbol, interval])

  // Initialize chart
  useEffect(() => {
    if (!chartContainerRef.current) return

    const chart = createChart(chartContainerRef.current, {
      layout: {
        background: { type: ColorType.Solid, color: '#0c0d10' },
        textColor: '#9a9590',
      },
      grid: {
        vertLines: { color: '#1a1d2515' },
        horzLines: { color: '#1a1d2515' },
      },
      width: chartContainerRef.current.clientWidth,
      height: chartContainerRef.current.clientHeight,
      timeScale: {
        borderColor: '#1f2128',
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
        borderColor: '#1f2128',
      },
    })

    const candleSeries = chart.addSeries(CandlestickSeries, {
      upColor: '#a78bfa',
      downColor: '#c75050',
      borderUpColor: '#a78bfa',
      borderDownColor: '#c75050',
      wickUpColor: '#a78bfa',
      wickDownColor: '#c75050',
    })

    chartRef.current = chart
    candleSeriesRef.current = candleSeries

    // Handle resize using ResizeObserver for container size changes
    const handleResize = () => {
      if (chartContainerRef.current && chartRef.current) {
        chartRef.current.applyOptions({
          width: chartContainerRef.current.clientWidth,
          height: chartContainerRef.current.clientHeight,
        })
      }
    }

    const resizeObserver = new ResizeObserver(handleResize)
    resizeObserver.observe(chartContainerRef.current)

    return () => {
      resizeObserver.disconnect()
      chart.remove()
    }
  }, [])

  // Fetch on mount, symbol change, interval change
  useEffect(() => {
    fetchCandles()
  }, [fetchCandles])

  // Also refetch on WS reconnect to fill gap
  const prevConnectedRef = useRef(false)
  useEffect(() => {
    if (isConnected && !prevConnectedRef.current) {
      fetchCandles()
    }
    prevConnectedRef.current = isConnected
  }, [isConnected, fetchCandles])

  // Update current candle with real-time trades using the aggregator
  useEffect(() => {
    if (!candleSeriesRef.current || !aggregatorRef.current || trades.length === 0) return

    const lastTrade = trades[0] // Most recent trade (trades are prepended)

    // Skip if we already processed this trade
    if (lastProcessedTradeRef.current === lastTrade.id) return
    lastProcessedTradeRef.current = lastTrade.id

    // Convert back to API units (cents/satoshis) for the aggregator
    const apiTrade = {
      price: Math.round(lastTrade.price * 100), // dollars to cents
      size: Math.round(lastTrade.size * 100_000_000), // BTC to satoshis
      timestamp: lastTrade.timestamp,
      side: lastTrade.side,
    }

    // Add trade to aggregator - it handles OHLC properly
    const candle = aggregatorRef.current.addTrade(apiTrade)

    if (candle) {
      try {
        const series = candleSeriesRef.current
        const candleTime = candle.time as number
        const lastTime = lastCandleTimeRef.current

        // update() only works for last candle or newer
        if (lastTime === null) {
          series.setData([{
            time: candle.time,
            open: candle.open,
            high: candle.high,
            low: candle.low,
            close: candle.close,
          }])
          lastCandleTimeRef.current = candleTime
        } else if (candleTime >= lastTime) {
          series.update({
            time: candle.time,
            open: candle.open,
            high: candle.high,
            low: candle.low,
            close: candle.close,
          })
          lastCandleTimeRef.current = candleTime
        }
        // Skip if candle time is older than last - historical data already has it
      } catch (e) {
        // Silently ignore chart update errors
        console.debug('[chart] update error:', e)
      }
    }
  }, [trades])

  return (
    <div className="flex h-full min-h-0 flex-col overflow-hidden bg-bg-secondary">
      {/* Chart header */}
      <div className="flex items-center justify-between border-b border-border px-4 py-2">
        <div className="flex items-center gap-4">
          <h3 className="text-sm font-semibold text-text-primary">{selectedSymbol}</h3>
          <div className="flex gap-1">
            {(['1m', '5m', '15m', '1h', '4h', '1d'] as CandleInterval[]).map((int) => (
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
          {loading ? 'Loading...' : 'Candlestick'}
        </div>
      </div>

      {/* Chart */}
      <div ref={chartContainerRef} className="min-h-0 flex-1" />

      {/* Chart footer - funding rate */}
      <div className="border-t border-border bg-bg-primary px-4 py-2">
        <div className="flex items-center justify-between text-xs">
          <div className="flex items-center gap-4">
            <div>
              <span className="text-text-muted">Funding Rate: </span>
              <span className="font-mono text-long">+0.0100%</span>
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
