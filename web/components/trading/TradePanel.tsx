'use client'

import { useState, useEffect } from 'react'
import { useTradingStore } from '@/lib/store'
import { useWallet, type OrderToSign } from '@/lib/useWallet'
import { toast } from '@/components/ui/Toast'
import type { Side, OrderType } from '@/lib/types'

interface AccountData {
  balance: number
  lockedCollateral: number
  availableBalance: number
  unrealizedPnL: number
  totalEquity: number
}

export function TradePanel() {
  const { selectedSymbol, currentPrice } = useTradingStore()
  const wallet = useWallet()
  const [side, setSide] = useState<Side>('buy')
  const [orderType, setOrderType] = useState<OrderType>('limit')
  const [price, setPrice] = useState('')
  const [size, setSize] = useState('')
  const [leverage, setLeverage] = useState(10)
  const [nonce, setNonce] = useState(1)

  // Account data from API
  const [account, setAccount] = useState<AccountData | null>(null)
  const [isLoadingAccount, setIsLoadingAccount] = useState(false)

  // Fetch account data when wallet connects
  useEffect(() => {
    if (!wallet.isConnected || !wallet.address) {
      setAccount(null)
      return
    }

    const fetchAccount = async () => {
      setIsLoadingAccount(true)
      try {
        const { getAccount } = await import('@/lib/api')
        const data = await getAccount(wallet.address!)
        setAccount({
          balance: data.balance / 100, // cents to dollars
          lockedCollateral: data.lockedCollateral / 100,
          availableBalance: data.availableBalance / 100,
          unrealizedPnL: data.unrealizedPnL / 100,
          totalEquity: data.totalEquity / 100
        })
      } catch (error) {
        console.error('[account] Failed to fetch:', error)
        // Use default values on error
        setAccount({
          balance: 100000,
          lockedCollateral: 0,
          availableBalance: 100000,
          unrealizedPnL: 0,
          totalEquity: 100000
        })
      } finally {
        setIsLoadingAccount(false)
      }
    }

    fetchAccount()
    // Refresh every 10 seconds
    const interval = setInterval(fetchAccount, 10000)
    return () => clearInterval(interval)
  }, [wallet.isConnected, wallet.address])

  const accountBalance = account?.totalEquity ?? 0
  const availableBalance = account?.availableBalance ?? 0

  // Calculate order details
  const priceNum = parseFloat(price) || currentPrice
  const sizeNum = parseFloat(size) || 0
  const notional = priceNum * sizeNum
  const requiredMargin = notional / leverage
  const estimatedFee = notional * 0.0005

  const handleSubmit = async () => {
    // Validation
    if (!size || parseFloat(size) <= 0) {
      toast.warning('Invalid Size', 'Please enter a valid size')
      return
    }

    if (orderType === 'limit' && (!price || parseFloat(price) <= 0)) {
      toast.warning('Invalid Price', 'Please enter a valid price')
      return
    }

    if (!wallet.isConnected || !wallet.address) {
      toast.warning('Not Connected', 'Please connect your wallet first')
      return
    }

    try {
      const { submitSignedTransaction, convertToApiPrice, convertToApiSize } = await import('@/lib/api')

      const orderPrice = orderType === 'limit' ? parseFloat(price) : currentPrice
      const orderSize = parseFloat(size)

      const orderToSign: OrderToSign = {
        symbol: selectedSymbol,
        side: side === 'buy' ? 1 : 2,
        type: orderType === 'limit' ? 1 : (orderType === 'market' ? 2 : 3),
        price: convertToApiPrice(orderPrice).toString(),
        qty: convertToApiSize(orderSize).toString(),
        nonce: nonce.toString(),
        deadline: '0',
        leverage,
        owner: wallet.address
      }

      const { signature, agentMode, delegationId } = await wallet.signOrderSmart(orderToSign)

      const signedTx = {
        type: 'order' as const,
        order: orderToSign,
        signature,
        agent_mode: agentMode,
        delegation_id: delegationId
      }

      const response = await submitSignedTransaction(signedTx)

      if (response.status === 'submitted') {
        const method = agentMode ? 'Agent Key' : (wallet.isRabby ? 'Rabby' : 'MetaMask')
        toast.success('Order Submitted', `Order #${response.orderId} signed with ${method}`)
        setNonce(n => n + 1)
        setSize('')
        if (orderType === 'limit') setPrice('')
      } else {
        toast.error('Order Rejected', response.message || 'Unknown error')
      }
    } catch (error) {
      console.error('[order] Error:', error)
      toast.error('Order Failed', error instanceof Error ? error.message : 'Unknown error')
    }
  }

  // Calculate size for percentage buttons
  const calculateSizeForPercent = (percent: number) => {
    if (currentPrice <= 0 || availableBalance <= 0) return 0
    return (availableBalance * leverage * (percent / 100)) / currentPrice
  }

  const handleSizePercent = (percent: number) => {
    const calculatedSize = calculateSizeForPercent(percent)
    setSize(calculatedSize.toFixed(4))
  }

  // Calculate margin ratio
  const marginRatio = account && account.lockedCollateral > 0
    ? ((account.totalEquity / account.lockedCollateral) * 100)
    : 0

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Header */}
      <div className="border-b border-border px-4 py-2">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold text-text-primary">Trade</h3>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto p-4">
        {/* Order Type Tabs */}
        <div className="mb-3 flex gap-1 rounded border border-border bg-bg-primary p-1">
          <button
            className={`flex-1 rounded px-3 py-1.5 text-xs font-medium transition-colors ${
              orderType === 'limit'
                ? 'bg-bg-secondary text-text-primary'
                : 'text-text-muted hover:text-text-secondary'
            }`}
            onClick={() => setOrderType('limit')}
          >
            Limit
          </button>
          <button
            className={`flex-1 rounded px-3 py-1.5 text-xs font-medium transition-colors ${
              orderType === 'market'
                ? 'bg-bg-secondary text-text-primary'
                : 'text-text-muted hover:text-text-secondary'
            }`}
            onClick={() => setOrderType('market')}
          >
            Market
          </button>
          <button
            className={`flex-1 rounded px-3 py-1.5 text-xs font-medium transition-colors ${
              orderType === 'stop'
                ? 'bg-bg-secondary text-text-primary'
                : 'text-text-muted hover:text-text-secondary'
            }`}
            onClick={() => setOrderType('stop')}
          >
            Stop
          </button>
        </div>

        {/* Available to Trade */}
        <div className="mb-3 flex items-center justify-between rounded bg-bg-tertiary px-3 py-2">
          <span className="text-xs text-text-muted">Available to Trade</span>
          <span className="font-mono text-sm font-semibold text-text-primary">
            ${availableBalance.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })} USDC
          </span>
        </div>

        {/* Side Toggle */}
        <div className="mb-4 grid grid-cols-2 gap-2">
          <button
            className={`rounded py-2 text-sm font-semibold transition-colors ${
              side === 'buy'
                ? 'bg-green-buy text-white'
                : 'border border-border bg-bg-tertiary text-text-secondary hover:bg-bg-tertiary/80'
            }`}
            onClick={() => setSide('buy')}
          >
            Buy / Long
          </button>
          <button
            className={`rounded py-2 text-sm font-semibold transition-colors ${
              side === 'sell'
                ? 'bg-red-sell text-white'
                : 'border border-border bg-bg-tertiary text-text-secondary hover:bg-bg-tertiary/80'
            }`}
            onClick={() => setSide('sell')}
          >
            Sell / Short
          </button>
        </div>

        {/* Price Input (Limit only) */}
        {orderType === 'limit' && (
          <div className="mb-4">
            <label className="mb-1 block text-xs text-text-muted">Price (USDT)</label>
            <input
              type="number"
              value={price}
              onChange={(e) => setPrice(e.target.value)}
              placeholder={currentPrice.toFixed(2)}
              className="w-full rounded border border-border bg-bg-primary px-3 py-2 text-sm font-mono text-text-primary focus:border-accent focus:outline-none"
            />
          </div>
        )}

        {/* Size Input */}
        <div className="mb-3">
          <label className="mb-1 block text-xs text-text-muted">Size (BTC)</label>
          <input
            type="number"
            value={size}
            onChange={(e) => setSize(e.target.value)}
            placeholder="0.00"
            className="w-full rounded border border-border bg-bg-primary px-3 py-2 text-sm font-mono text-text-primary focus:border-accent focus:outline-none"
          />
          {/* Size Percentage Buttons */}
          <div className="mt-2 flex gap-1">
            {[25, 50, 75, 100].map((percent) => (
              <button
                key={percent}
                onClick={() => handleSizePercent(percent)}
                className="flex-1 rounded border border-border bg-bg-tertiary py-1 text-xs text-text-muted transition-colors hover:border-accent hover:text-accent"
              >
                {percent}%
              </button>
            ))}
          </div>
          <div className="mt-1 flex justify-between text-xs text-text-muted">
            <span>Notional: ${notional.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</span>
            <span>Max: {currentPrice > 0 ? (availableBalance * leverage / currentPrice).toFixed(4) : '0.0000'}</span>
          </div>
        </div>

        {/* Leverage Slider */}
        <div className="mb-4">
          <div className="mb-2 flex items-center justify-between">
            <label className="text-xs text-text-muted">Leverage</label>
            <div className="text-sm font-mono font-semibold text-text-primary">{leverage}x</div>
          </div>
          <input
            type="range"
            min="1"
            max="50"
            value={leverage}
            onChange={(e) => setLeverage(parseInt(e.target.value))}
            className="w-full accent-accent"
          />
          <div className="mt-1 flex justify-between text-xs text-text-muted">
            <span>1x</span>
            <span>25x</span>
            <span>50x</span>
          </div>
        </div>

        {/* Margin Info */}
        <div className="mb-4 rounded border border-border bg-bg-primary p-3">
          <div className="mb-2 flex justify-between text-xs">
            <span className="text-text-muted">Required Margin</span>
            <span className="font-mono text-text-primary">${requiredMargin.toFixed(2)}</span>
          </div>
          <div className="mb-2 flex justify-between text-xs">
            <span className="text-text-muted">Estimated Fee</span>
            <span className="font-mono text-text-primary">${estimatedFee.toFixed(2)}</span>
          </div>
          <div className="mb-2 flex justify-between text-xs">
            <span className="text-text-muted">Available Balance</span>
            <span className="font-mono text-text-primary">${availableBalance.toFixed(2)}</span>
          </div>
          <div className="border-t border-border pt-2">
            <div className="flex justify-between text-xs font-semibold">
              <span className="text-text-muted">Total Cost</span>
              <span className="font-mono text-text-primary">${(requiredMargin + estimatedFee).toFixed(2)}</span>
            </div>
          </div>
        </div>

        {/* Submit Button */}
        {wallet.isConnected ? (
          wallet.tradingEnabled ? (
            <button
              onClick={handleSubmit}
              className={`w-full rounded py-3 text-sm font-semibold text-white transition-opacity hover:opacity-90 ${
                side === 'buy' ? 'bg-green-buy' : 'bg-red-sell'
              }`}
            >
              {side === 'buy' ? 'Buy' : 'Sell'} {selectedSymbol}
            </button>
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
              className="w-full rounded bg-accent py-3 text-sm font-semibold text-white transition-opacity hover:opacity-90"
            >
              Enable Trading (7d)
            </button>
          )
        ) : (
          <button
            onClick={() => wallet.connect()}
            className="w-full rounded border border-accent bg-bg-tertiary py-3 text-sm font-semibold text-accent transition-opacity hover:opacity-90"
          >
            Connect {wallet.isRabby ? 'Rabby' : 'Wallet'} to Trade
          </button>
        )}

        {/* Account Equity Section */}
        <div className="mt-4 rounded border border-border bg-bg-primary p-3">
          {/* Account Equity Header */}
          <div className="mb-3 flex items-center justify-between">
            <span className="text-xs font-medium text-text-muted">Account Equity</span>
            {isLoadingAccount ? (
              <span className="text-sm text-text-muted">Loading...</span>
            ) : (
              <span className="font-mono text-lg font-semibold text-text-primary">
                ${account?.totalEquity.toLocaleString('en-US', { minimumFractionDigits: 2 }) ?? '--'}
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
                  <span className={`font-mono ${account.unrealizedPnL >= 0 ? 'text-green-buy' : 'text-red-sell'}`}>
                    {account.unrealizedPnL >= 0 ? '+' : ''}${account.unrealizedPnL.toLocaleString('en-US', { minimumFractionDigits: 2 })}
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
      </div>
    </div>
  )
}
