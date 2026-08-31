'use client'

import { BrowserProvider, type Eip1193Provider, type JsonRpcSigner } from 'ethers'
import { useCallback, useEffect, useMemo, useRef } from 'react'
import { toast } from '@/components/ui/Toast'
import { convertPrice, convertSize, getOrders, getPositions } from '../api'
import { isDevelopment } from '../config'
import { useTradingStore, useWalletStore } from '../store'
import { normalizeAddress } from '../wallet/canonicalAction'
import * as handlers from './handlers'
import {
  buildAuthenticatedSubscriptionFrame,
  createSubscriptionAuth,
  type PendingSubscription,
} from './subscriptionAuth'
import type { WSMessage } from './types'

const WS_URL = process.env.NEXT_PUBLIC_WS_URL || 'ws://localhost:8080/ws'
const PRIVATE_EVENT_TYPES = new Set([
  'userFill',
  'transactionFinalized',
  'orderUpdate',
  'orderClosed',
  'positionUpdate',
  'balanceUpdate',
  'fundingPayment',
  'liquidated',
  'adl',
  'triggerOrderPlaced',
  'triggerOrderTriggered',
  'triggerOrderCancelled',
])

/**
 * Hook for WebSocket connection lifecycle and message handling
 */
export function useWebSocketConnection() {
  console.log('[ws] useWebSocket hook initialized')
  const wsRef = useRef<WebSocket | null>(null)
  const reconnectTimeoutRef = useRef<NodeJS.Timeout | undefined>(undefined)
  const subscribedAddressRef = useRef<string | null>(null)

  // Extract actions as individual selectors (stable references)
  const setWsConnected = useTradingStore((s) => s.setWsConnected)
  const setOpenOrders = useTradingStore((s) => s.setOpenOrders)
  const setPositions = useTradingStore((s) => s.setPositions)
  const clearUserFills = useTradingStore((s) => s.clearUserFills)
  const clearTriggerOrders = useTradingStore((s) => s.clearTriggerOrders)

  const walletConnected = useWalletStore((s) => s.isConnected)
  const address = useWalletStore((s) => s.address)
  const pendingSubscriptionRef = useRef<PendingSubscription | null>(null)

  const getConnectedSigner = useCallback(async (): Promise<JsonRpcSigner> => {
    if (typeof window === 'undefined') {
      throw new Error('Wallet signing is only available in a browser')
    }

    const ethereum = (window as Window & { ethereum?: Eip1193Provider }).ethereum
    if (!ethereum) {
      throw new Error('No connected wallet was found')
    }

    return new BrowserProvider(ethereum).getSigner()
  }, [])

  const subscribeToUser = useCallback(async (ws: WebSocket, requestedAddress: string) => {
    let normalizedAddress: string
    try {
      normalizedAddress = normalizeAddress(requestedAddress)
    } catch {
      toast.error('Private updates unavailable', 'The connected wallet address is invalid')
      return
    }

    // Do not start a second signature request for the same socket/address.
    const pending = pendingSubscriptionRef.current
    if (pending?.socket === ws && pending.address === normalizedAddress) return
    pendingSubscriptionRef.current = { socket: ws, address: normalizedAddress }

    try {
      const frame = isDevelopment
        ? { op: 'subscribe' as const, address: normalizedAddress }
        : buildAuthenticatedSubscriptionFrame(
            await createSubscriptionAuth(
              await getConnectedSigner(),
              normalizedAddress,
            ),
          )

      // The wallet or socket may have changed while the user approved the
      // signature. Never send an old signature to a new connection/address.
      const currentAddress = useWalletStore.getState().address
      const isCurrent = wsRef.current === ws
        && ws.readyState === WebSocket.OPEN
        && currentAddress !== null
        && normalizeAddress(currentAddress) === normalizedAddress

      if (!isCurrent) {
        const currentPending = pendingSubscriptionRef.current
        if (currentPending?.socket === ws && currentPending.address === normalizedAddress) {
          pendingSubscriptionRef.current = null
        }
        return
      }

      ws.send(JSON.stringify(frame))
    } catch (error) {
      const currentPending = pendingSubscriptionRef.current
      const isCurrentRequest = currentPending?.socket === ws
        && currentPending.address === normalizedAddress
      if (isCurrentRequest) pendingSubscriptionRef.current = null

      if (wsRef.current === ws) {
        const message = error instanceof Error && error.message
          ? error.message
          : 'Wallet signature is required for private updates'
        toast.error('Private updates unavailable', message)
        console.error('[ws] User subscription authentication failed:', error)
      }
    }
  }, [getConnectedSigner])

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
    } catch (_error) {
      // Silently fail - user data fetch is best effort
    }
  }, [setOpenOrders, setPositions])

  // Handler dependencies (memoized to avoid re-creating every render)
  const deps = useMemo(() => ({
    fetchUserData,
    subscribedAddressRef,
    pendingSubscriptionRef,
  }), [fetchUserData])

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
          void subscribeToUser(ws, address)
        }
      }

      ws.onmessage = (event) => {
        if (wsRef.current !== ws) return

        try {
          const data = JSON.parse(event.data)
          handleMessage(data, { ...deps, socket: ws })
        } catch (_err) {
          // Silently ignore parse errors
        }
      }

      ws.onerror = () => {
        console.error('[ws] WebSocket error - connection failed or was rejected')
      }

      ws.onclose = (event) => {
        console.log('[ws] Connection closed. Code:', event.code, 'Reason:', event.reason)
        if (wsRef.current !== ws) return

        setWsConnected(false)
        wsRef.current = null
        subscribedAddressRef.current = null
        pendingSubscriptionRef.current = null

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
  }, [walletConnected, address, subscribeToUser, deps, setWsConnected])

  // Subscribe/unsubscribe when wallet connection changes
  useEffect(() => {
    const ws = wsRef.current
    if (!ws || ws.readyState !== WebSocket.OPEN) return

    if (walletConnected && address) {
      let normalizedAddress: string
      try {
        normalizedAddress = normalizeAddress(address)
      } catch {
        toast.error('Private updates unavailable', 'The connected wallet address is invalid')
        return
      }

      if (
        subscribedAddressRef.current !== normalizedAddress
        && !(
          pendingSubscriptionRef.current?.socket === ws
          && pendingSubscriptionRef.current.address === normalizedAddress
        )
      ) {
        void subscribeToUser(ws, normalizedAddress)
      }
    } else if (!walletConnected && subscribedAddressRef.current) {
      ws.send(JSON.stringify({ op: 'unsubscribe', address: subscribedAddressRef.current }))
      subscribedAddressRef.current = null
      setOpenOrders([])
      setPositions([])
      clearUserFills()
      clearTriggerOrders()
    }
  }, [walletConnected, address, subscribeToUser, setOpenOrders, setPositions, clearUserFills, clearTriggerOrders])

  return wsRef.current
}

// Message router
function handleMessage(
  data: WSMessage,
  deps: {
    fetchUserData: (address: string) => Promise<void>
    subscribedAddressRef: React.MutableRefObject<string | null>
    pendingSubscriptionRef: React.MutableRefObject<PendingSubscription | null>
    socket: WebSocket
  }
) {
  if (PRIVATE_EVENT_TYPES.has(data.type) && !deps.subscribedAddressRef.current) return

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
    case 'transactionFinalized':
      handlers.handleTransactionFinalized(data, deps)
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
    case 'adl':
      handlers.handleADL(data, deps)
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
    case 'error':
      handlers.handleSubscriptionError(data, deps)
      break
  }
}
