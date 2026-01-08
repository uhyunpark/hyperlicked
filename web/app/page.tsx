'use client'

import { useEffect, useState } from 'react'
import { Header } from '@/components/trading/Header'
import { Orderbook } from '@/components/trading/Orderbook'
import { Chart } from '@/components/trading/Chart'
import { TradePanel } from '@/components/trading/TradePanel'
import { BottomTabs } from '@/components/trading/BottomTabs'
import { useWebSocket } from '@/lib/useWebSocket'
import { useUserData } from '@/lib/useUserData'
import { useTradingStore } from '@/lib/store'

export default function TradingPage() {
  const [isConnected, setIsConnected] = useState(false)
  const orderbook = useTradingStore((state) => state.orderbook)

  // Connect to WebSocket for real-time updates
  const ws = useWebSocket()

  // Fetch user data (orders, positions) when wallet is connected
  useUserData()

  // Check connection based on receiving orderbook data
  useEffect(() => {
    // Connected if we have orderbook data (proves WebSocket is working)
    if (orderbook && (orderbook.bids.length > 0 || orderbook.asks.length > 0)) {
      setIsConnected(true)
    }
  }, [orderbook])

  // Also check WebSocket state directly with polling
  useEffect(() => {
    const checkConnection = () => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        setIsConnected(true)
      }
    }

    // Check immediately and then every 500ms
    checkConnection()
    const interval = setInterval(checkConnection, 500)

    return () => clearInterval(interval)
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
