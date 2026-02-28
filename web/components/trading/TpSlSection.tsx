'use client'

import type { Side } from '@/lib/types'

interface TpSlSectionProps {
  enabled: boolean
  tpPrice: string
  slPrice: string
  side: Side
  currentPrice: number
  onToggle: () => void
  onTpPriceChange: (value: string) => void
  onSlPriceChange: (value: string) => void
}

export function TpSlSection({
  enabled,
  tpPrice,
  slPrice,
  side,
  currentPrice,
  onToggle,
  onTpPriceChange,
  onSlPriceChange
}: TpSlSectionProps) {
  return (
    <div className="mb-4 rounded border border-border/50 bg-bg-primary/80">
      <button
        onClick={onToggle}
        className="flex w-full items-center justify-between p-3 text-left"
      >
        <span className="text-xs font-medium text-text-secondary">
          TP / SL
        </span>
        <span className={`text-xs ${enabled ? 'text-accent' : 'text-text-muted'}`}>
          {enabled ? '▲ Enabled' : '▼ Disabled'}
        </span>
      </button>

      {enabled && (
        <div className="space-y-3 border-t border-border p-3">
          {/* Take Profit */}
          <div>
            <label htmlFor="tp-input" className="mb-1 flex items-center justify-between text-xs">
              <span className="text-long">Take Profit</span>
              <span className="text-text-muted">
                {side === 'buy' ? '> Mark' : '< Mark'}
              </span>
            </label>
            <input
              id="tp-input"
              type="number"
              value={tpPrice}
              onChange={(e) => onTpPriceChange(e.target.value)}
              placeholder={side === 'buy'
                ? (currentPrice * 1.05).toFixed(2)
                : (currentPrice * 0.95).toFixed(2)
              }
              className="w-full rounded border border-border/50 bg-bg-secondary px-3 py-2 text-sm font-mono text-text-primary focus:border-long focus:outline-none"
            />
          </div>

          {/* Stop Loss */}
          <div>
            <label htmlFor="sl-input" className="mb-1 flex items-center justify-between text-xs">
              <span className="text-short">Stop Loss</span>
              <span className="text-text-muted">
                {side === 'buy' ? '< Mark' : '> Mark'}
              </span>
            </label>
            <input
              id="sl-input"
              type="number"
              value={slPrice}
              onChange={(e) => onSlPriceChange(e.target.value)}
              placeholder={side === 'buy'
                ? (currentPrice * 0.95).toFixed(2)
                : (currentPrice * 1.05).toFixed(2)
              }
              className="w-full rounded border border-border/50 bg-bg-secondary px-3 py-2 text-sm font-mono text-text-primary focus:border-short focus:outline-none"
            />
          </div>

          <p className="text-xs text-text-muted">
            TP/SL will be placed after your order fills
          </p>
        </div>
      )}
    </div>
  )
}
