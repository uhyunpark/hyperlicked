'use client'

import { useState, useEffect, useCallback } from 'react'
import { useTradingStore } from '@/lib/store'
import { getOrderbook, getTrades, convertPrice, convertSize } from '@/lib/api'
import type { PriceLevel } from '@/lib/types'

type OrderbookTab = 'orderbook' | 'trades'

function OrderbookRow({
  level,
  side,
  maxSize,
  onClick
}: {
  level: PriceLevel
  side: 'bid' | 'ask'
  maxSize: number
  onClick?: (price: number) => void
}) {
  const depthPercent = (level.size / maxSize) * 100
  const isBid = side === 'bid'

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      onClick?.(level.price)
    }
  }

  return (
    <div
      className="relative flex cursor-pointer items-center justify-between px-3 py-0.5 text-xs font-mono transition-colors hover:bg-bg-tertiary focus:bg-bg-tertiary focus:outline-none focus:ring-1 focus:ring-accent focus:ring-inset"
      onClick={() => onClick?.(level.price)}
      onKeyDown={handleKeyDown}
      role="button"
      tabIndex={0}
      aria-label={`${isBid ? 'Bid' : 'Ask'} at ${level.price.toLocaleString('en-US', { minimumFractionDigits: 2 })}, size ${level.size.toFixed(4)}`}
    >
      {/* Depth bar */}
      <div
        className={`absolute inset-y-0 ${isBid ? 'right-0 bg-green-bg' : 'right-0 bg-red-bg'} opacity-30`}
        style={{ width: `${depthPercent}%` }}
      />

      {/* Price */}
      <div className={`relative z-10 ${isBid ? 'text-green-buy' : 'text-red-sell'}`}>
        {level.price.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
      </div>

      {/* Size */}
      <div className="relative z-10 text-text-primary">
        {level.size.toFixed(4)}
      </div>

      {/* Total (cumulative) */}
      <div className="relative z-10 text-text-muted">
        {(level.total || level.size).toFixed(2)}
      </div>
    </div>
  )
}

