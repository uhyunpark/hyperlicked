'use client'

import { useEffect, useRef, useCallback } from 'react'
import { useTradingStore, useWalletStore } from './store'
import { convertPrice, convertSize, getOrders, getPositions } from './api'
import { toast } from '@/components/ui/Toast'
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
  id: string  // Deterministic trade ID from server
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

interface WSBalanceUpdate {
  type: 'balanceUpdate'
  balance: number      // cents
  available: number
  locked: number
  timestamp: number
}

interface WSFundingPayment {
  type: 'fundingPayment'
  symbol: string
  payment: number       // cents: positive = received, negative = paid
  positionSize: number
  fundingRateBps: number
  timestamp: number
}

interface WSLiquidated {
  type: 'liquidated'
  symbol: string
  size: number
  price: number
  pnl: number
  wasLong: boolean
  timestamp: number
}

interface WSMarkPriceUpdate {
  type: 'markPrice'
  symbol: string
  markPrice: number
  indexPrice: number | null
  timestamp: number
}

interface WSAssetCtx {
  type: 'assetCtx'
  symbol: string
  markPrice: number        // cents
  oraclePrice: number | null // cents
  midPrice: number         // cents
  fundingRate: number      // 1/1M units
  premium: number          // 1/1M units
  openInterest: number     // satoshis
  prevDayPrice: number     // cents
  dayVolume: number        // satoshis
  dayNotionalVolume: number // cents
  nextFundingTime: number
  timestamp: number
}

interface WSSubscribed {
  type: 'subscribed'
  channel: string
  address: string
}

export function useWebSocket() {
  console.log('[ws] useWebSocket hook initialized')
  const wsRef = useRef<WebSocket | null>(null)
  const reconnectTimeoutRef = useRef<NodeJS.Timeout | undefined>(undefined)
  const subscribedAddressRef = useRef<string | null>(null)

  const { updateOrderbook, addTrade, setWsConnected, setOpenOrders, setPositions, triggerBalanceRefresh, updateMarkPrice, updateAssetCtx } = useTradingStore()
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
      console.log('[ws] Connecting to:', WS_URL)
      const ws = new WebSocket(WS_URL)
      wsRef.current = ws

      ws.onopen = () => {
        console.log('[ws] Connected successfully')
        setWsConnected(true)

        // Subscribe to public channels
        const subscribeMsg = {
          op: 'subscribe',
          channels: ['orderbook:BTC-USDT', 'trades:BTC-USDT']
        }
        console.log('[ws] Subscribing to:', subscribeMsg)
        ws.send(JSON.stringify(subscribeMsg))

        // Subscribe to user events if wallet connected
        if (walletConnected && address) {
          const userSubscribeMsg = {
            op: 'subscribe',
            address: address
          }
          console.log('[ws] Subscribing to user events:', userSubscribeMsg)
          ws.send(JSON.stringify(userSubscribeMsg))
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
              // Use server-provided deterministic ID for deduplication
              addTrade({
                id: update.id,
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

            case 'balanceUpdate': {
              // Balance changed (deposit/withdrawal) - trigger account refresh
              console.log('[ws] Balance update received:', data)
              if (subscribedAddressRef.current) {
                fetchUserData(subscribedAddressRef.current)
              }
              // Also trigger the balance refresh callback
              triggerBalanceRefresh()
              break
            }

            case 'fundingPayment': {
              const fp = data as WSFundingPayment
              const paymentUsd = convertPrice(fp.payment)
              const sign = paymentUsd >= 0 ? '+' : ''
              console.log(`[ws] Funding payment: ${sign}$${paymentUsd.toFixed(2)} for ${fp.symbol}`)
              // Show toast notification
              if (paymentUsd >= 0) {
                toast.success('Funding Received', `${sign}$${paymentUsd.toFixed(2)} on ${fp.symbol}`)
              } else {
                toast.info('Funding Paid', `$${Math.abs(paymentUsd).toFixed(2)} on ${fp.symbol}`)
              }
              // Trigger balance refresh to show updated balance
              triggerBalanceRefresh()
              if (subscribedAddressRef.current) {
                fetchUserData(subscribedAddressRef.current)
              }
              break
            }

            case 'liquidated': {
              const liq = data as WSLiquidated
              const pnlUsd = convertPrice(liq.pnl)
              console.log(`[ws] Liquidated: ${liq.symbol} position (${liq.wasLong ? 'long' : 'short'})`)
              // Show toast notification
              toast.error('Position Liquidated', `${liq.symbol} ${liq.wasLong ? 'long' : 'short'} liquidated. PnL: $${pnlUsd.toFixed(2)}`)
              // Trigger refresh to show position closed
              triggerBalanceRefresh()
              if (subscribedAddressRef.current) {
                fetchUserData(subscribedAddressRef.current)
              }
              break
            }

            case 'markPrice': {
              const mp = data as WSMarkPriceUpdate
              updateMarkPrice(mp.symbol, convertPrice(mp.markPrice))
              break
            }

            case 'assetCtx': {
              const ctx = data as WSAssetCtx
              updateAssetCtx({
                markPrice: convertPrice(ctx.markPrice),
                oraclePrice: ctx.oraclePrice ? convertPrice(ctx.oraclePrice) : null,
                midPrice: convertPrice(ctx.midPrice),
                fundingRate: ctx.fundingRate / 1_000_000, // 1/1M to decimal
                premium: ctx.premium / 1_000_000,         // 1/1M to decimal
                openInterest: convertSize(ctx.openInterest),
                prevDayPrice: convertPrice(ctx.prevDayPrice),
                dayVolume: convertSize(ctx.dayVolume),
                dayNotionalVolume: convertPrice(ctx.dayNotionalVolume),
                nextFundingTime: ctx.nextFundingTime,
              })
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
        // Note: WebSocket onerror event doesn't contain useful error details
        // The actual error info is in the close event that follows
        console.error('[ws] WebSocket error - connection failed or was rejected')
        console.error('[ws] WebSocket readyState:', ws.readyState, '(0=CONNECTING, 1=OPEN, 2=CLOSING, 3=CLOSED)')
      }

      ws.onclose = (event) => {
        console.log('[ws] Connection closed. Code:', event.code, 'Reason:', event.reason)
        setWsConnected(false)
        wsRef.current = null
        subscribedAddressRef.current = null

        // Attempt to reconnect after 3 seconds
        console.log('[ws] Reconnecting in 3 seconds...')
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
