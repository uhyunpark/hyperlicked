'use client'

import { useState } from 'react'
import { useTradingStore } from '@/lib/store'
import { useWallet, type OrderToSign } from '@/lib/useWallet'
import type { Side, OrderType } from '@/lib/types'

export function TradePanel() {
  const { selectedSymbol, currentPrice } = useTradingStore()
  const wallet = useWallet()
  const [side, setSide] = useState<Side>('buy')
  const [orderType, setOrderType] = useState<OrderType>('limit')
  const [price, setPrice] = useState('')
  const [size, setSize] = useState('')
  const [leverage, setLeverage] = useState(10)
  const [nonce, setNonce] = useState(1) // Track nonce for replay protection

  // Mock account data (TODO: Fetch from API using wallet.address)
  const accountBalance = 10000
  const availableBalance = 8500

  // Calculate order details
  const priceNum = parseFloat(price) || currentPrice
  const sizeNum = parseFloat(size) || 0
  const notional = priceNum * sizeNum
  const requiredMargin = notional / leverage
  const estimatedFee = notional * 0.0005 // 0.05% taker fee

  const handleSubmit = async () => {
    if (!size || parseFloat(size) <= 0) {
      alert('Please enter a valid size')
      return
    }

    if (orderType === 'limit' && (!price || parseFloat(price) <= 0)) {
      alert('Please enter a valid price')
      return
    }

    // Check wallet connection
    if (!wallet.isConnected || !wallet.address) {
      alert('Please connect your wallet first')
      return
    }

    try {
      // Import API functions
      const { submitSignedTransaction, convertToApiPrice, convertToApiSize } = await import('@/lib/api')

      const orderPrice = orderType === 'limit' ? parseFloat(price) : currentPrice
      const orderSize = parseFloat(size)

      // Convert to API units (BigInt strings)
      const priceInCents = convertToApiPrice(orderPrice).toString()
      const sizeInSats = convertToApiSize(orderSize).toString()

      // Create order to sign (EIP-712)
      const orderToSign: OrderToSign = {
        symbol: selectedSymbol,
        side: side === 'buy' ? 1 : 2,
        type: orderType === 'limit' ? 1 : (orderType === 'market' ? 2 : 3), // 1=GTC, 2=IOC, 3=ALO
        price: priceInCents,
        qty: sizeInSats,
        nonce: nonce.toString(),
        deadline: '0', // No expiry
        leverage,
        owner: wallet.address
      }

      console.log('[order] Signing order with Rabby/MetaMask...', orderToSign)

      // Sign order with wallet (Rabby/MetaMask)
      const signature = await wallet.signOrder(orderToSign)

      console.log('[order] Order signed! Signature:', signature)

      // Create signed transaction
      const signedTx = {
        type: 'order' as const,
        order: orderToSign,
        signature
      }

      console.log('[order] Submitting signed transaction...')

      // Submit signed transaction
      const response = await submitSignedTransaction(signedTx)

      console.log('[order] Response:', response)

      if (response.status === 'submitted') {
        alert(`Order submitted successfully!\n\nOrder ID: ${response.orderId}\nSigned with: ${wallet.isRabby ? 'Rabby Wallet' : 'MetaMask'}`)
        // Increment nonce for next order
        setNonce(nonce + 1)
        // Clear form
        setSize('')
        if (orderType === 'limit') {
          setPrice('')
        }
      } else {
        alert(`Order rejected: ${response.message || 'Unknown error'}`)
      }
    } catch (error) {
      console.error('[order] Error:', error)
      alert(`Failed to submit order: ${error instanceof Error ? error.message : 'Unknown error'}`)
    }
  }

  return (
    <div className="flex h-full flex-col bg-bg-secondary">
      {/* Header */}
      <div className="border-b border-border px-4 py-2">
        <h3 className="text-sm font-semibold text-text-primary">Trade</h3>
      </div>

      <div className="flex-1 overflow-y-auto p-4">
        {/* Order Type Tabs */}
        <div className="mb-4 flex gap-1 rounded border border-border bg-bg-primary p-1">
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
        <div className="mb-4">
          <label className="mb-1 block text-xs text-text-muted">Size (BTC)</label>
          <input
            type="number"
            value={size}
            onChange={(e) => setSize(e.target.value)}
            placeholder="0.00"
            className="w-full rounded border border-border bg-bg-primary px-3 py-2 text-sm font-mono text-text-primary focus:border-accent focus:outline-none"
          />
          <div className="mt-1 flex justify-between text-xs text-text-muted">
            <span>Notional: ${notional.toFixed(2)}</span>
            <span>Max: {(availableBalance * leverage / currentPrice).toFixed(4)}</span>
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
            onClick={() => wallet.connect()}
            className="w-full rounded border border-accent bg-bg-tertiary py-3 text-sm font-semibold text-accent transition-opacity hover:opacity-90"
          >
            Connect {wallet.isRabby ? 'Rabby' : 'Wallet'} to Trade
          </button>
        )}

        {/* Account Summary */}
        <div className="mt-4 rounded border border-border bg-bg-primary p-3">
          <div className="mb-1 text-xs text-text-muted">Account Balance</div>
          <div className="text-lg font-mono font-semibold text-text-primary">${accountBalance.toFixed(2)}</div>
        </div>
      </div>
    </div>
  )
}
