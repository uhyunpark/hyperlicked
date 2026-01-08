'use client'

import { useEffect, useState } from 'react'
import { useWallet } from '@/lib/useWallet'

interface FundingPayment {
  id: string
  timestamp: number
  symbol: string
  fundingRate: number
  positionSize: number
  payment: number
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

export function FundingHistory() {
  const wallet = useWallet()
  const [payments, setPayments] = useState<FundingPayment[]>([])
  const [isLoading, setIsLoading] = useState(false)

  useEffect(() => {
    if (!wallet.isConnected || !wallet.address) {
      setPayments([])
      return
    }

    const fetchPayments = async () => {
      setIsLoading(true)
      try {
        // TODO: Replace with actual API call when backend endpoint is ready
        // const response = await fetch(`/account/${wallet.address}/funding`)
        // const data = await response.json()

        // Mock data for now
        setPayments([])
      } catch (error) {
        console.error('[funding-history] Failed to fetch:', error)
        setPayments([])
      } finally {
        setIsLoading(false)
      }
    }

    fetchPayments()
  }, [wallet.isConnected, wallet.address])

  if (!wallet.isConnected) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Connect wallet to view funding history
      </div>
    )
  }

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        Loading funding history...
      </div>
    )
  }

  if (payments.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-text-muted">
        No funding payments yet
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
            <th className="px-4 py-2 text-right font-medium">Funding Rate</th>
            <th className="px-4 py-2 text-right font-medium">Position Size</th>
            <th className="px-4 py-2 text-right font-medium">Payment</th>
          </tr>
        </thead>
        <tbody>
          {payments.map((payment) => {
            const isPositive = payment.payment > 0

            return (
              <tr
                key={payment.id}
                className="border-b border-border/50 transition-colors hover:bg-bg-tertiary"
              >
                <td className="px-4 py-2 text-text-muted">{formatTimestamp(payment.timestamp)}</td>
                <td className="px-4 py-2 font-medium text-text-primary">{payment.symbol}</td>
                <td className={`px-4 py-2 text-right font-mono ${
                  payment.fundingRate >= 0 ? 'text-green-buy' : 'text-red-sell'
                }`}>
                  {payment.fundingRate >= 0 ? '+' : ''}{(payment.fundingRate * 100).toFixed(4)}%
                </td>
                <td className="px-4 py-2 text-right font-mono text-text-primary">
                  {payment.positionSize.toFixed(4)}
                </td>
                <td className={`px-4 py-2 text-right font-mono font-semibold ${
                  isPositive ? 'text-green-buy' : 'text-red-sell'
                }`}>
                  {isPositive ? '+' : ''}{payment.payment.toFixed(2)} USDC
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>

      {/* Summary */}
      {payments.length > 0 && (
        <div className="border-t border-border bg-bg-primary px-4 py-2">
          <div className="flex justify-between text-xs">
            <span className="text-text-muted">Total Funding Received:</span>
            <span className={`font-mono font-semibold ${
              payments.reduce((sum, p) => sum + p.payment, 0) >= 0 ? 'text-green-buy' : 'text-red-sell'
            }`}>
              ${payments.reduce((sum, p) => sum + p.payment, 0).toFixed(2)}
            </span>
          </div>
        </div>
      )}
    </div>
  )
}
