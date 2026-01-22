'use client'

import { useEffect, useRef, useCallback } from 'react'
import { useTradingStore, useWalletStore } from '../store'
import { convertPrice, convertSize, getOrders, getPositions } from '../api'
import * as handlers from './handlers'

const WS_URL = process.env.NEXT_PUBLIC_WS_URL || 'ws://localhost:8080/ws'

/**
 * Hook for WebSocket connection lifecycle and message handling
 */
export function useWebSocketConnection() {
  console.log('[ws] useWebSocket hook initialized')
  const wsRef = useRef<WebSocket | null>(null)
  const reconnectTimeoutRef = useRef<NodeJS.Timeout | undefined>(undefined)
  const subscribedAddressRef = useRef<string | null>(null)

  const {
    updateOrderbook,
    addTrade,
    setWsConnected,
    setOpenOrders,
    setPositions,
    clearUserFills,
    clearTriggerOrders,
  } = useTradingStore()
  const { isConnected: walletConnected, address } = useWalletStore()

  // Fetch user data (orders and positions)
  const fetchUserData = useCallback(async (userAddress: string) => {
    try {
      const [ordersData, positionsData] = await Promise.all([
        getOrders(userAddress),
        getPositions(userAddress)
      ])

      const openOrders = ordersData
        .filter(o => o.status === 'open' || o.status === 'partial')
        .map(o => {
          const size = convertSize(o.size)
          const filled = convertSize(o.filled)
          const orderType: 'limit' | 'market' = o.type === 'market' ? 'market' : 'limit'
          return {
            id: o.id,
            symbol: o.symbol,
            side: o.side as 'buy' | 'sell',
            type: orderType,
            price: convertPrice(o.price),
            size,
            filled,
            remaining: size - filled,
            status: o.status === 'partial' ? 'open' : o.status as 'open' | 'filled' | 'cancelled',
            timestamp: o.timestamp
          }
        })
      setOpenOrders(openOrders)

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
      // Silently fail - user data fetch is best effort
    }
  }, [setOpenOrders, setPositions])

  // Handler dependencies
  const deps = { fetchUserData, subscribedAddressRef }

  useEffect(() => {
    function connect() {
      console.log('[ws] Connecting to:', WS_URL)
      const ws = new WebSocket(WS_URL)
      wsRef.current = ws

      ws.onopen = () => {
        console.log('[ws] Connected successfully')
        setWsConnected(true)

        // Subscribe to public channels
        ws.send(JSON.stringify({
          op: 'subscribe',
          channels: ['orderbook:BTC-USDT', 'trades:BTC-USDT']
        }))

        // Subscribe to user events if wallet connected
        if (walletConnected && address) {
          ws.send(JSON.stringify({
            op: 'subscribe',
            address: address
          }))
          subscribedAddressRef.current = address
        }
      }

      ws.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data)
          handleMessage(data, deps)
        } catch (err) {
          // Silently ignore parse errors
        }
      }

      ws.onerror = () => {
        console.error('[ws] WebSocket error - connection failed or was rejected')
      }

      ws.onclose = (event) => {
        console.log('[ws] Connection closed. Code:', event.code, 'Reason:', event.reason)
        setWsConnected(false)
        wsRef.current = null
        subscribedAddressRef.current = null

        // Reconnect after 3 seconds
        console.log('[ws] Reconnecting in 3 seconds...')
        reconnectTimeoutRef.current = setTimeout(connect, 3000)
      }
    }

    connect()

    return () => {
      if (reconnectTimeoutRef.current) {
        clearTimeout(reconnectTimeoutRef.current)
      }
      if (wsRef.current) {
        wsRef.current.close()
      }
    }
  }, [updateOrderbook, addTrade, setWsConnected, fetchUserData, walletConnected, address])

  // Subscribe/unsubscribe when wallet connection changes
  useEffect(() => {
    const ws = wsRef.current
    if (!ws || ws.readyState !== WebSocket.OPEN) return

    if (walletConnected && address && subscribedAddressRef.current !== address) {
      ws.send(JSON.stringify({ op: 'subscribe', address }))
    } else if (!walletConnected && subscribedAddressRef.current) {
      ws.send(JSON.stringify({ op: 'unsubscribe', address: subscribedAddressRef.current }))
      subscribedAddressRef.current = null
      setOpenOrders([])
      setPositions([])
      clearUserFills()
      clearTriggerOrders()
    }
  }, [walletConnected, address, setOpenOrders, setPositions, clearUserFills, clearTriggerOrders])

  return wsRef.current
}

// Message router
function handleMessage(
  data: any,
  deps: { fetchUserData: (address: string) => Promise<void>; subscribedAddressRef: React.MutableRefObject<string | null> }
) {
  switch (data.type) {
    case 'orderbook':
      handlers.handleOrderbook(data)
      break
    case 'trade':
      handlers.handleTrade(data)
      break
    case 'userFill':
      handlers.handleUserFill(data, deps)
      break
    case 'orderUpdate':
      if (deps.subscribedAddressRef.current) {
        deps.fetchUserData(deps.subscribedAddressRef.current)
      }
      break
    case 'positionUpdate':
      handlers.handlePositionUpdate(data)
      break
    case 'balanceUpdate':
      handlers.handleBalanceUpdate(deps)
      break
    case 'fundingPayment':
      handlers.handleFundingPayment(data, deps)
      break
    case 'liquidated':
      handlers.handleLiquidated(data, deps)
      break
    case 'markPrice':
      handlers.handleMarkPrice(data)
      break
    case 'assetCtx':
      handlers.handleAssetCtx(data)
      break
    case 'triggerOrderPlaced':
      handlers.handleTriggerOrderPlaced(data)
      break
    case 'triggerOrderTriggered':
      handlers.handleTriggerOrderTriggered(data, deps)
      break
    case 'triggerOrderCancelled':
      handlers.handleTriggerOrderCancelled(data)
      break
    case 'orderClosed':
      handlers.handleOrderClosed(data)
      break
    case 'subscribed':
      handlers.handleSubscribed(data, deps)
      break
  }
}
