'use client'

import { useState, useEffect, useCallback } from 'react'
import { useTradingStore } from '@/lib/store'
import { useWallet } from '@/lib/useWallet'
import { toast } from '@/components/ui/Toast'
import { getTriggerOrders, cancelTriggerOrder, convertPrice, type ApiTriggerOrder } from '@/lib/api'

interface PositionTriggers {
  tp?: ApiTriggerOrder
  sl?: ApiTriggerOrder
}

export function Positions() {
  const { positions } = useTradingStore()
  const wallet = useWallet()
  const [triggersBySymbol, setTriggersBySymbol] = useState<Record<string, PositionTriggers>>({})

  // Fetch trigger orders for this user
  const fetchTriggerOrders = useCallback(async () => {
    if (!wallet.address) return

    try {
      const triggers = await getTriggerOrders(wallet.address)
      const bySymbol: Record<string, PositionTriggers> = {}

      for (const trigger of triggers) {
        if (trigger.status !== 'pending') continue
        if (!bySymbol[trigger.symbol]) {
          bySymbol[trigger.symbol] = {}
        }
        if (trigger.triggerType === 'tp') {
          bySymbol[trigger.symbol].tp = trigger
        } else if (trigger.triggerType === 'sl') {
          bySymbol[trigger.symbol].sl = trigger
        }
      }

      setTriggersBySymbol(bySymbol)
    } catch (err) {
      console.error('[positions] Failed to fetch trigger orders:', err)
    }
  }, [wallet.address])

  useEffect(() => {
    if (!wallet.address) return
    fetchTriggerOrders()
    const interval = setInterval(fetchTriggerOrders, 5000)
    return () => clearInterval(interval)
  }, [wallet.address, fetchTriggerOrders])

  const handleClose = (symbol: string, size: number) => {
    console.log('Closing position:', symbol, size)
    // TODO: Submit market order to close
    toast.info('Closing Position', `Closing ${Math.abs(size).toFixed(4)} ${symbol}`)
  }

  const handleCancelTrigger = async (triggerOrderId: string, symbol: string, type: 'tp' | 'sl') => {
    if (!wallet.address) return

    try {
      await cancelTriggerOrder(triggerOrderId, wallet.address)
      toast.success(`${type.toUpperCase()} Cancelled`, `Cancelled ${type === 'tp' ? 'Take Profit' : 'Stop Loss'} for ${symbol}`)
      fetchTriggerOrders()
    } catch (err: any) {
      toast.error('Cancel Failed', err.message)
    }
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
            <caption className="sr-only">Open positions with entry price, PnL, and actions</caption>
            <thead className="sticky top-0 border-b border-border bg-bg-secondary">
              <tr className="text-text-muted">
                <th className="px-4 py-2 text-left font-medium">Symbol</th>
                <th className="px-4 py-2 text-left font-medium">Side</th>
                <th className="px-4 py-2 text-right font-medium">Size</th>
                <th className="px-4 py-2 text-right font-medium">Entry Price</th>
                <th className="px-4 py-2 text-right font-medium">Mark Price</th>
                <th className="px-4 py-2 text-right font-medium">Liq. Price</th>
                <th className="px-4 py-2 text-right font-medium">TP</th>
                <th className="px-4 py-2 text-right font-medium">SL</th>
                <th className="px-4 py-2 text-right font-medium">Unrealized PnL</th>
                <th className="px-4 py-2 text-center font-medium">Action</th>
              </tr>
            </thead>
            <tbody>
              {positions.map((position) => {
                const isLong = position.size > 0
                const isProfitable = position.unrealizedPnl > 0
                const pnlPercent = ((position.unrealizedPnl / position.margin) * 100)
                const triggers = triggersBySymbol[position.symbol] || {}

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
                    {/* Take Profit */}
                    <td className="px-4 py-2 text-right">
                      {triggers.tp ? (
                        <div className="flex items-center justify-end gap-1">
                          <span className="font-mono text-green-buy">
                            ${convertPrice(triggers.tp.triggerPrice).toLocaleString('en-US', { minimumFractionDigits: 2 })}
                          </span>
                          <button
                            onClick={() => handleCancelTrigger(triggers.tp!.id, position.symbol, 'tp')}
                            className="text-text-muted hover:text-red-sell"
                            title="Cancel TP"
                          >
                            ×
                          </button>
                        </div>
                      ) : (
                        <span className="text-text-muted">--</span>
                      )}
                    </td>
                    {/* Stop Loss */}
                    <td className="px-4 py-2 text-right">
                      {triggers.sl ? (
                        <div className="flex items-center justify-end gap-1">
                          <span className="font-mono text-red-sell">
                            ${convertPrice(triggers.sl.triggerPrice).toLocaleString('en-US', { minimumFractionDigits: 2 })}
                          </span>
                          <button
                            onClick={() => handleCancelTrigger(triggers.sl!.id, position.symbol, 'sl')}
                            className="text-text-muted hover:text-red-sell"
                            title="Cancel SL"
                          >
                            ×
                          </button>
                        </div>
                      ) : (
                        <span className="text-text-muted">--</span>
                      )}
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
