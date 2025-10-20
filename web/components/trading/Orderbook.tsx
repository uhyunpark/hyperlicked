'use client'

import { useTradingStore } from '@/lib/store'
import type { PriceLevel } from '@/lib/types'

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

  return (
    <div
      className="relative flex cursor-pointer items-center justify-between px-3 py-0.5 text-xs font-mono transition-colors hover:bg-bg-tertiary"
      onClick={() => onClick?.(level.price)}
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

export function Orderbook() {
  const { orderbook } = useTradingStore()

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
      {/* Header */}
      <div className="border-b border-border px-3 py-2">
        <h3 className="text-sm font-semibold text-text-primary">Order Book</h3>
      </div>

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
  )
}
