'use client'

import type { Side } from '@/lib/types'
import { useWallet } from '@/lib/useWallet'

interface SubmitButtonProps {
  side: Side
  symbol: string
  onSubmit: () => void
}

export function SubmitButton({ side, symbol, onSubmit }: SubmitButtonProps) {
  const wallet = useWallet()

  if (!wallet.isConnected) {
    return (
      <button
        type="button"
        onClick={() => wallet.connect()}
        className="w-full rounded border border-accent bg-bg-tertiary py-3 text-sm font-semibold text-accent transition-opacity hover:opacity-90"
      >
        Connect {wallet.isRabby ? 'Rabby' : 'Wallet'} to Trade
      </button>
    )
  }

  return (
    <button
      type="button"
      onClick={onSubmit}
      className={`w-full rounded py-3 text-sm font-semibold text-white shadow-panel transition-opacity hover:opacity-90 ${
        side === 'buy' ? 'bg-long' : 'bg-short'
      }`}
    >
      {side === 'buy' ? 'Buy' : 'Sell'} {symbol}
    </button>
  )
}
