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

export interface ApiOrder {
  id: string
  symbol: string
  side: string        // "buy" or "sell"
  type: string        // "limit", "market", etc.
  price: number       // USDT cents
  size: number        // Satoshis
  filled: number      // Satoshis filled
  status: string      // "open", "partial", "filled", "cancelled"
  timestamp: number
  owner: string
}

export async function getOrders(address: string): Promise<ApiOrder[]> {
  const res = await fetch(`${API_BASE}/accounts/${address}/orders`)
  if (!res.ok) throw new Error(`Failed to fetch orders: ${res.statusText}`)
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
    // Backend returns plain text errors, not JSON
    const errorText = await res.text()
    throw new Error(errorText || res.statusText)
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

// ==============================
// Dev Faucet API
// ==============================

// Request test USDT from faucet (dev mode only)
// Default: $100,000 (10_000_000 cents)
export async function requestFaucet(address: string, amount: number = 10_000_000): Promise<boolean> {
  const res = await fetch(`${API_BASE}/deposit`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ trader: address, amount })
  })

  const data = await res.json()
  return data.success === true
}

// ==============================
// Funding Rate API
// ==============================

export interface ApiFundingInfo {
  symbol: string
  fundingRate: number      // Decimal (0.0001 = 0.01%)
  fundingRateBps: number   // Basis points
  nextFundingTime: number  // ms timestamp
  lastFundingTime: number  // ms timestamp
}

export interface ApiFundingPayment {
  symbol: string
  payment: number          // cents (negative = paid out)
  paymentUsd: number       // dollars
  fundingRateBps: number
  timestamp: number
}

export async function getFunding(symbol: string): Promise<ApiFundingInfo> {
  const res = await fetch(`${API_BASE}/markets/${symbol}/funding`)
  if (!res.ok) throw new Error(`Failed to fetch funding: ${res.statusText}`)
  return res.json()
}

export async function getAccountFunding(address: string): Promise<ApiFundingPayment[]> {
  const res = await fetch(`${API_BASE}/accounts/${address}/funding`)
  if (!res.ok) throw new Error(`Failed to fetch account funding: ${res.statusText}`)
  return res.json()
}

// ==============================
// Trade History API
// ==============================

export interface ApiTrade {
  id: string         // Deterministic trade ID for deduplication
  price: number      // cents
  size: number       // satoshis
  side: string       // "buy" or "sell"
  timestamp: number  // ms
}

export async function getTrades(symbol: string, limit: number = 100): Promise<ApiTrade[]> {
  const res = await fetch(`${API_BASE}/markets/${symbol}/trades?limit=${limit}`)
  if (!res.ok) throw new Error(`Failed to fetch trades: ${res.statusText}`)
  return res.json()
}

// ==============================
// Candle (OHLCV) API
// ==============================

export type CandleInterval = '1m' | '5m' | '15m' | '1h' | '4h' | '1d'

export interface ApiCandle {
  t: number  // Candle open time (ms)
  o: number  // Open price (cents)
  h: number  // High price (cents)
  l: number  // Low price (cents)
  c: number  // Close price (cents)
  v: number  // Volume (satoshis)
  n: number  // Number of trades
}

export async function getCandles(
  symbol: string,
  interval: CandleInterval = '1m',
  limit: number = 500
): Promise<ApiCandle[]> {
  const res = await fetch(`${API_BASE}/markets/${symbol}/candles?interval=${interval}&limit=${limit}`)
  if (!res.ok) throw new Error(`Failed to fetch candles: ${res.statusText}`)
  return res.json()
}

// ==============================
// Insurance Fund API
// ==============================

export interface ApiInsuranceFund {
  balance: number      // cents
  balance_usd: number  // dollars
}

export async function getInsuranceFund(): Promise<ApiInsuranceFund> {
  const res = await fetch(`${API_BASE}/chain/insurance-fund`)
  if (!res.ok) throw new Error(`Failed to fetch insurance fund: ${res.statusText}`)
  return res.json()
}

// ==============================
// Nonce API
// ==============================

export interface ApiNonce {
  address: string
  nonce: number
}

export async function getNonce(address: string): Promise<ApiNonce> {
  const res = await fetch(`${API_BASE}/accounts/${address}/nonce`)
  if (!res.ok) throw new Error(`Failed to fetch nonce: ${res.statusText}`)
  return res.json()
}
