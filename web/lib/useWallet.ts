'use client'

/**
 * Wallet Hook - Composable wallet integration for EIP-712 signing
 *
 * This module is split into three focused hooks:
 * - useWalletConnection: Connection/disconnect, event listeners, network switching
 * - useWalletSigning: EIP-712 signing for orders and cancels
 * - useAgentKeyHook: Agent key management for gasless trading
 *
 * @see lib/wallet/useWalletConnection.ts
 * @see lib/wallet/useWalletSigning.ts
 * @see lib/wallet/useAgentKeyHook.ts
 */

// Re-export the composed hook and types from the wallet module
export { useWallet } from './wallet'
export type { OrderToSign, CancelToSign, WalletState, LocalWalletState } from './wallet'
export {
  EIP712_DOMAIN,
  EIP712_ORDER_TYPES,
  EIP712_CANCEL_TYPES,
  EIP712_DELEGATION_TYPES
} from './wallet'
