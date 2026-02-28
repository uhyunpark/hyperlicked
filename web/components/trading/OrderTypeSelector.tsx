'use client'

import type { OrderType, TimeInForce } from '@/lib/types'

interface OrderTypeSelectorProps {
  orderType: OrderType
  tif: TimeInForce
  onOrderTypeChange: (type: OrderType) => void
  onTifChange: (tif: TimeInForce) => void
}

export function OrderTypeSelector({
  orderType,
  tif,
  onOrderTypeChange,
  onTifChange
}: OrderTypeSelectorProps) {
  return (
    <>
      {/* Order Type Tabs */}
      <div className="mb-3 flex gap-1 rounded border border-border/50 bg-bg-primary/80 p-1">
        <button
          className={`flex-1 rounded px-3 py-1.5 text-xs font-medium transition-colors ${
            orderType === 'limit'
              ? 'bg-bg-tertiary text-text-primary'
              : 'text-text-muted hover:text-text-secondary'
          }`}
          onClick={() => onOrderTypeChange('limit')}
        >
          Limit
        </button>
        <button
          className={`flex-1 rounded px-3 py-1.5 text-xs font-medium transition-colors ${
            orderType === 'market'
              ? 'bg-bg-tertiary text-text-primary'
              : 'text-text-muted hover:text-text-secondary'
          }`}
          onClick={() => onOrderTypeChange('market')}
        >
          Market
        </button>
      </div>

      {/* TIF Selector (Limit orders only) */}
      {orderType === 'limit' && (
        <div className="mb-3 flex gap-1 rounded border border-border/50 bg-bg-primary/80 p-1">
          <button
            className={`flex-1 rounded px-2 py-1 text-xs transition-colors ${
              tif === 'gtc'
                ? 'bg-accent/20 text-accent'
                : 'text-text-muted hover:text-text-secondary'
            }`}
            onClick={() => onTifChange('gtc')}
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
            onClick={() => onTifChange('ioc')}
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
            onClick={() => onTifChange('alo')}
            title="Post Only - rejected if would match immediately (maker only)"
          >
            Post Only
          </button>
        </div>
      )}
    </>
  )
}
