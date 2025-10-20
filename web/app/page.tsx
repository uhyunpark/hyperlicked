'use client'

import { useEffect, useState } from 'react'
import { Header } from '@/components/trading/Header'
import { Orderbook } from '@/components/trading/Orderbook'
import { Chart } from '@/components/trading/Chart'
import { TradePanel } from '@/components/trading/TradePanel'
import { BottomTabs } from '@/components/trading/BottomTabs'
import { useWebSocket } from '@/lib/useWebSocket'

export default function TradingPage() {
  const [isConnected, setIsConnected] = useState(false)

  // Connect to WebSocket for real-time updates
  const ws = useWebSocket()

  useEffect(() => {
    if (ws) {
      setIsConnected(ws.readyState === WebSocket.OPEN)
    }
  }, [ws])

  return (
    <div className="flex h-screen flex-col bg-bg-primary">
      {/* Connection status indicator */}
      {!isConnected && (
        <div className="bg-red-sell/20 px-4 py-1 text-center text-xs text-red-sell">
          Connecting to blockchain...
        </div>
      )}

      {/* Header */}
      <Header />

      {/* Main trading area */}
      <div className="flex flex-1 overflow-hidden">
        {/* Left: Orderbook */}
        <div className="w-80 border-r border-border">
          <Orderbook />
        </div>

        {/* Center: Chart */}
        <div className="flex flex-1 flex-col">
          <div className="flex-1 border-b border-border">
            <Chart />
          </div>

          {/* Bottom tabs */}
          <div className="h-64">
            <BottomTabs />
          </div>
        </div>

        {/* Right: Trade Panel */}
        <div className="w-96 border-l border-border">
          <TradePanel />
        </div>
      </div>
    </div>
  )
}
