'use client'

import { useEffect, useRef } from 'react'
import { useTradingStore } from './store'
import { convertPrice, convertSize } from './api'
import type { OrderbookData } from './types'

const WS_URL = process.env.NEXT_PUBLIC_WS_URL || 'ws://localhost:8080/ws'

interface WSOrderbookUpdate {
  type: 'orderbook'
  symbol: string
  bids: Array<{ price: number; size: number }>
  asks: Array<{ price: number; size: number }>
  timestamp: number
  height: number
}

interface WSTradeUpdate {
  type: 'trade'
  symbol: string
  price: number
  size: number
  side: 'buy' | 'sell'
  timestamp: number
  height: number
}

interface WSSubscribeRequest {
  op: 'subscribe' | 'unsubscribe'
  channels: string[]
}

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null)
  const reconnectTimeoutRef = useRef<NodeJS.Timeout | undefined>(undefined)
  const { updateOrderbook, addTrade } = useTradingStore()

  useEffect(() => {
    let isConnected = false

    function connect() {
      console.log('[ws] Connecting to', WS_URL)
      const ws = new WebSocket(WS_URL)
      wsRef.current = ws

      ws.onopen = () => {
        console.log('[ws] Connected!')
        isConnected = true

        // Subscribe to orderbook and trades
        const subscribeMsg: WSSubscribeRequest = {
          op: 'subscribe',
          channels: ['orderbook:BTC-USDT', 'trades:BTC-USDT']
        }
        ws.send(JSON.stringify(subscribeMsg))
        console.log('[ws] Subscribed to channels:', subscribeMsg.channels)
      }

      ws.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data)

          if (data.type === 'orderbook') {
            const update = data as WSOrderbookUpdate

            // Convert API units to display units
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
          } else if (data.type === 'trade') {
            const update = data as WSTradeUpdate

            addTrade({
              id: `${update.height}-${update.timestamp}`,
              symbol: update.symbol,
              price: convertPrice(update.price),
              size: convertSize(update.size),
              side: update.side,
              timestamp: update.timestamp
            })
          }
        } catch (err) {
          console.error('[ws] Failed to parse message:', err)
        }
      }

      ws.onerror = (error) => {
        console.error('[ws] Error:', error)
      }

      ws.onclose = () => {
        console.log('[ws] Disconnected')
        isConnected = false
        wsRef.current = null

        // Attempt to reconnect after 3 seconds
        reconnectTimeoutRef.current = setTimeout(() => {
          console.log('[ws] Attempting to reconnect...')
          connect()
        }, 3000)
      }
    }

    connect()

    // Cleanup on unmount
    return () => {
      if (reconnectTimeoutRef.current) {
        clearTimeout(reconnectTimeoutRef.current)
      }
      if (wsRef.current) {
        wsRef.current.close()
      }
    }
  }, [updateOrderbook, addTrade])

  return wsRef.current
}
