'use client'

interface OrderPreviewProps {
  requiredMargin: number
  estimatedFee: number
  availableBalance: number
}

export function OrderPreview({
  requiredMargin,
  estimatedFee,
  availableBalance
}: OrderPreviewProps) {
  return (
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
          <span className="font-mono text-text-primary">
            ${(requiredMargin + estimatedFee).toFixed(2)}
          </span>
        </div>
      </div>
    </div>
  )
}
