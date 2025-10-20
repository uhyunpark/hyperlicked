'use client'

import { useTradingStore } from '@/lib/store'

export function Positions() {
  const { positions } = useTradingStore()

  const handleClose = (symbol: string, size: number) => {
    console.log('Closing position:', symbol, size)
    // TODO: Submit market order to close
    alert(`Closing ${Math.abs(size).toFixed(4)} ${symbol}`)
  }

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Header */}
      <div className="border-b border-border px-4 py-2">
        <h3 className="text-sm font-semibold text-text-primary">Positions</h3>
      </div>

      {/* Table */}
      <div className="flex-1 overflow-x-auto">
        {positions.length === 0 ? (
          <div className="flex h-full items-center justify-center">
            <p className="text-sm text-text-muted">No open positions</p>
          </div>
        ) : (
          <table className="w-full text-xs">
            <thead className="sticky top-0 border-b border-border bg-bg-secondary">
              <tr className="text-text-muted">
                <th className="px-4 py-2 text-left font-medium">Symbol</th>
                <th className="px-4 py-2 text-left font-medium">Side</th>
                <th className="px-4 py-2 text-right font-medium">Size</th>
                <th className="px-4 py-2 text-right font-medium">Entry Price</th>
                <th className="px-4 py-2 text-right font-medium">Mark Price</th>
                <th className="px-4 py-2 text-right font-medium">Liq. Price</th>
                <th className="px-4 py-2 text-right font-medium">Margin</th>
                <th className="px-4 py-2 text-right font-medium">Leverage</th>
                <th className="px-4 py-2 text-right font-medium">Unrealized PnL</th>
                <th className="px-4 py-2 text-center font-medium">Action</th>
              </tr>
            </thead>
            <tbody>
              {positions.map((position) => {
                const isLong = position.size > 0
                const isProfitable = position.unrealizedPnl > 0
                const pnlPercent = ((position.unrealizedPnl / position.margin) * 100)

                return (
                  <tr
                    key={position.symbol}
                    className="border-b border-border/50 transition-colors hover:bg-bg-tertiary"
                  >
                    <td className="px-4 py-2 font-medium text-text-primary">{position.symbol}</td>
                    <td className={`px-4 py-2 font-semibold ${isLong ? 'text-green-buy' : 'text-red-sell'}`}>
                      {isLong ? 'LONG' : 'SHORT'}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      {Math.abs(position.size).toFixed(4)}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      ${position.entryPrice.toLocaleString('en-US', { minimumFractionDigits: 2 })}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      ${position.markPrice.toLocaleString('en-US', { minimumFractionDigits: 2 })}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-red-sell">
                      ${position.liquidationPrice.toLocaleString('en-US', { minimumFractionDigits: 2 })}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      ${position.margin.toLocaleString('en-US', { minimumFractionDigits: 2 })}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      {position.leverage}x
                    </td>
                    <td className={`px-4 py-2 text-right font-mono font-semibold ${isProfitable ? 'text-green-buy' : 'text-red-sell'}`}>
                      {isProfitable ? '+' : ''}${position.unrealizedPnl.toFixed(2)}
                      <div className="text-xs">
                        ({isProfitable ? '+' : ''}{pnlPercent.toFixed(2)}%)
                      </div>
                    </td>
                    <td className="px-4 py-2 text-center">
                      <button
                        onClick={() => handleClose(position.symbol, position.size)}
                        className="rounded border border-accent/30 bg-accent/10 px-2 py-1 text-accent transition-colors hover:bg-accent/20"
                      >
                        Close
                      </button>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        )}
      </div>

      {/* Summary */}
      {positions.length > 0 && (
        <div className="border-t border-border bg-bg-primary px-4 py-2">
          <div className="flex justify-between text-xs">
            <span className="text-text-muted">Total Unrealized PnL:</span>
            <span className={`font-mono font-semibold ${
              positions.reduce((sum, p) => sum + p.unrealizedPnl, 0) > 0 ? 'text-green-buy' : 'text-red-sell'
            }`}>
              ${positions.reduce((sum, p) => sum + p.unrealizedPnl, 0).toFixed(2)}
            </span>
          </div>
        </div>
      )}
    </div>
  )
}
