'use client'

import { useEffect, useState } from 'react'
import { useWallet } from '@/lib/useWallet'
import { useTradingStore } from '@/lib/store'
import { getTrades, ApiTrade, convertPrice, convertSize } from '@/lib/api'

interface Trade {
  id: string
  timestamp: number
  symbol: string
  side: 'buy' | 'sell'
  price: number
  size: number
  tradeValue: number
  fee: number
  closedPnL: number | null
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

export function TradeHistory() {
  const wallet = useWallet()
  const { selectedSymbol } = useTradingStore()
  const [trades, setTrades] = useState<Trade[]>([])
  const [isLoading, setIsLoading] = useState(false)

  useEffect(() => {
    if (!wallet.isConnected || !wallet.address) {
      setTrades([])
      return
    }

    const fetchTrades = async () => {
      setIsLoading(true)
      try {
        // Fetch recent market trades (user-specific trades API not yet available)
        const data = await getTrades(selectedSymbol, 50)
        const formatted: Trade[] = data.map((t: ApiTrade, i: number) => {
          const price = convertPrice(t.price)
          const size = convertSize(t.size)
          return {
            id: `${t.timestamp}-${i}`,
            timestamp: t.timestamp,
            symbol: selectedSymbol,
            side: t.side as 'buy' | 'sell',
            price,
            size,
            tradeValue: price * size,
            fee: 0, // Fee info not in API
            closedPnL: null, // PnL info not in API
          }
        })
        setTrades(formatted)
      } catch (error) {
        console.error('[trade-history] Failed to fetch:', error)
        setTrades([])
      } finally {
        setIsLoading(false)
      }
    }

    fetchTrades()
    const interval = setInterval(fetchTrades, 5000) // Refresh every 5s
    return () => clearInterval(interval)
  }, [wallet.isConnected, wallet.address, selectedSymbol])

  if (!wallet.isConnected) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Connect wallet to view trade history
      </div>
    )
  }

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Loading trade history...
      </div>
    )
  }

  if (trades.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        No trade history
      </div>
    )
  }

  return (
    <div className="flex h-full flex-col">
      <table className="w-full text-xs">
        <thead className="sticky top-0 border-b border-border bg-bg-secondary">
          <tr className="text-text-muted">
            <th className="px-4 py-2 text-left font-medium">Time</th>
            <th className="px-4 py-2 text-left font-medium">Coin</th>
            <th className="px-4 py-2 text-left font-medium">Direction</th>
            <th className="px-4 py-2 text-right font-medium">Price</th>
            <th className="px-4 py-2 text-right font-medium">Size</th>
            <th className="px-4 py-2 text-right font-medium">Trade Value</th>
            <th className="px-4 py-2 text-right font-medium">Fee</th>
            <th className="px-4 py-2 text-right font-medium">Closed PNL</th>
          </tr>
        </thead>
        <tbody>
          {trades.map((trade) => {
            const isBuy = trade.side === 'buy'
            const hasPnL = trade.closedPnL !== null
            const isProfitable = hasPnL && trade.closedPnL! > 0

            return (
              <tr
                key={trade.id}
                className="border-b border-border/50 transition-colors hover:bg-bg-tertiary"
              >
                <td className="px-4 py-2 text-text-muted">{formatTimestamp(trade.timestamp)}</td>
                <td className="px-4 py-2 font-medium text-text-primary">{trade.symbol}</td>
                <td className={`px-4 py-2 font-semibold ${isBuy ? 'text-green-buy' : 'text-red-sell'}`}>
                  {isBuy ? 'Buy' : 'Sell'}
                </td>
                <td className="px-4 py-2 text-right font-mono text-text-primary">
                  {trade.price.toLocaleString('en-US', { minimumFractionDigits: 2 })}
                </td>
                <td className="px-4 py-2 text-right font-mono text-text-primary">
                  {trade.size.toFixed(4)}
                </td>
                <td className="px-4 py-2 text-right font-mono text-text-primary">
                  {trade.tradeValue.toLocaleString('en-US', { minimumFractionDigits: 2 })} USDC
                </td>
                <td className="px-4 py-2 text-right font-mono text-text-secondary">
                  {trade.fee.toFixed(2)} USDC
                </td>
                <td className={`px-4 py-2 text-right font-mono ${
                  hasPnL
                    ? isProfitable
                      ? 'text-green-buy'
                      : 'text-red-sell'
                    : 'text-text-muted'
                }`}>
                  {hasPnL
                    ? `${isProfitable ? '+' : ''}${trade.closedPnL!.toFixed(2)} USDC`
                    : '-'}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}
