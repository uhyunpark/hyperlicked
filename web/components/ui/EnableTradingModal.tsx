'use client'

import { useState } from 'react'

interface EnableTradingModalProps {
  isOpen: boolean
  onClose: () => void
  onEnableTrading: () => Promise<void>
  onUseMetaMask: () => Promise<void>
}

export function EnableTradingModal({
  isOpen,
  onClose,
  onEnableTrading,
  onUseMetaMask
}: EnableTradingModalProps) {
  const [isEnabling, setIsEnabling] = useState(false)
  const [isSubmitting, setIsSubmitting] = useState(false)

  if (!isOpen) return null

  const handleEnableTrading = async () => {
    setIsEnabling(true)
    try {
      await onEnableTrading()
      onClose()
    } catch (error) {
      console.error('[modal] Enable trading failed:', error)
    } finally {
      setIsEnabling(false)
    }
  }

  const handleUseMetaMask = async () => {
    setIsSubmitting(true)
    try {
      await onUseMetaMask()
      onClose()
    } catch (error) {
      console.error('[modal] MetaMask signing failed:', error)
    } finally {
      setIsSubmitting(false)
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-black/60 backdrop-blur-sm"
        onClick={onClose}
      />

      {/* Modal */}
      <div className="relative bg-bg-secondary border border-border rounded-xl p-6 w-full max-w-md mx-4 shadow-2xl">
        {/* Header */}
        <div className="flex items-center justify-between mb-4">
          <h2 className="text-xl font-semibold text-text-primary">
            Enable Gasless Trading
          </h2>
          <button
            onClick={onClose}
            className="text-text-muted hover:text-text-primary transition-colors"
          >
            ✕
          </button>
        </div>

        {/* Content */}
        <div className="mb-6">
          <p className="text-text-secondary mb-4">
            Sign once to enable gasless trading for the next 7 days.
            No more wallet popups for every trade!
          </p>
          <div className="bg-bg-tertiary rounded-lg p-4 space-y-2">
            <div className="flex items-center gap-2 text-sm">
              <span className="text-green-500">✓</span>
              <span className="text-text-secondary">No transaction fees</span>
            </div>
            <div className="flex items-center gap-2 text-sm">
              <span className="text-green-500">✓</span>
              <span className="text-text-secondary">Instant order signing</span>
            </div>
            <div className="flex items-center gap-2 text-sm">
              <span className="text-green-500">✓</span>
              <span className="text-text-secondary">Revoke anytime</span>
            </div>
          </div>
        </div>

        {/* Actions */}
        <div className="flex gap-3">
          <button
            onClick={handleEnableTrading}
            disabled={isEnabling || isSubmitting}
            className="flex-1 bg-accent hover:bg-accent/90 text-white font-semibold py-3 px-4 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isEnabling ? (
              <span className="flex items-center justify-center gap-2">
                <span className="animate-spin">⟳</span>
                Enabling...
              </span>
            ) : (
              'Enable Trading (7d)'
            )}
          </button>
          <button
            onClick={handleUseMetaMask}
            disabled={isEnabling || isSubmitting}
            className="flex-1 bg-bg-tertiary hover:bg-border text-text-primary font-semibold py-3 px-4 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isSubmitting ? (
              <span className="flex items-center justify-center gap-2">
                <span className="animate-spin">⟳</span>
                Signing...
              </span>
            ) : (
              'Use MetaMask'
            )}
          </button>
        </div>

        {/* Footer */}
        <p className="mt-4 text-xs text-text-muted text-center">
          Agent keys are stored locally and never leave your browser.
        </p>
      </div>
    </div>
  )
}
