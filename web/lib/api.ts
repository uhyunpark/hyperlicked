// API client for HyperLicked blockchain

import { config } from './config'

const API_BASE = config.api.baseUrl

export interface ApiMarket {
  symbol: string
  baseAsset: string
  quoteAsset: string
  type: string
  status: string
  tickSize: number
  lotSize: number
  maxLeverage: number
  takerFeeBps: number
  makerFeeBps: number
  maintenanceMarginBps: number
}

export interface ApiPriceLevel {
  price: number // USDT cents (5000000 = $50,000.00)
  size: number  // Satoshis (100000000 = 1 BTC)
}

export interface ApiOrderbook {
  symbol: string
  bids: ApiPriceLevel[]
  asks: ApiPriceLevel[]
  timestamp: number
}

export interface ApiAccount {
  address: string
  balance: number           // USDT cents
  lockedCollateral: number
  availableBalance: number
  unrealizedPnL: number
  totalEquity: number
}

export interface ApiPosition {
  symbol: string
  size: number             // Positive = long, negative = short
  entryPrice: number
  markPrice: number
  liquidationPrice: number
  unrealizedPnl: number
  margin: number
  leverage: number
}

export interface ApiOrderResponse {
  status: 'submitted' | 'rejected'
  orderId: string
  message?: string
}

// Signed transaction format (matches backend)
export interface SignedTransaction {
  type: 'order' | 'cancel'
  order?: {
    symbol: string
    side: number      // 1=Buy, 2=Sell
    type: number      // 1=GTC, 2=IOC, 3=ALO
    price: string     // BigInt as string
    qty: string       // BigInt as string
    nonce: string     // BigInt as string
    deadline: string  // BigInt as string
    leverage: number
    owner: string     // Address
  }
  cancel?: {
    order_id: string  // Order ID to cancel
    symbol: string
    nonce: string     // BigInt as string
    owner: string     // Address
  }
  signature: string   // Hex-encoded signature
  agent_mode?: boolean
  delegation_id?: string
}

// Helper to convert API units to display units
export const convertPrice = (cents: number): number => cents / 100 // cents → dollars
export const convertSize = (sats: number): number => sats / 100000000 // sats → BTC
export const convertToApiPrice = (dollars: number): number => Math.round(dollars * 100)
export const convertToApiSize = (btc: number): number => Math.round(btc * 100000000)

// API Methods

export async function getMarkets(): Promise<ApiMarket[]> {
  const res = await fetch(`${API_BASE}/markets`)
  if (!res.ok) throw new Error(`Failed to fetch markets: ${res.statusText}`)
  return res.json()
}

export async function getMarket(symbol: string): Promise<ApiMarket> {
  const res = await fetch(`${API_BASE}/markets/${symbol}`)
  if (!res.ok) throw new Error(`Failed to fetch market: ${res.statusText}`)
  return res.json()
}

export async function getOrderbook(symbol: string): Promise<ApiOrderbook> {
  const res = await fetch(`${API_BASE}/markets/${symbol}/orderbook`)
  if (!res.ok) throw new Error(`Failed to fetch orderbook: ${res.statusText}`)
  return res.json()
}

export async function getAccount(address: string): Promise<ApiAccount> {
  const res = await fetch(`${API_BASE}/accounts/${address}`)
  if (!res.ok) throw new Error(`Failed to fetch account: ${res.statusText}`)
  return res.json()
}

export async function getPositions(address: string): Promise<ApiPosition[]> {
  const res = await fetch(`${API_BASE}/accounts/${address}/positions`)
  if (!res.ok) throw new Error(`Failed to fetch positions: ${res.statusText}`)
  return res.json()
}

// Cancel order with signed transaction (deprecated - use submitSignedTransaction instead)
export async function cancelOrder(orderId: string, address: string): Promise<{ status: string }> {
  const res = await fetch(`${API_BASE}/orders/cancel`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ orderId, address })
  })

  if (!res.ok) {
    const error = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error(error.message || error.error || 'Failed to cancel order')
  }

  return res.json()
}

// Submit a signed cancel transaction (EIP-712 format)
export async function submitCancelOrder(signedCancel: SignedTransaction): Promise<{ status: string; orderId: string }> {
  if (signedCancel.type !== 'cancel') {
    throw new Error('Transaction type must be "cancel"')
  }

  const res = await fetch(`${API_BASE}/orders/cancel`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(signedCancel)
  })

  if (!res.ok) {
    const error = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error(error.message || error.error || 'Failed to submit cancel order')
  }

  return res.json()
}

// Submit a signed transaction (EIP-712 format)
export async function submitSignedTransaction(signedTx: SignedTransaction): Promise<ApiOrderResponse> {
  const res = await fetch(`${API_BASE}/orders`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(signedTx)
  })

  if (!res.ok) {
    const error = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error(error.message || error.error || 'Failed to submit signed transaction')
  }

  return res.json()
}

// ==============================
// Agent Delegation API
// ==============================

export interface RegisterDelegationRequest {
  wallet: string       // Main wallet address (0x...)
  agent: string        // Agent key address (0x...)
  expiration: string   // Unix timestamp (BigInt as string)
  nonce: string        // Nonce (BigInt as string)
  signature: string    // EIP-712 signature from wallet (0x...)
}

export interface RegisterDelegationResponse {
  status: string        // "registered"
  delegationId: string  // ID to use in agent-signed orders
  message: string
}

// Register an agent key delegation with the backend
export async function registerDelegation(req: RegisterDelegationRequest): Promise<RegisterDelegationResponse> {
  const res = await fetch(`${API_BASE}/delegations`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req)
  })

  if (!res.ok) {
    const error = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error(error.message || error.error || 'Failed to register delegation')
  }

  return res.json()
}
