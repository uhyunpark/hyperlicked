'use client'

import { useTradingStore } from '@/lib/store'
import { useWallet } from '@/lib/useWallet'
import { config } from '@/lib/config'
import { toast } from '@/components/ui/Toast'

export function Header() {
  const { selectedSymbol, currentPrice } = useTradingStore()
  const wallet = useWallet()

  // Calculate 24h change (mock for now)
  const priceChange24h = 1234.56
  const priceChangePercent = 2.53
  const isPositive = priceChangePercent >= 0

  // Check if on wrong network
  const isWrongNetwork = wallet.isConnected && wallet.chainId !== config.network.chainId

  return (
    <header className="border-b border-border bg-bg-secondary">
      {/* Error/Warning Banner */}
      {(wallet.error || isWrongNetwork) && (
        <div className="flex items-center justify-between bg-red-sell/20 px-6 py-2">
          <div className="flex items-center gap-2">
            <span className="text-red-sell">⚠️</span>
            <span className="text-sm text-red-sell">
              {wallet.error || `Wrong network. Please switch to ${config.network.chainName}`}
            </span>
          </div>
          <div className="flex items-center gap-2">
            {isWrongNetwork && (
              <button
                onClick={() => wallet.switchNetwork(config.network.chainId)}
                className="rounded bg-red-sell px-3 py-1 text-xs font-medium text-white hover:bg-red-sell/80"
              >
                Switch Network
              </button>
            )}
            {wallet.needsReconnect && (
              <button
                onClick={() => {
                  wallet.clearError()
                  wallet.connect()
                }}
                className="rounded bg-accent px-3 py-1 text-xs font-medium text-white hover:bg-accent/80"
              >
                Reconnect
              </button>
            )}
            {wallet.error && !wallet.needsReconnect && (
              <button
                onClick={() => wallet.clearError()}
                className="text-xs text-red-sell hover:text-red-sell/80"
              >
                ✕
              </button>
            )}
          </div>
        </div>
      )}

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
              {/* Trading status */}
              {wallet.tradingEnabled ? (
                <div className="flex items-center gap-2 rounded border border-accent bg-accent/10 px-3 py-2">
                  <div className="h-2 w-2 rounded-full bg-accent animate-pulse" />
                  <div className="text-xs font-semibold text-accent">Trading Enabled</div>
                  <div className="text-xs text-text-muted">({wallet.delegationExpiry})</div>
                  <button
                    onClick={() => wallet.disableTrading()}
                    className="ml-2 text-xs text-text-muted hover:text-red-sell"
                  >
                    Disable
                  </button>
                </div>
              ) : (
                <button
                  onClick={async () => {
                    try {
                      await wallet.enableTrading(7)
                      toast.success('Trading Enabled', 'You can now trade without signing every order')
                    } catch (error: any) {
                      toast.error('Enable Trading Failed', error.message)
                    }
                  }}
                  className="rounded border border-accent bg-accent/10 px-4 py-2 text-sm font-semibold text-accent transition-colors hover:bg-accent/20"
                >
                  Enable Trading (7d)
                </button>
              )}

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
