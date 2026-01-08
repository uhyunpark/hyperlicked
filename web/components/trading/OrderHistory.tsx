'use client'

import { useEffect, useState } from 'react'
import { useWallet } from '@/lib/useWallet'

interface HistoricalOrder {
  id: string
  timestamp: number
  symbol: string
  side: 'buy' | 'sell'
  type: 'limit' | 'market' | 'stop'
  price: number
  size: number
  filled: number
  status: 'filled' | 'cancelled' | 'expired' | 'rejected'
}

// Format timestamp like Hyperliquid: "2025. 9. 20( 23H 18M 46S"
function formatTimestamp(timestamp: number) {
  const date = new Date(timestamp)
  const y = date.getFullYear()
  const m = date.getMonth() + 1
  const d = date.getDate()
  const h = date.getHours().toString().padStart(2, '0')
  const min = date.getMinutes().toString().padStart(2, '0')
  const s = date.getSeconds().toString().padStart(2, '0')
  return `${y}. ${m}. ${d}( ${h}H ${min}M ${s}S`
}

export function OrderHistory() {
  const wallet = useWallet()
  const [orders, setOrders] = useState<HistoricalOrder[]>([])
  const [isLoading, setIsLoading] = useState(false)

  useEffect(() => {
    if (!wallet.isConnected || !wallet.address) {
      setOrders([])
      return
    }

    const fetchOrders = async () => {
      setIsLoading(true)
      try {
        // TODO: Replace with actual API call when backend endpoint is ready
        // const response = await fetch(`/account/${wallet.address}/orders`)
        // const data = await response.json()

        // Mock data for now
        setOrders([])
      } catch (error) {
        console.error('[order-history] Failed to fetch:', error)
        setOrders([])
      } finally {
        setIsLoading(false)
      }
    }

    fetchOrders()
  }, [wallet.isConnected, wallet.address])

  const getStatusColor = (status: HistoricalOrder['status']) => {
    switch (status) {
      case 'filled':
        return 'text-green-buy'
      case 'cancelled':
        return 'text-text-muted'
      case 'expired':
        return 'text-yellow-500'
      case 'rejected':
        return 'text-red-sell'
      default:
        return 'text-text-muted'
    }
  }

  if (!wallet.isConnected) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Connect wallet to view order history
      </div>
    )
  }

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Loading order history...
      </div>
    )
  }

  if (orders.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        No order history
      </div>
    )
  }

  return (
    <div className="flex h-full flex-col overflow-x-auto">
      <table className="w-full text-xs">
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
          </tr>
        </thead>
        <tbody>
          {orders.map((order) => {
            const isBuy = order.side === 'buy'
            const fillPercent = (order.filled / order.size) * 100

            return (
              <tr
                key={order.id}
                className="border-b border-border/50 transition-colors hover:bg-bg-tertiary"
              >
                <td className="px-4 py-2 text-text-muted">{formatTimestamp(order.timestamp)}</td>
                <td className="px-4 py-2 font-medium text-text-primary">{order.symbol}</td>
                <td className={`px-4 py-2 font-semibold ${isBuy ? 'text-green-buy' : 'text-red-sell'}`}>
                  {isBuy ? 'Buy' : 'Sell'}
                </td>
                <td className="px-4 py-2 text-text-secondary capitalize">{order.type}</td>
                <td className="px-4 py-2 text-right font-mono text-text-primary">
                  ${order.price.toLocaleString('en-US', { minimumFractionDigits: 2 })}
                </td>
                <td className="px-4 py-2 text-right font-mono text-text-primary">
                  {order.size.toFixed(4)}
                </td>
                <td className="px-4 py-2 text-right font-mono text-text-secondary">
                  {fillPercent.toFixed(0)}%
                </td>
                <td className={`px-4 py-2 text-center font-medium capitalize ${getStatusColor(order.status)}`}>
                  {order.status}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}
