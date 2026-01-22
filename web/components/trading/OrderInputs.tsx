'use client'

import type { OrderType } from '@/lib/types'

interface OrderInputsProps {
  orderType: OrderType
  price: string
  size: string
  leverage: number
  currentPrice: number
  availableBalance: number
  onPriceChange: (value: string) => void
  onSizeChange: (value: string) => void
  onLeverageChange: (value: number) => void
  onSizePercent: (percent: number) => void
}

export function OrderInputs({
  orderType,
  price,
  size,
  leverage,
  currentPrice,
  availableBalance,
  onPriceChange,
  onSizeChange,
  onLeverageChange,
  onSizePercent
}: OrderInputsProps) {
  const priceNum = parseFloat(price) || currentPrice
  const sizeNum = parseFloat(size) || 0
  const notional = priceNum * sizeNum

  return (
    <>
      {/* Price Input (Limit only) */}
      {orderType === 'limit' && (
        <div className="mb-4">
          <label htmlFor="price-input" className="mb-1 block text-xs text-text-muted">
            Price (USDT)
          </label>
          <input
            id="price-input"
            type="number"
            value={price}
            onChange={(e) => onPriceChange(e.target.value)}
            placeholder={currentPrice.toFixed(2)}
            className="w-full rounded border border-border bg-bg-primary px-3 py-2 text-sm font-mono text-text-primary focus:border-accent focus:outline-none"
          />
        </div>
      )}

      {/* Size Input */}
      <div className="mb-3">
        <label htmlFor="size-input" className="mb-1 block text-xs text-text-muted">
          Size (BTC)
        </label>
        <input
          id="size-input"
          type="number"
          value={size}
          onChange={(e) => onSizeChange(e.target.value)}
          placeholder="0.00"
          className="w-full rounded border border-border bg-bg-primary px-3 py-2 text-sm font-mono text-text-primary focus:border-accent focus:outline-none"
        />
        {/* Size Percentage Buttons */}
        <div className="mt-2 flex gap-1">
          {[25, 50, 75, 100].map((percent) => (
            <button
              key={percent}
              onClick={() => onSizePercent(percent)}
              className="flex-1 rounded border border-border bg-bg-tertiary py-1 text-xs text-text-muted transition-colors hover:border-accent hover:text-accent"
            >
              {percent}%
            </button>
          ))}
        </div>
        <div className="mt-1 flex justify-between text-xs text-text-muted">
          <span>
            Notional: ${notional.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
          </span>
          <span>
            Max: {currentPrice > 0 ? (availableBalance * leverage / currentPrice).toFixed(4) : '0.0000'}
          </span>
        </div>
      </div>

      {/* Leverage Slider */}
      <div className="mb-4">
        <div className="mb-2 flex items-center justify-between">
          <label htmlFor="leverage-slider" className="text-xs text-text-muted">
            Leverage
          </label>
          <div className="text-sm font-mono font-semibold text-text-primary" aria-hidden="true">
            {leverage}x
          </div>
        </div>
        <input
          id="leverage-slider"
          type="range"
          min="1"
          max="50"
          value={leverage}
          onChange={(e) => onLeverageChange(parseInt(e.target.value))}
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
    </>
  )
}
