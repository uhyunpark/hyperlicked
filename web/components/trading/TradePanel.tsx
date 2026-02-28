'use client'

import { useState, useCallback } from 'react'
import { useTradingStore } from '@/lib/store'
import { useWallet } from '@/lib/useWallet'
import { useAccountData } from '@/lib/useAccountData'
import { useOrderSubmit } from '@/lib/useOrderSubmit'
import { isDevelopment } from '@/lib/config'
import type { Side, OrderType, TimeInForce } from '@/lib/types'
import { OrderTypeSelector } from './OrderTypeSelector'
import { SideToggle } from './SideToggle'
import { OrderInputs } from './OrderInputs'
import { TpSlSection } from './TpSlSection'
import { OrderPreview } from './OrderPreview'
import { SubmitButton } from './SubmitButton'

export function TradePanel() {
  const selectedSymbol = useTradingStore((s) => s.selectedSymbol)
  const currentPrice = useTradingStore((s) => s.currentPrice)
  const wallet = useWallet()
  const { submitOrder } = useOrderSubmit()
  const {
    availableBalance,
    isFaucetLoading,
    handleFaucet,
  } = useAccountData(wallet.isConnected ? wallet.address : null)

  // Form state
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

  // Calculate order details
  const priceNum = parseFloat(price) || currentPrice
  const sizeNum = parseFloat(size) || 0
  const notional = priceNum * sizeNum
  const requiredMargin = notional / leverage
  const estimatedFee = notional * 0.0005

  // Calculate size for percentage buttons
  const handleSizePercent = useCallback((percent: number) => {
    if (currentPrice <= 0 || availableBalance <= 0) return
    const calculatedSize = (availableBalance * leverage * (percent / 100)) / currentPrice
    setSize(calculatedSize.toFixed(4))
  }, [currentPrice, availableBalance, leverage])

  // Submit order handler
  const handleSubmit = useCallback(() => {
    submitOrder({
      side,
      orderType,
      tif,
      price,
      size,
      leverage,
      reduceOnly,
      tpSlEnabled,
      tpPrice,
      slPrice,
    }, {
      onSuccess: () => {
        setSize('')
        if (orderType === 'limit') setPrice('')
        setTpPrice('')
        setSlPrice('')
      }
    })
  }, [submitOrder, side, orderType, tif, price, size, leverage, reduceOnly, tpSlEnabled, tpPrice, slPrice])

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Header */}
      <div className="border-b border-border px-4 py-2">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold text-text-primary">Trade</h3>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto p-4">
        <OrderTypeSelector
          orderType={orderType}
          tif={tif}
          onOrderTypeChange={setOrderType}
          onTifChange={setTif}
        />

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
            className="mb-3 w-full rounded border border-warning/30 bg-warning/10 py-2 text-sm font-medium text-warning transition-colors hover:bg-warning/20 disabled:opacity-50"
          >
            {isFaucetLoading ? 'Requesting...' : 'Get Test USDC ($100k)'}
          </button>
        )}

        <SideToggle side={side} onSideChange={setSide} />

        <OrderInputs
          orderType={orderType}
          price={price}
          size={size}
          leverage={leverage}
          currentPrice={currentPrice}
          availableBalance={availableBalance}
          onPriceChange={setPrice}
          onSizeChange={setSize}
          onLeverageChange={setLeverage}
          onSizePercent={handleSizePercent}
        />

        {/* TP/SL Section (hide for reduce-only orders) */}
        {!reduceOnly && (
          <TpSlSection
            enabled={tpSlEnabled}
            tpPrice={tpPrice}
            slPrice={slPrice}
            side={side}
            currentPrice={currentPrice}
            onToggle={() => setTpSlEnabled(!tpSlEnabled)}
            onTpPriceChange={setTpPrice}
            onSlPriceChange={setSlPrice}
          />
        )}

        <OrderPreview
          requiredMargin={requiredMargin}
          estimatedFee={estimatedFee}
          availableBalance={availableBalance}
        />
      </div>

      {/* Pinned submit button */}
      <div className="border-t border-border/50 p-4">
        <SubmitButton
          side={side}
          symbol={selectedSymbol}
          onSubmit={handleSubmit}
        />
      </div>
    </div>
  )
}
