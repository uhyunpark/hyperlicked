'use client'

import { useEffect, useState } from 'react'
import { useWallet } from '@/lib/useWallet'
import { useTradingStore } from '@/lib/store'
import { getUserFills, ApiFill, convertPrice, convertSize } from '@/lib/api'
import type { UserFill } from '@/lib/types'

interface DisplayTrade {
  id: string
  timestamp: number
  symbol: string
  side: 'buy' | 'sell'
  price: number
  size: number
  tradeValue: number
  fee: number
  isMaker: boolean
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
  const selectedSymbol = useTradingStore((s) => s.selectedSymbol)
  const userFills = useTradingStore((s) => s.userFills)
  const setUserFills = useTradingStore((s) => s.setUserFills)
  const [isLoading, setIsLoading] = useState(false)
  const [initialLoadDone, setInitialLoadDone] = useState(false)

  // Fetch initial fills on mount/wallet connect
  useEffect(() => {
    if (!wallet.isConnected || !wallet.address) {
      setInitialLoadDone(false)
      return
    }

    const address = wallet.address

    const fetchInitialFills = async () => {
      setIsLoading(true)
      try {
        const data = await getUserFills(address, 100)
        const formatted: UserFill[] = data.map((f: ApiFill) => ({
          id: f.id,
          symbol: f.symbol,
          side: f.side as 'buy' | 'sell',
          price: convertPrice(f.price),
          size: convertSize(f.size),
          fee: convertPrice(f.fee),
          isMaker: f.isMaker,
          timestamp: f.timestamp,
        }))
        setUserFills(formatted)
        setInitialLoadDone(true)
      } catch (error) {
        console.error('[trade-history] Failed to fetch:', error)
      } finally {
        setIsLoading(false)
      }
    }

    fetchInitialFills()
  }, [wallet.isConnected, wallet.address, setUserFills])

  // Filter fills by selected symbol and convert to display format
  const displayTrades: DisplayTrade[] = userFills
    .filter(f => f.symbol === selectedSymbol)
    .map(f => ({
      id: f.id,
      timestamp: f.timestamp,
      symbol: f.symbol,
      side: f.side,
      price: f.price,
      size: f.size,
      tradeValue: f.price * f.size,
      fee: f.fee,
      isMaker: f.isMaker,
    }))

  if (!wallet.isConnected) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Connect wallet to view trade history
      </div>
    )
  }

  if (isLoading && !initialLoadDone) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Loading trade history...
      </div>
    )
  }

  if (displayTrades.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        No trade history for {selectedSymbol}
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
            <th className="px-4 py-2 text-right font-medium">Role</th>
          </tr>
        </thead>
        <tbody>
          {displayTrades.map((trade) => {
            const isBuy = trade.side === 'buy'

            return (
              <tr
                key={trade.id}
                className="border-b border-border/50 transition-colors hover:bg-bg-tertiary"
              >
                <td className="px-4 py-2 text-text-muted">{formatTimestamp(trade.timestamp)}</td>
                <td className="px-4 py-2 font-medium text-text-primary">{trade.symbol}</td>
                <td className={`px-4 py-2 font-semibold ${isBuy ? 'text-long' : 'text-short'}`}>
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
                <td className="px-4 py-2 text-right font-mono text-text-muted">
                  {trade.isMaker ? 'Maker' : 'Taker'}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}
