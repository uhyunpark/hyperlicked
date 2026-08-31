import { toast } from '@/components/ui/Toast'
import { convertPrice, convertSize } from '../api'
import { useTradingStore } from '../store'
import type { OrderbookData } from '../types'
import type { PendingSubscription } from './subscriptionAuth'
import type {
  WSAssetCtx,
  WSAutoDeleveraged,
  WSFundingPayment,
  WSLiquidated,
  WSMarkPriceUpdate,
  WSOrderbookUpdate,
  WSOrderClosed,
  WSPositionUpdate,
  WSSubscribed,
  WSSubscriptionError,
  WSTradeUpdate,
  WSTransactionFinalized,
  WSTriggerOrderCancelled,
  WSTriggerOrderPlaced,
  WSTriggerOrderTriggered,
  WSUserFill,
} from './types'

interface HandlerDependencies {
  fetchUserData: (address: string) => Promise<void>
  subscribedAddressRef: React.MutableRefObject<string | null>
  pendingSubscriptionRef: React.MutableRefObject<PendingSubscription | null>
  socket: WebSocket
}

export function handleOrderbook(data: WSOrderbookUpdate) {
  const orderbook: OrderbookData = {
    symbol: data.symbol,
    bids: data.bids.map(b => ({
      price: convertPrice(b.price),
      size: convertSize(b.size)
    })),
    asks: data.asks.map(a => ({
      price: convertPrice(a.price),
      size: convertSize(a.size)
    })),
    timestamp: data.timestamp
  }
  useTradingStore.getState().updateOrderbook(orderbook)
}

export function handleTrade(data: WSTradeUpdate) {
  useTradingStore.getState().addTrade({
    id: data.id,
    symbol: data.symbol,
    price: convertPrice(data.price),
    size: convertSize(data.size),
    side: data.side,
    timestamp: data.timestamp
  })
}

export function handleUserFill(
  data: WSUserFill,
  deps: HandlerDependencies
) {
  if (!deps.subscribedAddressRef.current) return

  useTradingStore.getState().addUserFill({
    id: data.orderId,
    symbol: data.symbol,
    side: data.side as 'buy' | 'sell',
    price: convertPrice(data.price),
    size: convertSize(data.size),
    fee: convertPrice(data.fee),
    isMaker: data.isMaker,
    timestamp: data.timestamp,
  })
  if (deps.subscribedAddressRef.current) {
    deps.fetchUserData(deps.subscribedAddressRef.current)
  }
}

/**
 * Refresh user state only after the node has durably committed the receipt.
 * The receipt itself remains available to clients through the transaction API;
 * this notification deliberately does not infer a semantic UI event.
 */
export function handleTransactionFinalized(
  data: WSTransactionFinalized,
  deps: HandlerDependencies
) {
  if (!deps.subscribedAddressRef.current) return

  console.log('[ws] Transaction finalized:', data.tx_hash)
  useTradingStore.getState().triggerBalanceRefresh()
  if (deps.subscribedAddressRef.current) {
    deps.fetchUserData(deps.subscribedAddressRef.current)
  }
}

export function handlePositionUpdate(data: WSPositionUpdate) {
  const updatedPosition = {
    symbol: data.symbol,
    size: convertSize(data.size),
    entryPrice: convertPrice(data.entryPrice),
    markPrice: convertPrice(data.markPrice),
    liquidationPrice: convertPrice(data.liquidationPrice),
    unrealizedPnl: convertPrice(data.unrealizedPnl),
    margin: convertPrice(data.margin),
    leverage: data.leverage,
  }

  useTradingStore.setState((state) => {
    const existingIdx = state.positions.findIndex(p => p.symbol === data.symbol)
    if (updatedPosition.size === 0) {
      return { positions: state.positions.filter(p => p.symbol !== data.symbol) }
    } else if (existingIdx >= 0) {
      const newPositions = [...state.positions]
      newPositions[existingIdx] = updatedPosition
      return { positions: newPositions }
    } else {
      return { positions: [...state.positions, updatedPosition] }
    }
  })
}

export function handleBalanceUpdate(deps: HandlerDependencies) {
  console.log('[ws] Balance update received')
  if (deps.subscribedAddressRef.current) {
    deps.fetchUserData(deps.subscribedAddressRef.current)
  }
  useTradingStore.getState().triggerBalanceRefresh()
}

export function handleFundingPayment(
  data: WSFundingPayment,
  deps: HandlerDependencies
) {
  const paymentUsd = convertPrice(data.payment)
  const sign = paymentUsd >= 0 ? '+' : ''
  console.log(`[ws] Funding payment: ${sign}$${paymentUsd.toFixed(2)} for ${data.symbol}`)

  if (paymentUsd >= 0) {
    toast.success('Funding Received', `${sign}$${paymentUsd.toFixed(2)} on ${data.symbol}`)
  } else {
    toast.info('Funding Paid', `$${Math.abs(paymentUsd).toFixed(2)} on ${data.symbol}`)
  }

  useTradingStore.getState().triggerBalanceRefresh()
  if (deps.subscribedAddressRef.current) {
    deps.fetchUserData(deps.subscribedAddressRef.current)
  }
}

