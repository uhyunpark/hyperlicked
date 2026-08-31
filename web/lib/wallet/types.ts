import { BrowserProvider, JsonRpcSigner } from 'ethers'
import { config } from '../config'

// EIP-712 Domain for HyperLicked (from config)
export const EIP712_DOMAIN = config.eip712

// EIP-712 Types for Order
export const EIP712_ORDER_TYPES = {
  Order: [
    { name: 'symbol', type: 'string' },
    { name: 'side', type: 'uint8' },
    { name: 'type', type: 'uint8' },
    { name: 'price', type: 'uint256' },
    { name: 'qty', type: 'uint256' },
    { name: 'nonce', type: 'uint256' },
    { name: 'deadline', type: 'uint256' },
    { name: 'leverage', type: 'uint8' },
    { name: 'owner', type: 'address' }
  ]
}

// EIP-712 Types for Agent Delegation
export const EIP712_DELEGATION_TYPES = {
  AgentDelegation: [
    { name: 'wallet', type: 'address' },
    { name: 'agent', type: 'address' },
    { name: 'expiration', type: 'uint256' },
    { name: 'nonce', type: 'uint256' }
  ]
}

// EIP-712 Types for Cancel Order
export const EIP712_CANCEL_TYPES = {
  CancelOrder: [
    { name: 'orderId', type: 'string' },
    { name: 'symbol', type: 'string' },
    { name: 'nonce', type: 'uint256' },
    { name: 'owner', type: 'address' }
  ]
}

// EIP-712 Types for Trigger Order
export const EIP712_TRIGGER_ORDER_TYPES = {
  TriggerOrder: [
    { name: 'symbol', type: 'string' },
    { name: 'triggerType', type: 'uint8' },
    { name: 'triggerPrice', type: 'uint256' },
    { name: 'size', type: 'uint256' },
    { name: 'limitPrice', type: 'uint256' },
    { name: 'nonce', type: 'uint256' },
    { name: 'owner', type: 'address' }
  ]
}

// EIP-712 Types for Cancel Trigger Order
export const EIP712_CANCEL_TRIGGER_ORDER_TYPES = {
  CancelTriggerOrder: [
    { name: 'triggerOrderId', type: 'string' },
    { name: 'symbol', type: 'string' },
    { name: 'nonce', type: 'uint256' },
    { name: 'owner', type: 'address' }
  ]
}

export interface WalletState {
  isConnected: boolean
  address: string | null
  provider: BrowserProvider | null
  signer: JsonRpcSigner | null
  isRabby: boolean
  chainId: number | null
  // Agent key state
  tradingEnabled: boolean
  agentAddress: string | null
  delegationExpiry: string | null
  // Error/warning state
  error: string | null
  needsReconnect: boolean
}

export interface OrderToSign {
  symbol: string
  side: number // 1=Buy, 2=Sell
  type: number // 1=GTC, 2=IOC, 3=ALO
  price: string // BigInt as string
  qty: string // BigInt as string
  nonce: string // BigInt as string
  deadline: string // BigInt as string (0 = no expiry)
  validAfter?: string // BigInt as string (canonical envelope lower bound)
  leverage: number
  owner: string // Address
  reduce_only?: boolean
}

export interface CancelToSign {
  orderId: string
  symbol: string
  nonce: string // BigInt as string
  owner: string // Address
  deadline?: string // BigInt as string (canonical envelope upper bound)
  validAfter?: string // BigInt as string (canonical envelope lower bound)
}

export interface TriggerOrderToSign {
  symbol: string
  triggerType: number   // 1=StopLoss, 2=TakeProfit
  triggerPrice: string  // BigInt as string
  size: string          // BigInt as string
  limitPrice: string    // BigInt as string (0 = no limit)
  nonce: string         // BigInt as string
  owner: string         // Address
  deadline?: string     // BigInt as string (canonical envelope upper bound)
  validAfter?: string   // BigInt as string (canonical envelope lower bound)
  cloid?: string
}

export interface CancelTriggerOrderToSign {
  triggerOrderId?: string
  symbol?: string
  nonce: string // BigInt as string
  owner: string // Address
  deadline?: string // BigInt as string (canonical envelope upper bound)
  validAfter?: string // BigInt as string (canonical envelope lower bound)
  cloid?: string
}

export interface LocalWalletState {
  provider: BrowserProvider | null
  signer: JsonRpcSigner | null
  isRabby: boolean
  chainId: number | null
  error: string | null
  needsReconnect: boolean
}
