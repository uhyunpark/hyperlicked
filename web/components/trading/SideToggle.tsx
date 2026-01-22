'use client'

import type { Side } from '@/lib/types'

interface SideToggleProps {
  side: Side
  onSideChange: (side: Side) => void
}

export function SideToggle({ side, onSideChange }: SideToggleProps) {
  return (
    <div className="mb-4 grid grid-cols-2 gap-2">
      <button
        className={`rounded py-2 text-sm font-semibold transition-colors ${
          side === 'buy'
            ? 'bg-green-buy text-white'
            : 'border border-border bg-bg-tertiary text-text-secondary hover:bg-bg-tertiary/80'
        }`}
        onClick={() => onSideChange('buy')}
      >
        Buy / Long
      </button>
      <button
        className={`rounded py-2 text-sm font-semibold transition-colors ${
          side === 'sell'
            ? 'bg-red-sell text-white'
            : 'border border-border bg-bg-tertiary text-text-secondary hover:bg-bg-tertiary/80'
        }`}
        onClick={() => onSideChange('sell')}
      >
        Sell / Short
      </button>
    </div>
  )
}
