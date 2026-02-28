'use client'

import { useEffect, useState, useCallback } from 'react'
import { useWallet } from '@/lib/useWallet'
import { getOrders, convertPrice, convertSize, type ApiOrder } from '@/lib/api'

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

  const fetchOrders = useCallback(async () => {
    if (!wallet.address) return

    setIsLoading(true)
    try {
      const apiOrders = await getOrders(wallet.address)

      // Filter to only show historical orders (filled/cancelled)
      const history: HistoricalOrder[] = apiOrders
        .filter(o => o.status === 'filled' || o.status === 'cancelled')
        .map((o: ApiOrder) => ({
          id: o.id,
          timestamp: o.timestamp,
          symbol: o.symbol,
          side: o.side as 'buy' | 'sell',
          type: o.type as 'limit' | 'market' | 'stop',
          price: convertPrice(o.price),
          size: convertSize(o.size),
          filled: convertSize(o.filled),
          status: o.status as 'filled' | 'cancelled'
        }))
        .sort((a, b) => b.timestamp - a.timestamp) // Most recent first

      setOrders(history)
    } catch (error) {
      console.error('[order-history] Failed to fetch:', error)
      setOrders([])
    } finally {
      setIsLoading(false)
    }
  }, [wallet.address])

  useEffect(() => {
    if (!wallet.isConnected || !wallet.address) {
      setOrders([])
      return
    }

    fetchOrders()
    // Refresh every 10 seconds
    const interval = setInterval(fetchOrders, 10000)
    return () => clearInterval(interval)
  }, [wallet.isConnected, wallet.address, fetchOrders])

  const getStatusColor = (status: HistoricalOrder['status']) => {
    switch (status) {
      case 'filled':
        return 'text-long'
      case 'cancelled':
        return 'text-text-muted'
      case 'expired':
        return 'text-warning'
      case 'rejected':
        return 'text-short'
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
                <td className={`px-4 py-2 font-semibold ${isBuy ? 'text-long' : 'text-short'}`}>
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
