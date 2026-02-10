'use client'

import { useEffect, useCallback, useRef, useState } from 'react'
import { Header } from '@/components/trading/Header'
import { Orderbook } from '@/components/trading/Orderbook'
import { Chart } from '@/components/trading/Chart'
import { TradePanel } from '@/components/trading/TradePanel'
import { BottomTabs } from '@/components/trading/BottomTabs'
import { ErrorBoundary, SectionErrorFallback } from '@/components/ui/ErrorBoundary'
import { useWebSocket } from '@/lib/useWebSocket'
import { useTradingStore } from '@/lib/store'

const MIN_BOTTOM_HEIGHT = 120
const MAX_BOTTOM_HEIGHT = 500
const DEFAULT_BOTTOM_HEIGHT = 256

export default function TradingPage() {
  const isConnected = useTradingStore((s) => s.isConnected)
  const [bottomHeight, setBottomHeight] = useState(DEFAULT_BOTTOM_HEIGHT)
  const [isResizing, setIsResizing] = useState(false)
  const containerRef = useRef<HTMLDivElement>(null)

  // Handle resize drag
  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault()
    setIsResizing(true)
  }, [])

  useEffect(() => {
    if (!isResizing) return

    const handleMouseMove = (e: MouseEvent) => {
      if (!containerRef.current) return
      const containerRect = containerRef.current.getBoundingClientRect()
      const newHeight = containerRect.bottom - e.clientY
      setBottomHeight(Math.min(MAX_BOTTOM_HEIGHT, Math.max(MIN_BOTTOM_HEIGHT, newHeight)))
    }

    const handleMouseUp = () => {
      setIsResizing(false)
    }

    document.addEventListener('mousemove', handleMouseMove)
    document.addEventListener('mouseup', handleMouseUp)

    return () => {
      document.removeEventListener('mousemove', handleMouseMove)
      document.removeEventListener('mouseup', handleMouseUp)
    }
  }, [isResizing])

  // Connect to WebSocket for real-time updates (also handles user data)
  useWebSocket()

  return (
    <div className="flex h-screen flex-col bg-bg-primary">
      {/* Connection status indicator */}
      {!isConnected && (
        <div className="bg-red-sell/20 px-4 py-1 text-center text-xs text-red-sell">
          Connecting to blockchain...
        </div>
      )}

      {/* Header */}
      <ErrorBoundary fallback={<SectionErrorFallback title="Header" />}>
        <Header />
      </ErrorBoundary>

      {/* Main trading area */}
      <div className="flex flex-1 overflow-hidden">
        {/* Left: Chart + Bottom Tabs (dominant area) */}
        <div ref={containerRef} className="flex flex-1 flex-col overflow-hidden">
          <div className="min-h-0 flex-1 border-b border-border">
            <ErrorBoundary fallback={<SectionErrorFallback title="Chart" />}>
              <Chart />
            </ErrorBoundary>
          </div>

          {/* Resizable Bottom tabs */}
          <div className="relative flex flex-col" style={{ height: bottomHeight }}>
            {/* Resize handle */}
            <div
              onMouseDown={handleMouseDown}
              className={`absolute -top-1 left-0 right-0 z-10 flex h-2 cursor-ns-resize items-center justify-center ${
                isResizing ? 'bg-accent/20' : 'hover:bg-accent/10'
              }`}
            >
              <div className="h-0.5 w-12 rounded-full bg-text-muted/50" />
            </div>
            <ErrorBoundary fallback={<SectionErrorFallback title="Orders & Positions" />}>
              <BottomTabs />
            </ErrorBoundary>
          </div>
        </div>

        {/* Center-Right: Orderbook */}
        <div className="w-72 border-l border-border">
          <ErrorBoundary fallback={<SectionErrorFallback title="Orderbook" />}>
            <Orderbook />
          </ErrorBoundary>
        </div>

        {/* Far Right: Trade Panel */}
        <div className="w-80 border-l border-border">
          <ErrorBoundary fallback={<SectionErrorFallback title="Trade Panel" />}>
            <TradePanel />
          </ErrorBoundary>
        </div>
      </div>
    </div>
  )
}