// Trades panel component for the tab
function TradesPanel() {
  const { trades } = useTradingStore()

  return (
    <div className="flex h-full flex-col">
      {/* Column headers */}
      <div className="flex justify-between border-b border-border px-3 py-1 text-xs text-text-muted">
        <div>Price (USDT)</div>
        <div>Size (BTC)</div>
        <div>Time</div>
      </div>

      {/* Trades list */}
      <div className="flex-1 overflow-y-auto">
        {trades.map((trade) => {
          const time = new Date(trade.timestamp)
          const timeStr = time.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', second: '2-digit' })
          const isBuy = trade.side === 'buy'

          return (
            <div
              key={trade.id}
              className="flex justify-between px-3 py-0.5 text-xs font-mono transition-colors hover:bg-bg-tertiary"
            >
              <div className={isBuy ? 'text-green-buy' : 'text-red-sell'}>
                {trade.price.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
              </div>
              <div className="text-text-primary">{trade.size.toFixed(4)}</div>
              <div className="text-text-muted">{timeStr}</div>
            </div>
          )
        })}
      </div>
    </div>
  )
}

export function Orderbook() {
  const [activeTab, setActiveTab] = useState<OrderbookTab>('orderbook')
  const { orderbook, updateOrderbook, addTrade, isConnected: wsConnected } = useTradingStore()

  // REST fallback: fetch orderbook data if WebSocket isn't connected
  const fetchData = useCallback(async () => {
    try {
      // Fetch orderbook
      const book = await getOrderbook('BTC-USDT')
      updateOrderbook({
        symbol: book.symbol,
        bids: book.bids.map(b => ({ price: convertPrice(b.price), size: convertSize(b.size) })),
        asks: book.asks.map(a => ({ price: convertPrice(a.price), size: convertSize(a.size) })),
        timestamp: book.timestamp
      })

      // Fetch recent trades
      const tradesData = await getTrades('BTC-USDT', 50)
      tradesData.forEach((t) => {
        addTrade({
          id: t.id,  // Use server-provided deterministic ID
          symbol: 'BTC-USDT',
          price: convertPrice(t.price),
          size: convertSize(t.size),
          side: t.side as 'buy' | 'sell',
          timestamp: t.timestamp
        })
      })
    } catch (error) {
      console.error('[orderbook] REST fetch failed:', error)
    }
  }, [updateOrderbook, addTrade])

  // Poll for data if WebSocket isn't connected
  useEffect(() => {
    // Fetch immediately
    fetchData()

    // If WebSocket isn't connected, poll every 2 seconds
    if (!wsConnected) {
      const interval = setInterval(fetchData, 2000)
      return () => clearInterval(interval)
    }
  }, [wsConnected, fetchData])

  // Calculate cumulative totals
  const bidsWithTotal = orderbook.bids.map((bid, i) => ({
    ...bid,
    total: orderbook.bids.slice(0, i + 1).reduce((sum, b) => sum + b.size, 0)
  }))

  const asksWithTotal = orderbook.asks.map((ask, i) => ({
    ...ask,
    total: orderbook.asks.slice(0, i + 1).reduce((sum, a) => sum + a.size, 0)
  }))

  // Get max size for depth visualization
  const maxBidSize = Math.max(...bidsWithTotal.map(b => b.size), 1)
  const maxAskSize = Math.max(...asksWithTotal.map(a => a.size), 1)
  const maxSize = Math.max(maxBidSize, maxAskSize)

  // Get spread
  const bestBid = bidsWithTotal[0]?.price || 0
  const bestAsk = asksWithTotal[0]?.price || 0
  const spread = bestAsk - bestBid
  const spreadPercent = ((spread / bestAsk) * 100) || 0

  const handlePriceClick = (price: number) => {
    console.log('Selected price:', price)
    // TODO: Set price in trade panel
  }

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Tab Header */}
      <div className="flex border-b border-border" role="tablist" aria-label="Orderbook and trades">
        <button
          onClick={() => setActiveTab('orderbook')}
          className={`flex-1 px-3 py-2 text-sm font-medium transition-colors ${
            activeTab === 'orderbook'
              ? 'border-b-2 border-accent text-text-primary'
              : 'text-text-muted hover:text-text-secondary'
          }`}
          role="tab"
          aria-selected={activeTab === 'orderbook'}
          aria-controls="orderbook-panel"
          id="orderbook-tab"
        >
          Order Book
        </button>
        <button
          onClick={() => setActiveTab('trades')}
          className={`flex-1 px-3 py-2 text-sm font-medium transition-colors ${
            activeTab === 'trades'
              ? 'border-b-2 border-accent text-text-primary'
              : 'text-text-muted hover:text-text-secondary'
          }`}
          role="tab"
          aria-selected={activeTab === 'trades'}
          aria-controls="trades-panel"
          id="trades-tab"
        >
          Trades
        </button>
      </div>

      {/* Tab Content */}
      {activeTab === 'trades' ? (
        <div role="tabpanel" id="trades-panel" aria-labelledby="trades-tab" className="flex-1 overflow-hidden">
          <TradesPanel />
        </div>
      ) : (
        <div role="tabpanel" id="orderbook-panel" aria-labelledby="orderbook-tab" className="flex flex-1 flex-col overflow-hidden">
          {/* Column headers */}
          <div className="flex justify-between border-b border-border px-3 py-1 text-xs text-text-muted">
            <div>Price (USDT)</div>
            <div>Size (BTC)</div>
            <div>Total</div>
          </div>

          {/* Orderbook content */}
          <div className="flex-1 overflow-y-auto">
            {/* Asks (reversed - lowest at bottom) */}
            <div className="flex flex-col-reverse">
              {asksWithTotal.slice(0, 15).map((ask, i) => (
                <OrderbookRow
                  key={`ask-${i}`}
                  level={ask}
                  side="ask"
                  maxSize={maxSize}
                  onClick={handlePriceClick}
                />
              ))}
            </div>

            {/* Spread indicator */}
            <div className="border-y border-border bg-bg-tertiary px-3 py-1.5 text-center">
              <div className="text-xs font-mono text-text-primary">
                {bestAsk.toLocaleString()} ↔ {bestBid.toLocaleString()}
              </div>
              <div className="text-xs text-text-muted">
                Spread: {spread.toFixed(2)} ({spreadPercent.toFixed(3)}%)
              </div>
            </div>

            {/* Bids */}
            <div>
              {bidsWithTotal.slice(0, 15).map((bid, i) => (
                <OrderbookRow
                  key={`bid-${i}`}
                  level={bid}
                  side="bid"
                  maxSize={maxSize}
                  onClick={handlePriceClick}
                />
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