export function handleLiquidated(
  data: WSLiquidated,
  deps: HandlerDependencies
) {
  const pnlUsd = convertPrice(data.pnl)
  console.log(`[ws] Liquidated: ${data.symbol} position (${data.wasLong ? 'long' : 'short'})`)

  toast.error('Position Liquidated',
    `${data.symbol} ${data.wasLong ? 'long' : 'short'} liquidated. PnL: $${pnlUsd.toFixed(2)}`)

  useTradingStore.getState().triggerBalanceRefresh()
  if (deps.subscribedAddressRef.current) {
    deps.fetchUserData(deps.subscribedAddressRef.current)
  }
}

export function handleADL(
  data: WSAutoDeleveraged,
  deps: HandlerDependencies
) {
  const sizeReduced = convertSize(data.size_reduced)
  const realizedPnlUsd = convertPrice(data.realized_pnl)
  console.log(`[ws] ADL: ${data.symbol} position reduced by ${sizeReduced}`)

  toast.warning('Position Auto-Deleveraged',
    `${data.symbol} reduced by ${sizeReduced.toFixed(4)}. PnL: $${realizedPnlUsd.toFixed(2)}`)

  useTradingStore.getState().triggerBalanceRefresh()
  if (deps.subscribedAddressRef.current) {
    deps.fetchUserData(deps.subscribedAddressRef.current)
  }
}

export function handleMarkPrice(data: WSMarkPriceUpdate) {
  useTradingStore.getState().updateMarkPrice(data.symbol, convertPrice(data.markPrice))
}

export function handleAssetCtx(data: WSAssetCtx) {
  const store = useTradingStore.getState()
  const markPrice = convertPrice(data.markPrice)

  // Update markPrices for real-time PnL calculation
  store.updateMarkPrice(data.symbol, markPrice)

  // Update full asset context
  store.updateAssetCtx({
    markPrice,
    oraclePrice: data.oraclePrice ? convertPrice(data.oraclePrice) : null,
    midPrice: convertPrice(data.midPrice),
    fundingRate: data.fundingRate / 1_000_000,
    premium: data.premium / 1_000_000,
    openInterest: convertSize(data.openInterest),
    prevDayPrice: convertPrice(data.prevDayPrice),
    dayVolume: convertSize(data.dayVolume),
    dayNotionalVolume: convertPrice(data.dayNotionalVolume),
    nextFundingTime: data.nextFundingTime,
  })
}

export function handleTriggerOrderPlaced(data: WSTriggerOrderPlaced) {
  console.log('[ws] Trigger order placed:', data.id, data.triggerType)
  useTradingStore.getState().addTriggerOrder({
    id: data.id,
    symbol: data.symbol,
    triggerType: data.triggerType as 'sl' | 'tp',
    side: data.triggerType === 'sl' ? 'sell' : 'buy',
    triggerPrice: convertPrice(data.triggerPrice),
    size: convertSize(data.size),
    status: 'pending',
    timestamp: data.timestamp,
  })
}

export function handleTriggerOrderTriggered(
  data: WSTriggerOrderTriggered,
  deps: HandlerDependencies
) {
  console.log('[ws] Trigger order triggered:', data.id, '-> order', data.orderId)
  useTradingStore.getState().removeTriggerOrder(data.id)
  toast.success('Trigger Activated', `${data.symbol} TP/SL triggered`)
  if (deps.subscribedAddressRef.current) {
    deps.fetchUserData(deps.subscribedAddressRef.current)
  }
}

export function handleTriggerOrderCancelled(data: WSTriggerOrderCancelled) {
  console.log('[ws] Trigger order cancelled:', data.id)
  useTradingStore.getState().removeTriggerOrder(data.id)
}

export function handleOrderClosed(data: WSOrderClosed) {
  console.log(`[ws] Order closed: ${data.orderId} - ${data.status}`)
}

export function handleSubscribed(
  data: WSSubscribed,
  deps: HandlerDependencies
) {
  if (data.channel === 'user') {
    const pending = deps.pendingSubscriptionRef.current
    if (!pending || pending.socket !== deps.socket || pending.address !== data.address.toLowerCase()) {
      return
    }

    deps.pendingSubscriptionRef.current = null
    deps.subscribedAddressRef.current = data.address
    deps.fetchUserData(data.address)
  }
}

export function handleSubscriptionError(
  data: WSSubscriptionError,
  deps: HandlerDependencies,
) {
  if (data.code !== 'AUTH_REQUIRED') return

  const pending = deps.pendingSubscriptionRef.current
  if (pending && pending.socket !== deps.socket) return

  deps.pendingSubscriptionRef.current = null
  deps.subscribedAddressRef.current = null
  toast.error('Private updates unavailable', data.message || 'Wallet signature was rejected or expired')
}
