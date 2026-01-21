'use client'

import { useState, useEffect } from 'react'
import { useTradingStore } from '@/lib/store'
import { useWallet } from '@/lib/useWallet'
import { config, isDevelopment } from '@/lib/config'
import { toast } from '@/components/ui/Toast'
import { getFunding, getInsuranceFund, ApiFundingInfo, ApiInsuranceFund } from '@/lib/api'

interface NavTabProps {
  label: string
  active?: boolean
  disabled?: boolean
}

function NavTab({ label, active, disabled }: NavTabProps) {
  return (
    <button
      role="tab"
      aria-selected={active}
      aria-disabled={disabled}
      disabled={disabled}
      className={`relative px-4 py-2 text-sm font-medium transition-colors ${
        disabled
          ? 'cursor-not-allowed text-text-muted/40'
          : active
            ? 'text-text-primary'
            : 'text-text-muted hover:text-text-secondary'
      }`}
      title={disabled ? 'Coming Soon' : undefined}
    >
      {label}
      {active && (
        <div className="absolute bottom-0 left-0 right-0 h-0.5 bg-accent" />
      )}
    </button>
  )
}

// Format countdown from ms timestamp
function formatCountdown(targetMs: number): string {
  const now = Date.now()
  const diff = targetMs - now
  if (diff <= 0) return '0:00'

  const minutes = Math.floor(diff / 60000)
  const seconds = Math.floor((diff % 60000) / 1000)

  if (minutes >= 60) {
    const hours = Math.floor(minutes / 60)
    const mins = minutes % 60
    return `${hours}h ${mins}m`
  }
  return `${minutes}:${seconds.toString().padStart(2, '0')}`
}

