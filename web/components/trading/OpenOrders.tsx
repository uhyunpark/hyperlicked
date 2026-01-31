'use client'

import { useEffect, useCallback, memo } from 'react'
import { useTradingStore, useWalletStore } from '@/lib/store'
import { cancelOrder, getOrders, convertPrice, convertSize } from '@/lib/api'
import { toast } from '@/components/ui/Toast'

function OpenOrdersInner() {
  const { openOrders, setOpenOrders } = useTradingStore()
  const { address, isConnected } = useWalletStore()

  // Fetch orders via REST as fallback (in case WebSocket isn't working)
  const fetchOrders = useCallback(async () => {
    if (!address) return
    try {
      const ordersData = await getOrders(address)
      const orders = ordersData
        .filter(o => o.status === 'open' || o.status === 'partial')
        .map(o => {
          const size = convertSize(o.size)
          const filled = convertSize(o.filled)
          const orderType: 'limit' | 'market' = o.type === 'market' ? 'market' : 'limit'
          return {
            id: o.id,
            symbol: o.symbol,
            side: o.side as 'buy' | 'sell',
            type: orderType,
            price: convertPrice(o.price),
            size,
            filled,
            remaining: size - filled,
            status: o.status === 'partial' ? 'open' : o.status as 'open' | 'filled' | 'cancelled',
            timestamp: o.timestamp
          }
        })
      setOpenOrders(orders)
    } catch (error) {
      console.error('[open-orders] Failed to fetch:', error)
    }
  }, [address, setOpenOrders])

  // Fetch on mount and periodically
  useEffect(() => {
    if (!isConnected || !address) return

    // Fetch immediately
    fetchOrders()

    // Refresh every 5 seconds
    const interval = setInterval(fetchOrders, 5000)
    return () => clearInterval(interval)
  }, [isConnected, address, fetchOrders])

  const handleCancel = useCallback(async (orderId: string) => {
    if (!address) {
      toast.error('Not Connected', 'Please connect your wallet first')
      return
    }
    try {
      await cancelOrder(orderId, address)
      toast.success('Order Cancelled', `Order ${orderId} cancelled`)
    } catch (error: unknown) {
      const message = error instanceof Error ? error.message : 'Unknown error'
      toast.error('Cancel Failed', message)
    }
  }, [address])

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Header */}
      <div className="border-b border-border px-4 py-2">
        <h3 className="text-sm font-semibold text-text-primary">Open Orders</h3>
      </div>

      {/* Table */}
      <div className="flex-1 overflow-x-auto">
        {openOrders.length === 0 ? (
          <div className="flex h-full items-center justify-center">
            <p className="text-sm text-text-muted">No open orders</p>
          </div>
        ) : (
          <table className="w-full text-xs">
            <caption className="sr-only">Open orders with price, size, fill status, and actions</caption>
            <thead className="sticky top-0 border-b border-border bg-bg-secondary">
              <tr className="text-text-muted">
                <th className="px-4 py-2 text-left font-medium">Time</th>
                <th className="px-4 py-2 text-left font-medium">Symbol</th>
                <th className="px-4 py-2 text-left font-medium">Side</th>
                <th className="px-4 py-2 text-left font-medium">Type</th>
                <th className="px-4 py-2 text-right font-medium">Price</th>
                <th className="px-4 py-2 text-right font-medium">Size</th>
                <th className="px-4 py-2 text-right font-medium">Filled</th>
                <th className="px-4 py-2 text-center font-medium">Status</th>
                <th className="px-4 py-2 text-center font-medium">Action</th>
              </tr>
            </thead>
            <tbody>
              {openOrders.map((order) => {
                const time = new Date(order.timestamp)
                const timeStr = time.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit' })
                const isBuy = order.side === 'buy'
                const fillPercent = (order.filled / order.size) * 100

                return (
                  <tr
                    key={order.id}
                    className="border-b border-border/50 transition-colors hover:bg-bg-tertiary"
                  >
                    <td className="px-4 py-2 font-mono text-text-muted">{timeStr}</td>
                    <td className="px-4 py-2 font-medium text-text-primary">{order.symbol}</td>
                    <td className={`px-4 py-2 font-semibold ${isBuy ? 'text-green-buy' : 'text-red-sell'}`}>
                      {order.side.toUpperCase()}
                    </td>
                    <td className="px-4 py-2 text-text-secondary">{order.type}</td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      {order.price.toLocaleString('en-US', { minimumFractionDigits: 2 })}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      {order.size.toFixed(4)}
                    </td>
                    <td className="px-4 py-2 text-right font-mono text-text-primary">
                      {order.filled.toFixed(4)} ({fillPercent.toFixed(0)}%)
                    </td>
                    <td className="px-4 py-2 text-center">
                      <span className="rounded bg-accent/20 px-2 py-0.5 text-accent">
                        {order.status}
                      </span>
                    </td>
                    <td className="px-4 py-2 text-center">
                      <button
                        onClick={() => handleCancel(order.id)}
                        className="rounded border border-red-sell/30 bg-red-sell/10 px-2 py-1 text-red-sell transition-colors hover:bg-red-sell/20"
                      >
                        Cancel
                      </button>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        )}
      </div>
    </div>
  )
}

// Export memoized component to prevent re-renders from parent state changes
export const OpenOrders = memo(OpenOrdersInner)
