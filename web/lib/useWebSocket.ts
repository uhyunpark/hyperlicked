'use client'

import { useEffect, useRef, useCallback } from 'react'
import { useTradingStore, useWalletStore } from './store'
import { convertPrice, convertSize, getOrders, getPositions } from './api'
import type { OrderbookData } from './types'

const WS_URL = process.env.NEXT_PUBLIC_WS_URL || 'ws://localhost:8080/ws'

// Public event types
interface WSOrderbookUpdate {
  type: 'orderbook'
  symbol: string
  bids: Array<{ price: number; size: number }>
  asks: Array<{ price: number; size: number }>
  timestamp: number
}

interface WSTradeUpdate {
  type: 'trade'
  symbol: string
  price: number
  size: number
  side: 'buy' | 'sell'
  timestamp: number
}

// User-specific event types
interface WSUserFill {
  type: 'userFill'
  symbol: string
  orderId: string
  side: string
  price: number
  size: number
  fee: number
  isMaker: boolean
  timestamp: number
}

interface WSOrderUpdate {
  type: 'orderUpdate'
  orderId: string
  symbol: string
  status: string
  filled: number
  remaining: number
  timestamp: number
}

interface WSPositionUpdate {
  type: 'positionUpdate'
  symbol: string
  size: number
  entryPrice: number
  markPrice: number
  unrealizedPnl: number
  timestamp: number
}

interface WSSubscribed {
  type: 'subscribed'
  channel: string
  address: string
}

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null)
  const reconnectTimeoutRef = useRef<NodeJS.Timeout | undefined>(undefined)
  const subscribedAddressRef = useRef<string | null>(null)

  const { updateOrderbook, addTrade, setWsConnected, setOpenOrders, setPositions } = useTradingStore()
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
          // Normalize order type - only 'limit' and 'market' are supported
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

  useEffect(() => {
    function connect() {
      const ws = new WebSocket(WS_URL)
      wsRef.current = ws

      ws.onopen = () => {
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

          switch (data.type) {
            case 'orderbook': {
              const update = data as WSOrderbookUpdate
              const orderbook: OrderbookData = {
                symbol: update.symbol,
                bids: update.bids.map(b => ({
                  price: convertPrice(b.price),
                  size: convertSize(b.size)
                })),
                asks: update.asks.map(a => ({
                  price: convertPrice(a.price),
                  size: convertSize(a.size)
                })),
                timestamp: update.timestamp
              }
              updateOrderbook(orderbook)
              break
            }

            case 'trade': {
              const update = data as WSTradeUpdate
              // Generate unique ID: timestamp + price + size + random suffix
              const uniqueId = `${update.timestamp}-${update.price}-${update.size}-${Math.random().toString(36).slice(2, 7)}`
              addTrade({
                id: uniqueId,
                symbol: update.symbol,
                price: convertPrice(update.price),
                size: convertSize(update.size),
                side: update.side,
                timestamp: update.timestamp
              })
              break
            }

            case 'userFill': {
              // User's order was filled - refetch data
              if (subscribedAddressRef.current) {
                fetchUserData(subscribedAddressRef.current)
              }
              break
            }

            case 'orderUpdate': {
              // Order status changed - refetch orders
              if (subscribedAddressRef.current) {
                fetchUserData(subscribedAddressRef.current)
              }
              break
            }

            case 'positionUpdate': {
              // Position changed - refetch positions
              if (subscribedAddressRef.current) {
                fetchUserData(subscribedAddressRef.current)
              }
              break
            }

            case 'subscribed': {
              const sub = data as WSSubscribed
              if (sub.channel === 'user') {
                subscribedAddressRef.current = sub.address
                // Fetch initial data
                fetchUserData(sub.address)
              }
              break
            }
          }
        } catch (err) {
          // Silently ignore parse errors
        }
      }

      ws.onerror = () => {
        // Silently handle errors
      }

      ws.onclose = () => {
        setWsConnected(false)
        wsRef.current = null
        subscribedAddressRef.current = null

        // Attempt to reconnect after 3 seconds
        reconnectTimeoutRef.current = setTimeout(() => {
          connect()
        }, 3000)
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
      // Subscribe to user events
      ws.send(JSON.stringify({
        op: 'subscribe',
        address: address
      }))
    } else if (!walletConnected && subscribedAddressRef.current) {
      // Unsubscribe from user events
      ws.send(JSON.stringify({
        op: 'unsubscribe',
        address: subscribedAddressRef.current
      }))
      subscribedAddressRef.current = null
      setOpenOrders([])
      setPositions([])
    }
  }, [walletConnected, address, setOpenOrders, setPositions])

  return wsRef.current
}