export function Header() {
  const { selectedSymbol, currentPrice, isConnected: wsConnected } = useTradingStore()
  const wallet = useWallet()
  const [showWalletDropdown, setShowWalletDropdown] = useState(false)
  const [fundingInfo, setFundingInfo] = useState<ApiFundingInfo | null>(null)
  const [insuranceFund, setInsuranceFund] = useState<ApiInsuranceFund | null>(null)
  const [countdown, setCountdown] = useState('')

  // Fetch funding info and insurance fund
  useEffect(() => {
    const fetchData = async () => {
      try {
        const [funding, insurance] = await Promise.all([
          getFunding(selectedSymbol),
          getInsuranceFund()
        ])
        setFundingInfo(funding)
        setInsuranceFund(insurance)
      } catch (e) {
        // Silently fail - data will show as "--"
      }
    }

    fetchData()
    const interval = setInterval(fetchData, 10000) // Refresh every 10s
    return () => clearInterval(interval)
  }, [selectedSymbol])

  // Update countdown timer
  useEffect(() => {
    if (!fundingInfo?.nextFundingTime) return

    const updateCountdown = () => {
      setCountdown(formatCountdown(fundingInfo.nextFundingTime))
    }

    updateCountdown()
    const interval = setInterval(updateCountdown, 1000)
    return () => clearInterval(interval)
  }, [fundingInfo?.nextFundingTime])

  // Calculate 24h change (mock for now)
  const priceChange24h = 1234.56
  const priceChangePercent = 2.53
  const isPositive = priceChangePercent >= 0

  // Funding rate display
  const fundingRate = fundingInfo?.fundingRate ?? 0
  const fundingRatePercent = (fundingRate * 100).toFixed(4)
  const isFundingPositive = fundingRate > 0

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
                aria-label="Dismiss error"
              >
                ✕
              </button>
            )}
          </div>
        </div>
      )}

      {/* Top Row: Logo + Navigation + Wallet */}
      <div className="flex items-center justify-between border-b border-border px-6 py-2">
        {/* Left: Logo + Online indicator */}
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-2">
            <div
              className="h-2 w-2 rounded-full bg-green-500"
              title={wsConnected ? 'Online' : 'Connecting...'}
              role="status"
              aria-label={wsConnected ? 'Connection status: Online' : 'Connection status: Connecting'}
            />
            <div className="text-lg font-bold text-text-primary">HyperLicked</div>
            {isDevelopment && (
              <span className="rounded bg-yellow-500/20 px-2 py-0.5 text-xs font-medium text-yellow-500">
                DEV
              </span>
            )}
          </div>
        </div>

        {/* Center: Navigation */}
        <nav className="flex items-center" role="tablist" aria-label="Main navigation">
          <NavTab label="Trade" active />
          <NavTab label="Vaults" disabled />
          <NavTab label="Portfolio" disabled />
          <NavTab label="Staking" disabled />
        </nav>

        {/* Right: Wallet Address Dropdown */}
        <div className="relative">
          {wallet.isConnected && wallet.address ? (
            <>
              <button
                onClick={() => setShowWalletDropdown(!showWalletDropdown)}
                className="flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm font-mono font-medium text-white transition-opacity hover:opacity-90"
                aria-expanded={showWalletDropdown}
                aria-haspopup="menu"
                aria-label={`Wallet ${wallet.address.slice(0, 6)}...${wallet.address.slice(-4)}, click to ${showWalletDropdown ? 'close' : 'open'} menu`}
              >
                <span aria-hidden="true">{wallet.address.slice(0, 6)}...{wallet.address.slice(-4)}</span>
                <svg className="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 9l-7 7-7-7" />
                </svg>
              </button>
              {showWalletDropdown && (
                <>
                  {/* Backdrop to close dropdown on outside click */}
                  <div
                    className="fixed inset-0 z-10"
                    onClick={() => setShowWalletDropdown(false)}
                    aria-hidden="true"
                  />
                  <div
                    className="absolute right-0 top-full z-20 mt-1 min-w-[160px] rounded-lg border border-border bg-bg-primary shadow-lg"
                    role="menu"
                    aria-orientation="vertical"
                  >
                    <div className="border-b border-border px-4 py-2">
                      <div className="text-xs text-text-muted">Connected</div>
                      <div className="font-mono text-sm text-text-primary">
                        {wallet.address.slice(0, 8)}...{wallet.address.slice(-6)}
                      </div>
                    </div>
                    <button
                      onClick={() => {
                        wallet.disconnect()
                        setShowWalletDropdown(false)
                      }}
                      className="w-full px-4 py-2 text-left text-sm text-red-sell transition-colors hover:bg-red-sell/10"
                      role="menuitem"
                    >
                      Disconnect
                    </button>
                  </div>
                </>
              )}
            </>
          ) : (
            <button
              onClick={() => wallet.connect()}
              className="rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition-opacity hover:opacity-90"
            >
              Connect Wallet
            </button>
          )}
        </div>
      </div>

      {/* Second Row: Market Info + Wallet Actions */}
      <div className="flex items-center justify-between px-6 py-2">
        {/* Left: Market Info */}
        <div className="flex items-center gap-6">
          {/* Market selector */}
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

          {/* Funding Rate */}
          <div>
            <div className="text-xs text-text-muted">Funding / Countdown</div>
            <div className="flex items-center gap-2">
              <span className={`text-sm font-mono ${isFundingPositive ? 'text-red-sell' : 'text-green-buy'}`}>
                {isFundingPositive ? '+' : ''}{fundingRatePercent}%
              </span>
              <span className="text-xs text-text-muted">in {countdown || '--:--'}</span>
            </div>
          </div>

          {/* Insurance Fund */}
          <div>
            <div className="text-xs text-text-muted">Insurance Fund</div>
            <div className="text-sm font-mono text-text-primary">
              ${insuranceFund?.balance_usd?.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 }) ?? '--'}
            </div>
          </div>
        </div>

        {/* Right: Trading Status */}
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
                    aria-label="Disable trading"
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
            </div>
          ) : (
            <button
              onClick={() => wallet.connect()}
              className="rounded border border-accent bg-bg-tertiary px-4 py-2 text-sm font-medium text-accent transition-colors hover:bg-accent hover:text-white"
            >
              Connect Wallet
            </button>
          )}
        </div>
      </div>
    </header>
  )
}
