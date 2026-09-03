'use client'

import { useWalletConnection } from './useWalletConnection'
import { useWalletSigning } from './useWalletSigning'
import { useAgentKeyHook } from './useAgentKeyHook'

// Re-export types for convenience
export * from './types'

/**
 * Main wallet hook that composes connection, signing, and agent key functionality
 */
export function useWallet() {
  const connection = useWalletConnection()
  const signing = useWalletSigning(connection.signer)
  const agentKey = useAgentKeyHook(
    connection.signer,
    connection.isConnected,
    connection.address
  )

  return {
    // Connection state
    isConnected: connection.isConnected,
    address: connection.address,
    provider: connection.provider,
    signer: connection.signer,
    isRabby: connection.isRabby,
    chainId: connection.chainId,
    error: connection.error,
    needsReconnect: connection.needsReconnect,
    // Connection methods
    connect: connection.connect,
    disconnect: connection.disconnect,
    clearError: connection.clearError,
    switchNetwork: connection.switchNetwork,
    // Signing methods
    signOrder: signing.signOrder,
    signOrderSmart: signing.signOrderSmart,
    signCancel: signing.signCancel,
    signCancelSmart: signing.signCancelSmart,
    signTriggerOrder: signing.signTriggerOrder,
    signTriggerOrderSmart: signing.signTriggerOrderSmart,
    signCancelTriggerOrder: signing.signCancelTriggerOrder,
    signCancelTriggerOrderSmart: signing.signCancelTriggerOrderSmart,
    signCanonicalOrder: signing.signCanonicalOrder,
    signCanonicalCancel: signing.signCanonicalCancel,
    signCanonicalTriggerOrder: signing.signCanonicalTriggerOrder,
    signCanonicalCancelTriggerOrder: signing.signCanonicalCancelTriggerOrder,
    // Agent key state
    tradingEnabled: agentKey.tradingEnabled,
    agentAddress: agentKey.agentAddress,
    delegationExpiry: agentKey.delegationExpiry,
    agentWallet: agentKey.agentWallet,
    // Agent key methods
    enableTrading: agentKey.enableTrading,
    disableTrading: agentKey.disableTrading,
  }
}
