'use client'

import type { AccountData } from '@/lib/useAccountData'

interface AccountInfoProps {
  account: AccountData | null
  isLoading: boolean
  leverage: number
  marginRatio: number
  realtimeEquity: number
  realtimePnL: number
}

export function AccountInfo({
  account,
  isLoading,
  leverage,
  marginRatio,
  realtimeEquity,
  realtimePnL
}: AccountInfoProps) {
  return (
    <div className="mt-4 rounded border border-border bg-bg-primary p-3">
      {/* Account Equity Header */}
      <div className="mb-3 flex items-center justify-between">
        <span className="text-xs font-medium text-text-muted">Account Equity</span>
        {isLoading ? (
          <span className="text-sm text-text-muted">Loading...</span>
        ) : (
          <span className="font-mono text-lg font-semibold text-text-primary">
            ${realtimeEquity.toLocaleString('en-US', { minimumFractionDigits: 2 })}
          </span>
        )}
      </div>

      {/* Perps Overview */}
      {account && (
        <div className="space-y-2 border-t border-border pt-3">
          <div className="text-xs font-medium text-text-secondary">Perps Overview</div>
          <div className="space-y-1.5">
            <div className="flex justify-between text-xs">
              <span className="text-text-muted">Balance</span>
              <span className="font-mono text-text-primary">
                ${account.balance.toLocaleString('en-US', { minimumFractionDigits: 2 })}
              </span>
            </div>
            <div className="flex justify-between text-xs">
              <span className="text-text-muted">Positions</span>
              <span className="font-mono text-text-primary">
                ${account.lockedCollateral.toLocaleString('en-US', { minimumFractionDigits: 2 })}
              </span>
            </div>
            <div className="flex justify-between text-xs">
              <span className="text-text-muted">Unrealized PnL</span>
              <span className={`font-mono ${realtimePnL >= 0 ? 'text-green-buy' : 'text-red-sell'}`}>
                {realtimePnL >= 0 ? '+' : ''}${realtimePnL.toLocaleString('en-US', { minimumFractionDigits: 2 })}
              </span>
            </div>
            <div className="flex justify-between text-xs">
              <span className="text-text-muted">Cross Margin Ratio</span>
              <span className="font-mono text-text-primary">
                {marginRatio > 0 ? marginRatio.toFixed(2) : '--'}%
              </span>
            </div>
            <div className="flex justify-between text-xs">
              <span className="text-text-muted">Maintenance Margin</span>
              <span className="font-mono text-text-secondary">--</span>
            </div>
            <div className="flex justify-between text-xs">
              <span className="text-text-muted">Cross Account Leverage</span>
              <span className="font-mono text-text-primary">{leverage}x</span>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
