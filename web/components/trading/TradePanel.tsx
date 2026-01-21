'use client'

import { useState, useEffect, useCallback } from 'react'
import { useTradingStore } from '@/lib/store'
import { useWallet, type OrderToSign } from '@/lib/useWallet'
import { toast } from '@/components/ui/Toast'
import type { Side, OrderType, TimeInForce } from '@/lib/types'
import { TIF_CODES } from '@/lib/types'
import { isDevelopment } from '@/lib/config'
import { requestFaucet, placeTriggerOrder, convertToApiPrice, convertToApiSize } from '@/lib/api'

interface AccountData {
  balance: number
  lockedCollateral: number
  availableBalance: number
  unrealizedPnL: number
  totalEquity: number
}

export function TradePanel() {
  const { selectedSymbol, currentPrice, balanceRefreshTrigger } = useTradingStore()
  const wallet = useWallet()
  const [side, setSide] = useState<Side>('buy')
  const [orderType, setOrderType] = useState<OrderType>('limit')
  const [tif, setTif] = useState<TimeInForce>('gtc')
  const [reduceOnly, setReduceOnly] = useState(false)
  const [price, setPrice] = useState('')
  const [size, setSize] = useState('')
  const [leverage, setLeverage] = useState(10)

  // TP/SL state
  const [tpSlEnabled, setTpSlEnabled] = useState(false)
  const [tpPrice, setTpPrice] = useState('')
  const [slPrice, setSlPrice] = useState('')

  // Account data from API
  const [account, setAccount] = useState<AccountData | null>(null)
  const [isLoadingAccount, setIsLoadingAccount] = useState(false)
  const [isFaucetLoading, setIsFaucetLoading] = useState(false)

  // Account fetch function (reusable)
  const fetchAccount = useCallback(async () => {
    if (!wallet.address) return
    setIsLoadingAccount(true)
    try {
      const { getAccount } = await import('@/lib/api')
      const data = await getAccount(wallet.address)
      setAccount({
        balance: data.balance / 100, // cents to dollars
        lockedCollateral: data.lockedCollateral / 100,
        availableBalance: data.availableBalance / 100,
        unrealizedPnL: data.unrealizedPnL / 100,
        totalEquity: data.totalEquity / 100
      })
    } catch (error) {
      console.error('[account] Failed to fetch:', error)
    } finally {
      setIsLoadingAccount(false)
    }
  }, [wallet.address])

  // Fetch account data when wallet connects
  useEffect(() => {
    if (!wallet.isConnected || !wallet.address) {
      setAccount(null)
      return
    }

    fetchAccount()
    // Refresh every 10 seconds
    const interval = setInterval(fetchAccount, 10000)
    return () => clearInterval(interval)
  }, [wallet.isConnected, wallet.address, fetchAccount])

  // Refetch account when WebSocket balance update is received
  useEffect(() => {
    if (balanceRefreshTrigger > 0 && wallet.address) {
      console.log('[account] Balance refresh triggered via WebSocket')
      fetchAccount()
    }
  }, [balanceRefreshTrigger, wallet.address, fetchAccount])

  const accountBalance = account?.totalEquity ?? 0
  const availableBalance = account?.availableBalance ?? 0

  // Faucet handler (dev mode only)
  const handleFaucet = async () => {
    if (!wallet.address || isFaucetLoading) return
    setIsFaucetLoading(true)
    try {
      const success = await requestFaucet(wallet.address)
      if (success) {
        toast.success('Faucet', 'Received $100,000 test USDT')
        // Wait for block to be processed (deposit is in mempool, executed on next block ~100ms)
        // Then refresh account data
        setTimeout(async () => {
          try {
            const { getAccount } = await import('@/lib/api')
            const data = await getAccount(wallet.address!)
            setAccount({
              balance: data.balance / 100,
              lockedCollateral: data.lockedCollateral / 100,
              availableBalance: data.availableBalance / 100,
              unrealizedPnL: data.unrealizedPnL / 100,
              totalEquity: data.totalEquity / 100
            })
          } catch (err) {
            console.error('[faucet] Failed to refresh balance:', err)
          }
        }, 500) // Wait 500ms for block to process
      } else {
        toast.error('Faucet Failed', 'Could not get test USDT')
      }
    } catch (error: any) {
      toast.error('Faucet Failed', error.message)
    } finally {
      setIsFaucetLoading(false)
    }
  }

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
      const { submitSignedTransaction, convertToApiPrice, convertToApiSize, getNonce } = await import('@/lib/api')

      // Always fetch fresh nonce from server before submitting
      // This ensures we have the correct nonce even after previous orders
      const nonceData = await getNonce(wallet.address!)
      const currentNonce = nonceData.nonce

      // For market orders, use sweep prices to ensure execution
      // Buy: Use very high price to sweep all asks
      // Sell: Use minimum price (1 cent) to sweep all bids
      let orderPrice: number
      if (orderType === 'market') {
        orderPrice = side === 'buy' ? currentPrice * 2 : 0.01
      } else {
        orderPrice = parseFloat(price)
      }
      const orderSize = parseFloat(size)

      // For market orders, always use IOC (immediate-or-cancel)
      // For limit orders, use the selected TIF
      const tifCode = orderType === 'market' ? TIF_CODES.ioc : TIF_CODES[tif]

      const orderToSign: OrderToSign = {
        symbol: selectedSymbol,
        side: side === 'buy' ? 1 : 2,
        type: tifCode,
        price: convertToApiPrice(orderPrice).toString(),
        qty: convertToApiSize(orderSize).toString(),
        nonce: currentNonce.toString(),
        deadline: '0',
        leverage,
        owner: wallet.address,
        reduce_only: reduceOnly
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

        // Place TP/SL orders if enabled and this is not a reduce-only order
        if (tpSlEnabled && !reduceOnly) {
          const orderSizeApi = convertToApiSize(parseFloat(size))

          // Place Take Profit if set
          if (tpPrice && parseFloat(tpPrice) > 0) {
            try {
              await placeTriggerOrder({
                trader: wallet.address!,
                symbol: selectedSymbol,
                triggerType: 'tp',
                triggerPrice: convertToApiPrice(parseFloat(tpPrice)),
                size: orderSizeApi,
              })
              toast.success('Take Profit Set', `TP at $${parseFloat(tpPrice).toLocaleString()}`)
            } catch (err: any) {
              toast.warning('TP Failed', err.message)
            }
          }

          // Place Stop Loss if set
          if (slPrice && parseFloat(slPrice) > 0) {
            try {
              await placeTriggerOrder({
                trader: wallet.address!,
                symbol: selectedSymbol,
                triggerType: 'sl',
                triggerPrice: convertToApiPrice(parseFloat(slPrice)),
                size: orderSizeApi,
              })
              toast.success('Stop Loss Set', `SL at $${parseFloat(slPrice).toLocaleString()}`)
            } catch (err: any) {
              toast.warning('SL Failed', err.message)
            }
          }
        }

        setSize('')
        if (orderType === 'limit') setPrice('')
        setTpPrice('')
        setSlPrice('')

        // Immediately refresh open orders so the new order appears
        try {
          const { getOrders, convertPrice, convertSize } = await import('@/lib/api')
          const ordersData = await getOrders(wallet.address!)
          const orders = ordersData
            .filter(o => o.status === 'open' || o.status === 'partial')
            .map(o => {
              const size = convertSize(o.size)
              const filled = convertSize(o.filled)
              return {
                id: o.id,
                symbol: o.symbol,
                side: o.side as 'buy' | 'sell',
                type: (o.type === 'market' ? 'market' : 'limit') as 'limit' | 'market',
                price: convertPrice(o.price),
                size,
                filled,
                remaining: size - filled,
                status: (o.status === 'partial' ? 'open' : o.status) as 'open' | 'filled' | 'cancelled',
                timestamp: o.timestamp
              }
            })
          useTradingStore.getState().setOpenOrders(orders)
        } catch (err) {
          console.error('[order] Failed to refresh orders:', err)
        }
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
        </div>

        {/* TIF Selector (Limit orders only) */}
        {orderType === 'limit' && (
          <div className="mb-3 flex gap-1 rounded border border-border bg-bg-primary p-1">
            <button
              className={`flex-1 rounded px-2 py-1 text-xs transition-colors ${
                tif === 'gtc'
                  ? 'bg-accent/20 text-accent'
                  : 'text-text-muted hover:text-text-secondary'
              }`}
              onClick={() => setTif('gtc')}
              title="Good til Cancel - stays on book until filled or cancelled"
            >
              GTC
            </button>
            <button
              className={`flex-1 rounded px-2 py-1 text-xs transition-colors ${
                tif === 'ioc'
                  ? 'bg-accent/20 text-accent'
                  : 'text-text-muted hover:text-text-secondary'
              }`}
              onClick={() => setTif('ioc')}
              title="Immediate or Cancel - fill immediately, cancel unfilled portion"
            >
              IOC
            </button>
            <button
              className={`flex-1 rounded px-2 py-1 text-xs transition-colors ${
                tif === 'alo'
                  ? 'bg-accent/20 text-accent'
                  : 'text-text-muted hover:text-text-secondary'
              }`}
              onClick={() => setTif('alo')}
              title="Post Only - rejected if would match immediately (maker only)"
            >
              Post Only
            </button>
          </div>
        )}

        {/* Reduce Only Checkbox */}
        <label className="mb-3 flex cursor-pointer items-center gap-2">
          <input
            type="checkbox"
            checked={reduceOnly}
            onChange={(e) => setReduceOnly(e.target.checked)}
            className="h-4 w-4 rounded border-border accent-accent focus:ring-2 focus:ring-accent focus:ring-offset-2 focus:ring-offset-bg-secondary"
            aria-describedby="reduce-only-description"
          />
          <span id="reduce-only-description" className="text-xs text-text-secondary" title="Only reduce existing position, never increase">
            Reduce Only
          </span>
        </label>

        {/* Available to Trade */}
        <div className="mb-3 flex items-center justify-between rounded bg-bg-tertiary px-3 py-2">
          <span className="text-xs text-text-muted">Available to Trade</span>
          <span className="font-mono text-sm font-semibold text-text-primary">
            ${availableBalance.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })} USDC
          </span>
        </div>

        {/* Dev Faucet Button */}
        {isDevelopment && wallet.isConnected && (
          <button
            onClick={handleFaucet}
            disabled={isFaucetLoading}
            className="mb-3 w-full rounded border border-yellow-500/30 bg-yellow-500/10 py-2 text-sm font-medium text-yellow-500 transition-colors hover:bg-yellow-500/20 disabled:opacity-50"
          >
            {isFaucetLoading ? 'Requesting...' : 'Get Test USDT ($100k)'}
          </button>
        )}

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
            <label htmlFor="price-input" className="mb-1 block text-xs text-text-muted">Price (USDT)</label>
            <input
              id="price-input"
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
          <label htmlFor="size-input" className="mb-1 block text-xs text-text-muted">Size (BTC)</label>
          <input
            id="size-input"
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
            <label htmlFor="leverage-slider" className="text-xs text-text-muted">Leverage</label>
            <div className="text-sm font-mono font-semibold text-text-primary" aria-hidden="true">{leverage}x</div>
          </div>
          <input
            id="leverage-slider"
            type="range"
            min="1"
            max="50"
            value={leverage}
            onChange={(e) => setLeverage(parseInt(e.target.value))}
            className="w-full accent-accent"
            aria-valuemin={1}
            aria-valuemax={50}
            aria-valuenow={leverage}
            aria-valuetext={`${leverage}x leverage`}
          />
          <div className="mt-1 flex justify-between text-xs text-text-muted">
            <span>1x</span>
            <span>25x</span>
            <span>50x</span>
          </div>
        </div>

        {/* TP/SL Section */}
        {!reduceOnly && (
          <div className="mb-4 rounded border border-border bg-bg-primary">
            <button
              onClick={() => setTpSlEnabled(!tpSlEnabled)}
              className="flex w-full items-center justify-between p-3 text-left"
            >
              <span className="text-xs font-medium text-text-secondary">
                TP / SL
              </span>
              <span className={`text-xs ${tpSlEnabled ? 'text-accent' : 'text-text-muted'}`}>
                {tpSlEnabled ? '▲ Enabled' : '▼ Disabled'}
              </span>
            </button>

            {tpSlEnabled && (
              <div className="space-y-3 border-t border-border p-3">
                {/* Take Profit */}
                <div>
                  <label htmlFor="tp-input" className="mb-1 flex items-center justify-between text-xs">
                    <span className="text-green-buy">Take Profit</span>
                    <span className="text-text-muted">
                      {side === 'buy' ? '> Mark' : '< Mark'}
                    </span>
                  </label>
                  <input
                    id="tp-input"
                    type="number"
                    value={tpPrice}
                    onChange={(e) => setTpPrice(e.target.value)}
                    placeholder={side === 'buy'
                      ? (currentPrice * 1.05).toFixed(2)
                      : (currentPrice * 0.95).toFixed(2)
                    }
                    className="w-full rounded border border-border bg-bg-secondary px-3 py-2 text-sm font-mono text-text-primary focus:border-green-buy focus:outline-none"
                  />
                </div>

                {/* Stop Loss */}
                <div>
                  <label htmlFor="sl-input" className="mb-1 flex items-center justify-between text-xs">
                    <span className="text-red-sell">Stop Loss</span>
                    <span className="text-text-muted">
                      {side === 'buy' ? '< Mark' : '> Mark'}
                    </span>
                  </label>
                  <input
                    id="sl-input"
                    type="number"
                    value={slPrice}
                    onChange={(e) => setSlPrice(e.target.value)}
                    placeholder={side === 'buy'
                      ? (currentPrice * 0.95).toFixed(2)
                      : (currentPrice * 1.05).toFixed(2)
                    }
                    className="w-full rounded border border-border bg-bg-secondary px-3 py-2 text-sm font-mono text-text-primary focus:border-red-sell focus:outline-none"
                  />
                </div>

                <p className="text-xs text-text-muted">
                  TP/SL will be placed after your order fills
                </p>
              </div>
            )}
          </div>
        )}

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
