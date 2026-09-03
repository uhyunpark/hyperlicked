// WebSocket message types

// Public event types
export interface WSOrderbookUpdate {
  type: 'orderbook'
  symbol: string
  bids: Array<{ price: number; size: number }>
  asks: Array<{ price: number; size: number }>
  timestamp: number
}

export interface WSTradeUpdate {
  type: 'trade'
  id: string
  symbol: string
  price: number
  size: number
  side: 'buy' | 'sell'
  timestamp: number
}

// User-specific event types
export interface WSUserFill {
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

/** Durable receipt notification for a signed transaction. */
export interface WSTransactionFinalized {
  type: 'transactionFinalized'
  tx_hash: string
  block_height: number
  block_hash: string
  tx_index: number
  tx_type: number
  status: number
  error_code: number
  compute_units: number
  storage_read_bytes: number
  storage_write_bytes: number
  events: Array<{
    event_index: number
    event_type: number
    payload_hex: string
  }>
}

export interface WSOrderUpdate {
  type: 'orderUpdate'
  orderId: string
  symbol: string
  status: string
  filled: number
  remaining: number
  timestamp: number
}

export interface WSPositionUpdate {
  type: 'positionUpdate'
  symbol: string
  size: number
  entryPrice: number
  markPrice: number
  unrealizedPnl: number
  liquidationPrice: number
  margin: number
  leverage: number
  timestamp: number
}

export interface WSBalanceUpdate {
  type: 'balanceUpdate'
  balance: number
  available: number
  locked: number
  timestamp: number
}

export interface WSFundingPayment {
  type: 'fundingPayment'
  symbol: string
  payment: number
  positionSize: number
  fundingRateBps: number
  timestamp: number
}

export interface WSLiquidated {
  type: 'liquidated'
  symbol: string
  size: number
  price: number
  pnl: number
  wasLong: boolean
  timestamp: number
}

export interface WSAutoDeleveraged {
  type: 'adl'
  symbol: string
  size_reduced: number
  close_price: number
  realized_pnl: number
  triggering_liquidation: string
  timestamp: number
}

// Trigger order events
export interface WSTriggerOrderPlaced {
  type: 'triggerOrderPlaced'
  id: string
  symbol: string
  triggerType: string
  triggerPrice: number
  size: number
  timestamp: number
}

export interface WSTriggerOrderTriggered {
  type: 'triggerOrderTriggered'
  id: string
  symbol: string
  orderId: string
  timestamp: number
}

export interface WSTriggerOrderCancelled {
  type: 'triggerOrderCancelled'
  id: string
  symbol: string
  timestamp: number
}

export interface WSOrderClosed {
  type: 'orderClosed'
  orderId: string
  symbol: string
  side: string
  price: number
  size: number
  filled: number
  status: string
  timestamp: number
}

export interface WSMarkPriceUpdate {
  type: 'markPrice'
  symbol: string
  markPrice: number
  indexPrice: number | null
  timestamp: number
}

export interface WSAssetCtx {
  type: 'assetCtx'
  symbol: string
  markPrice: number
  oraclePrice: number | null
  midPrice: number
  fundingRate: number
  premium: number
  openInterest: number
  prevDayPrice: number
  dayVolume: number
  dayNotionalVolume: number
  nextFundingTime: number
  timestamp: number
}

export interface WSSubscribed {
  type: 'subscribed'
  channel: string
  address: string
}

export interface WSSubscriptionError {
  type: 'error'
  code: string
  message: string
}

export type WSMessage =
  | WSOrderbookUpdate
  | WSTradeUpdate
  | WSUserFill
  | WSTransactionFinalized
  | WSOrderUpdate
  | WSPositionUpdate
  | WSBalanceUpdate
  | WSFundingPayment
  | WSLiquidated
  | WSAutoDeleveraged
  | WSTriggerOrderPlaced
  | WSTriggerOrderTriggered
  | WSTriggerOrderCancelled
  | WSOrderClosed
  | WSMarkPriceUpdate
  | WSAssetCtx
  | WSSubscribed
  | WSSubscriptionError
