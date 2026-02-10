'use client'

import { useEffect, useState } from 'react'
import { useWallet } from '@/lib/useWallet'

interface Balance {
  token: string
  symbol: string
  available: number
  inOrder: number
  total: number
  usdValue: number
}

export function Balances() {
  const wallet = useWallet()
  const [balances, setBalances] = useState<Balance[]>([])
  const [isLoading, setIsLoading] = useState(false)

  useEffect(() => {
    if (!wallet.isConnected || !wallet.address) {
      setBalances([])
      return
    }

    const fetchBalances = async () => {
      setIsLoading(true)
      try {
        const { getAccount } = await import('@/lib/api')
        const data = await getAccount(wallet.address!)

        // Convert account data to balance format
        setBalances([
          {
            token: 'USDC',
            symbol: 'USDC',
            available: data.availableBalance / 100,
            inOrder: data.lockedCollateral / 100,
            total: data.balance / 100,
            usdValue: data.balance / 100
          }
        ])
      } catch (error) {
        console.error('[balances] Failed to fetch:', error)
        setBalances([])
      } finally {
        setIsLoading(false)
      }
    }

    fetchBalances()
    const interval = setInterval(fetchBalances, 10000)
    return () => clearInterval(interval)
  }, [wallet.isConnected, wallet.address])

  // Format timestamp like Hyperliquid
  const formatTimestamp = (date: Date) => {
    const y = date.getFullYear()
    const m = date.getMonth() + 1
    const d = date.getDate()
    const h = date.getHours()
    const min = date.getMinutes()
    const s = date.getSeconds()
    return `${y}. ${m}. ${d}( ${h}H ${min}M ${s}S`
  }

  if (!wallet.isConnected) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Connect wallet to view balances
      </div>
    )
  }

  if (isLoading && balances.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Loading balances...
      </div>
    )
  }

  return (
    <div className="flex h-full flex-col">
      <table className="w-full text-xs">
        <thead className="sticky top-0 border-b border-border bg-bg-secondary">
          <tr className="text-text-muted">
            <th className="px-4 py-2 text-left font-medium">Token</th>
            <th className="px-4 py-2 text-right font-medium">Available</th>
            <th className="px-4 py-2 text-right font-medium">In Order</th>
            <th className="px-4 py-2 text-right font-medium">Total</th>
            <th className="px-4 py-2 text-right font-medium">Value (USDC)</th>
          </tr>
        </thead>
        <tbody>
          {balances.map((balance) => (
            <tr
              key={balance.token}
              className="border-b border-border/50 transition-colors hover:bg-bg-tertiary"
            >
              <td className="px-4 py-2 font-medium text-text-primary">{balance.token}</td>
              <td className="px-4 py-2 text-right font-mono text-text-primary">
                {balance.available.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
              </td>
              <td className="px-4 py-2 text-right font-mono text-text-secondary">
                {balance.inOrder.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
              </td>
              <td className="px-4 py-2 text-right font-mono text-text-primary">
                {balance.total.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
              </td>
              <td className="px-4 py-2 text-right font-mono text-text-primary">
                ${balance.usdValue.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      {balances.length === 0 && (
        <div className="flex flex-1 items-center justify-center text-text-muted">
          No balances found
        </div>
      )}

      {/* Total */}
      {balances.length > 0 && (
        <div className="border-t border-border bg-bg-primary px-4 py-2">
          <div className="flex justify-between text-xs">
            <span className="text-text-muted">Total Value (USDC):</span>
            <span className="font-mono font-semibold text-text-primary">
              ${balances.reduce((sum, b) => sum + b.usdValue, 0).toLocaleString('en-US', { minimumFractionDigits: 2 })}
            </span>
          </div>
        </div>
      )}
    </div>
  )
}
