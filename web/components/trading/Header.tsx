'use client'

import { useTradingStore } from '@/lib/store'
import { useWallet } from '@/lib/useWallet'

export function Header() {
  const { selectedSymbol, currentPrice } = useTradingStore()
  const wallet = useWallet()

  // Calculate 24h change (mock for now)
  const priceChange24h = 1234.56
  const priceChangePercent = 2.53
  const isPositive = priceChangePercent >= 0

  return (
    <header className="border-b border-border bg-bg-secondary">
      <div className="flex items-center justify-between px-6 py-3">
        {/* Left: Logo + Market Info */}
        <div className="flex items-center gap-8">
          <div className="flex items-center gap-3">
            <div className="text-xl font-bold text-text-primary">HyperLicked</div>
          </div>

          <div className="h-8 w-px bg-border" />

          {/* Market selector */}
          <div className="flex items-center gap-6">
            <div>
              <div className="text-sm font-medium text-text-primary">{selectedSymbol}</div>
              <div className="text-xs text-text-muted">Perpetual</div>
            </div>

            {/* Mark Price */}
            <div>
              <div className="text-xs text-text-muted">Mark Price</div>
              <div className={`text-lg font-mono font-semibold ${isPositive ? 'text-green-buy' : 'text-red-sell'}`}>
                ${currentPrice.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
              </div>
            </div>

            {/* 24h Change */}
            <div>
              <div className="text-xs text-text-muted">24h Change</div>
              <div className={`text-sm font-mono ${isPositive ? 'text-green-buy' : 'text-red-sell'}`}>
                {isPositive ? '+' : ''}{priceChangePercent.toFixed(2)}%
                <span className="ml-1 text-xs">
                  ({isPositive ? '+' : ''}${priceChange24h.toLocaleString()})
                </span>
              </div>
            </div>

            {/* 24h Volume */}
            <div>
              <div className="text-xs text-text-muted">24h Volume</div>
              <div className="text-sm font-mono text-text-primary">$1.2B</div>
            </div>
          </div>
        </div>

        {/* Right: Wallet */}
        <div>
          {wallet.isConnected && wallet.address ? (
            <div className="flex items-center gap-3">
              {/* Wallet indicator */}
              <div className="flex items-center gap-2 rounded border border-border bg-bg-tertiary px-3 py-2">
                {wallet.isRabby && (
                  <div className="text-xs font-semibold text-accent">🐰 Rabby</div>
                )}
                <div className="text-sm font-mono text-text-primary">
                  {wallet.address.slice(0, 6)}...{wallet.address.slice(-4)}
                </div>
              </div>
              {/* Disconnect button */}
              <button
                onClick={() => wallet.disconnect()}
                className="rounded border border-border bg-bg-tertiary px-3 py-2 text-xs text-text-muted transition-colors hover:bg-bg-tertiary/80 hover:text-red-sell"
              >
                Disconnect
              </button>
            </div>
          ) : (
            <button
              onClick={() => wallet.connect()}
              className="rounded border border-accent bg-bg-tertiary px-4 py-2 text-sm font-medium text-accent transition-colors hover:bg-accent hover:text-white"
            >
              Connect {wallet.isRabby ? 'Rabby' : 'Wallet'}
            </button>
          )}
        </div>
      </div>
    </header>
  )
}
