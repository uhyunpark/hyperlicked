'use client'

import { useTradingStore } from '@/lib/store'

export function RecentTrades() {
  const { trades } = useTradingStore()

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Header */}
      <div className="border-b border-border px-4 py-2">
        <h3 className="text-sm font-semibold text-text-primary">Recent Trades</h3>
      </div>

      {/* Column headers */}
      <div className="flex justify-between border-b border-border px-4 py-1 text-xs text-text-muted">
        <div>Time</div>
        <div>Price (USDT)</div>
        <div>Size (BTC)</div>
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
              className="flex justify-between px-4 py-1 text-xs font-mono transition-colors hover:bg-bg-tertiary"
            >
              <div className="text-text-muted">{timeStr}</div>
              <div className={isBuy ? 'text-green-buy' : 'text-red-sell'}>
                {trade.price.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
              </div>
              <div className="text-text-primary">{trade.size.toFixed(4)}</div>
            </div>
          )
        })}
      </div>
    </div>
  )
}
