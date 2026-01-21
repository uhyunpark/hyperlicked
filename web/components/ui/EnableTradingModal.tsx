'use client'

import { useState, useEffect, useRef, useCallback } from 'react'

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
  const modalRef = useRef<HTMLDivElement>(null)
  const previousActiveElement = useRef<HTMLElement | null>(null)

  // Focus trap and keyboard handling
  const handleKeyDown = useCallback((e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      onClose()
      return
    }

    if (e.key !== 'Tab' || !modalRef.current) return

    const focusableElements = modalRef.current.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
    )
    const firstElement = focusableElements[0]
    const lastElement = focusableElements[focusableElements.length - 1]

    if (e.shiftKey && document.activeElement === firstElement) {
      e.preventDefault()
      lastElement?.focus()
    } else if (!e.shiftKey && document.activeElement === lastElement) {
      e.preventDefault()
      firstElement?.focus()
    }
  }, [onClose])

  useEffect(() => {
    if (!isOpen) return

    // Store the previously focused element
    previousActiveElement.current = document.activeElement as HTMLElement

    // Add event listener for keyboard navigation
    document.addEventListener('keydown', handleKeyDown)

    // Focus the first focusable element in the modal
    const timer = setTimeout(() => {
      const firstFocusable = modalRef.current?.querySelector<HTMLElement>(
        'button:not([disabled]), [href], input:not([disabled])'
      )
      firstFocusable?.focus()
    }, 0)

    return () => {
      document.removeEventListener('keydown', handleKeyDown)
      clearTimeout(timer)
      // Restore focus to the previously focused element
      previousActiveElement.current?.focus()
    }
  }, [isOpen, handleKeyDown])

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
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      role="dialog"
      aria-modal="true"
      aria-labelledby="modal-title"
    >
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-black/60 backdrop-blur-sm"
        onClick={onClose}
        aria-hidden="true"
      />

      {/* Modal */}
      <div
        ref={modalRef}
        className="relative bg-bg-secondary border border-border rounded-xl p-6 w-full max-w-md mx-4 shadow-2xl"
      >
        {/* Header */}
        <div className="flex items-center justify-between mb-4">
          <h2 id="modal-title" className="text-xl font-semibold text-text-primary">
            Enable Gasless Trading
          </h2>
          <button
            onClick={onClose}
            className="text-text-muted hover:text-text-primary transition-colors"
            aria-label="Close modal"
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
          <ul className="bg-bg-tertiary rounded-lg p-4 space-y-2" aria-label="Benefits of enabling trading">
            <li className="flex items-center gap-2 text-sm">
              <span className="text-green-500" aria-hidden="true">✓</span>
              <span className="text-text-secondary">No transaction fees</span>
            </li>
            <li className="flex items-center gap-2 text-sm">
              <span className="text-green-500" aria-hidden="true">✓</span>
              <span className="text-text-secondary">Instant order signing</span>
            </li>
            <li className="flex items-center gap-2 text-sm">
              <span className="text-green-500" aria-hidden="true">✓</span>
              <span className="text-text-secondary">Revoke anytime</span>
            </li>
          </ul>
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
