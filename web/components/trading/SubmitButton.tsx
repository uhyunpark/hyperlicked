'use client'

import type { Side } from '@/lib/types'
import { useWallet } from '@/lib/useWallet'
import { toast } from '@/components/ui/Toast'

interface SubmitButtonProps {
  side: Side
  symbol: string
  onSubmit: () => void
}

export function SubmitButton({ side, symbol, onSubmit }: SubmitButtonProps) {
  const wallet = useWallet()

  const handleEnableTrading = async () => {
    try {
      await wallet.enableTrading(7)
      toast.success('Trading Enabled', 'You can now trade without signing every order')
    } catch (error: any) {
      toast.error('Enable Trading Failed', error.message)
    }
  }

  if (!wallet.isConnected) {
    return (
      <button
        onClick={() => wallet.connect()}
        className="w-full rounded border border-accent bg-bg-tertiary py-3 text-sm font-semibold text-accent transition-opacity hover:opacity-90"
      >
        Connect {wallet.isRabby ? 'Rabby' : 'Wallet'} to Trade
      </button>
    )
  }

  if (!wallet.tradingEnabled) {
    return (
      <button
        onClick={handleEnableTrading}
        className="w-full rounded bg-accent py-3 text-sm font-semibold text-white transition-opacity hover:opacity-90"
      >
        Enable Trading (7d)
      </button>
    )
  }

  return (
    <button
      onClick={onSubmit}
      className={`w-full rounded py-3 text-sm font-semibold text-white shadow-panel transition-opacity hover:opacity-90 ${
        side === 'buy' ? 'bg-long' : 'bg-short'
      }`}
    >
      {side === 'buy' ? 'Buy' : 'Sell'} {symbol}
    </button>
  )
}
