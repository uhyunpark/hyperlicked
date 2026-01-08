'use client'

import { useEffect, useRef } from 'react'
import { useWalletStore, useTradingStore } from './store'
import { getOrders, getPositions, convertPrice, convertSize } from './api'

/**
 * Hook to fetch user data (orders, positions) when wallet is connected.
 * Automatically refreshes on an interval.
 */
export function useUserData() {
  const { isConnected, address } = useWalletStore()
  const { setOpenOrders, setPositions } = useTradingStore()
  const intervalRef = useRef<NodeJS.Timeout | null>(null)

  useEffect(() => {
    if (!isConnected || !address) {
      // Clear data when disconnected
      setOpenOrders([])
      setPositions([])
      return
    }

    const fetchData = async () => {
      try {
        // Fetch orders
        const ordersData = await getOrders(address)
        const openOrders = ordersData
          .filter(o => o.status === 'open' || o.status === 'partial')
          .map(o => {
            const size = convertSize(o.size)
            const filled = convertSize(o.filled)
            return {
              id: o.id,
              symbol: o.symbol,
              side: o.side as 'buy' | 'sell',
              type: (o.type || 'limit') as 'limit' | 'market' | 'stop',
              price: convertPrice(o.price),
              size,
              filled,
              remaining: size - filled,
              status: o.status === 'partial' ? 'open' : o.status as 'open' | 'filled' | 'cancelled',
              timestamp: o.timestamp
            }
          })
        setOpenOrders(openOrders)

        // Fetch positions
        const positionsData = await getPositions(address)
        const positions = positionsData.map(p => ({
          symbol: p.symbol,
          size: convertSize(p.size),
          entryPrice: convertPrice(p.entryPrice),
          markPrice: convertPrice(p.markPrice),
          liquidationPrice: convertPrice(p.liquidationPrice),
          unrealizedPnl: convertPrice(p.unrealizedPnl),
          margin: convertPrice(p.margin),
          leverage: p.leverage
        }))
        setPositions(positions)

      } catch (error) {
        console.error('[user-data] Failed to fetch:', error)
      }
    }

    // Fetch immediately
    fetchData()

    // Refresh every 5 seconds
    intervalRef.current = setInterval(fetchData, 5000)

    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current)
      }
    }
  }, [isConnected, address, setOpenOrders, setPositions])
}
