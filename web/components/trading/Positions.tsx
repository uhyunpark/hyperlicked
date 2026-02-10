'use client'

import { useState, useEffect, useCallback, useMemo, memo } from 'react'
import { useTradingStore } from '@/lib/store'
import { useWallet, type OrderToSign, type CancelTriggerOrderToSign } from '@/lib/useWallet'
import { toast } from '@/components/ui/Toast'
import {
  getTriggerOrders,
  cancelTriggerOrder,
  convertPrice,
  convertSize,
  convertToApiPrice,
  convertToApiSize,
  submitSignedTransaction,
  getNonce,
  type ApiTriggerOrder
} from '@/lib/api'
import type { TriggerOrder } from '@/lib/types'

interface PositionTriggers {
  tp?: TriggerOrder
  sl?: TriggerOrder
}

function PositionsInner() {
  const positions = useTradingStore((s) => s.positions)
  const markPrices = useTradingStore((s) => s.markPrices)
  const triggerOrders = useTradingStore((s) => s.triggerOrders)
  const setTriggerOrders = useTradingStore((s) => s.setTriggerOrders)
  const wallet = useWallet()

  // Derive trigger orders by symbol from store
  const triggersBySymbol = useMemo(() => {
    const bySymbol: Record<string, PositionTriggers> = {}
    for (const trigger of triggerOrders) {
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
    return bySymbol
  }, [triggerOrders])

  // Safe display helper - returns '--' for non-finite values
  const displayValue = (value: number, decimals: number = 2, prefix: string = ''): string => {
    if (!Number.isFinite(value)) return '--'
    return prefix + value.toFixed(decimals)
  }

  // Format currency with locale string, safe for NaN
  const displayCurrency = (value: number): string => {
    if (!Number.isFinite(value)) return '--'
    return '$' + value.toLocaleString('en-US', { minimumFractionDigits: 2 })
  }

  // Calculate unrealized PnL using realtime mark prices
  const getRealtimePnL = (position: typeof positions[0]) => {
    // Use getRealtimeMarkPrice which handles 0/undefined properly
    const currentMarkPrice = getRealtimeMarkPrice(position)
    const entryPrice = position.entryPrice ?? 0
    const size = position.size ?? 0
    // Guard against NaN from invalid inputs
    if (!Number.isFinite(currentMarkPrice) || !Number.isFinite(entryPrice) || !Number.isFinite(size)) {
      return 0
    }
    // PnL = (markPrice - entryPrice) * size
    return (currentMarkPrice - entryPrice) * size
  }

  // Get realtime mark price for a position
  const getRealtimeMarkPrice = (position: typeof positions[0]) => {
    // Prefer realtime markPrices from assetCtx updates
    // Fall back to position.markPrice only if it's non-zero
    // If both are 0/undefined, use entry price to avoid $0 flicker
    const realtimePrice = markPrices[position.symbol]
    if (realtimePrice && Number.isFinite(realtimePrice) && realtimePrice > 0) {
      return realtimePrice
    }
    const posMarkPrice = position.markPrice
    if (posMarkPrice && Number.isFinite(posMarkPrice) && posMarkPrice > 0) {
      return posMarkPrice
    }
    // Last resort: use entry price (better than showing $0)
    return position.entryPrice ?? 0
  }

  // Fetch trigger orders as backup (30s interval instead of 5s)
  const fetchTriggerOrders = useCallback(async () => {
    if (!wallet.address) return

    try {
      const triggers = await getTriggerOrders(wallet.address)
      // Convert API trigger orders to store format
      const storeTriggers: TriggerOrder[] = triggers
        .filter(t => t.status === 'pending')
        .map(t => ({
          id: t.id,
          cloid: t.cloid,
          symbol: t.symbol,
          triggerType: t.triggerType as 'sl' | 'tp',
          side: t.side as 'buy' | 'sell',
          triggerPrice: convertPrice(t.triggerPrice),
          size: convertSize(t.size),
          limitPrice: t.limitPrice ? convertPrice(t.limitPrice) : undefined,
          status: t.status as 'pending' | 'triggered' | 'cancelled' | 'failed',
          timestamp: t.timestamp,
        }))
      setTriggerOrders(storeTriggers)
    } catch (err) {
      console.error('[positions] Failed to fetch trigger orders:', err)
    }
  }, [wallet.address, setTriggerOrders])

  // Fetch on mount and as backup every 30s (primary updates come via WebSocket)
  useEffect(() => {
    if (!wallet.address) return
    fetchTriggerOrders()
    const interval = setInterval(fetchTriggerOrders, 30000)  // 30s backup instead of 5s
    return () => clearInterval(interval)
  }, [wallet.address, fetchTriggerOrders])

  const handleClose = useCallback(async (symbol: string, size: number, markPrice: number) => {
    if (!wallet.address) {
      toast.warning('Not Connected', 'Please connect your wallet first')
      return
    }

    if (!wallet.tradingEnabled) {
      toast.warning('Trading Disabled', 'Please enable trading first')
      return
    }

    try {
      // Get fresh nonce from server
      const nonceData = await getNonce(wallet.address)
      const currentNonce = nonceData.nonce

      // Close = market order in opposite direction with reduce_only
      // Long position (size > 0) → Sell (side = 2)
      // Short position (size < 0) → Buy (side = 1)
      const side = size > 0 ? 2 : 1
      const orderSize = Math.abs(size)

      // Use sweep price to ensure execution
      // Sell: Use minimum price (0.01) to sweep all bids
      // Buy: Use very high price (2x mark) to sweep all asks
      const orderPrice = side === 2 ? 0.01 : markPrice * 2

      const orderToSign: OrderToSign = {
        symbol,
        side,
        type: 2, // IOC (immediate-or-cancel) for market order
        price: convertToApiPrice(orderPrice).toString(),
        qty: convertToApiSize(orderSize).toString(),
        nonce: currentNonce.toString(),
        deadline: '0',
        leverage: 1, // Leverage doesn't matter for reduce-only
        owner: wallet.address,
        reduce_only: true
      }

      const { signature, agentMode, delegationId } = await wallet.signOrderSmart(orderToSign)

      const signedTx = {
        type: 'order' as const,
        order: orderToSign,
        signature,
        agent_mode: agentMode,
        delegation_id: delegationId
      }

      const response = await submitSignedTransaction(signedTx)

      if (response.status === 'submitted') {
        toast.success('Position Closed', `Closed ${orderSize.toFixed(4)} ${symbol}`)
      } else {
        toast.error('Close Failed', response.message || 'Unknown error')
      }
    } catch (error: any) {
      console.error('[position-close] Error:', error)
      toast.error('Close Failed', error.message || 'Unknown error')
    }
  }, [wallet.address, wallet.tradingEnabled, wallet.signOrderSmart])

  const handleCancelTrigger = useCallback(async (triggerOrderId: string, symbol: string, type: 'tp' | 'sl') => {
    if (!wallet.address) return

    try {
      // Get fresh nonce for signature
      const nonceData = await getNonce(wallet.address)
      const cancelToSign: CancelTriggerOrderToSign = {
        triggerOrderId,
        symbol,
        nonce: nonceData.nonce.toString(),
        owner: wallet.address,
      }
      const { signature, agentMode, delegationId } = await wallet.signCancelTriggerOrderSmart(cancelToSign)
      await cancelTriggerOrder({
        cancel: {
          triggerOrderId,
          symbol,
          nonce: nonceData.nonce.toString(),
          owner: wallet.address,
        },
        signature,
        agent_mode: agentMode,
        delegation_id: delegationId,
      })
      toast.success(`${type.toUpperCase()} Cancelled`, `Cancelled ${type === 'tp' ? 'Take Profit' : 'Stop Loss'} for ${symbol}`)
      // WebSocket will handle the removal, but also update local state immediately
      useTradingStore.getState().removeTriggerOrder(triggerOrderId)
    } catch (err: any) {
      toast.error('Cancel Failed', err.message)
    }
  }, [wallet.address, wallet.signCancelTriggerOrderSmart])

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
                const realtimePnL = getRealtimePnL(position)
                const realtimeMarkPrice = getRealtimeMarkPrice(position)
                const isProfitable = realtimePnL > 0
                // Protect against division by zero if margin is 0
                const pnlPercent = position.margin > 0 ? ((realtimePnL / position.margin) * 100) : 0
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
                      {displayValue(Math.abs(position.size ?? 0), 4)}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      {displayCurrency(position.entryPrice ?? 0)}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      {displayCurrency(realtimeMarkPrice)}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-red-sell">
                      {displayCurrency(position.liquidationPrice ?? 0)}
                    </td>
                    {/* Take Profit */}
                    <td className="px-4 py-2 text-right">
                      {triggers.tp ? (
                        <div className="flex items-center justify-end gap-1">
                          <span className="font-mono text-green-buy">
                            ${triggers.tp.triggerPrice.toLocaleString('en-US', { minimumFractionDigits: 2 })}
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
                            ${triggers.sl.triggerPrice.toLocaleString('en-US', { minimumFractionDigits: 2 })}
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
                      {Number.isFinite(realtimePnL) ? (
                        <>
                          {isProfitable ? '+' : ''}${realtimePnL.toFixed(2)}
                          <div className="text-xs">
                            ({isProfitable ? '+' : ''}{Number.isFinite(pnlPercent) ? pnlPercent.toFixed(2) : '0.00'}%)
                          </div>
                        </>
                      ) : (
                        <span className="text-text-muted">--</span>
                      )}
                    </td>
                    <td className="px-4 py-2 text-center">
                      <button
                        onClick={() => handleClose(position.symbol, position.size, realtimeMarkPrice)}
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
      {positions.length > 0 && (() => {
        const totalPnL = positions.reduce((sum, p) => sum + getRealtimePnL(p), 0)
        const safeTotalPnL = Number.isFinite(totalPnL) ? totalPnL : 0
        return (
          <div className="border-t border-border bg-bg-primary px-4 py-2">
            <div className="flex justify-between text-xs">
              <span className="text-text-muted">Total Unrealized PnL:</span>
              <span className={`font-mono font-semibold ${
                safeTotalPnL > 0 ? 'text-green-buy' : 'text-red-sell'
              }`}>
                ${safeTotalPnL.toFixed(2)}
              </span>
            </div>
          </div>
        )
      })()}
    </div>
  )
}

// Export memoized component to prevent re-renders from parent state changes
export const Positions = memo(PositionsInner)
