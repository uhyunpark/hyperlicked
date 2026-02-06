'use client'

import { useCallback } from 'react'
import { JsonRpcSigner } from 'ethers'
import {
  loadAgentKey,
  getStoredDelegation,
} from '../agentKey'
import {
  EIP712_DOMAIN,
  EIP712_ORDER_TYPES,
  EIP712_CANCEL_TYPES,
  EIP712_TRIGGER_ORDER_TYPES,
  EIP712_CANCEL_TRIGGER_ORDER_TYPES,
  type OrderToSign,
  type CancelToSign,
  type TriggerOrderToSign,
  type CancelTriggerOrderToSign,
} from './types'

interface SigningResult {
  signature: string
  agentMode: boolean
  delegationId?: string
}

/**
 * Hook for EIP-712 signing operations
 */
export function useWalletSigning(signer: JsonRpcSigner | null) {
  // Sign an order using EIP-712 (MetaMask)
  const signOrder = useCallback(async (order: OrderToSign): Promise<string> => {
    if (!signer) {
      throw new Error('Wallet not connected')
    }

    const signature = await signer.signTypedData(
      EIP712_DOMAIN,
      EIP712_ORDER_TYPES,
      order
    )

    return signature
  }, [signer])

  // Sign order with agent key (if available) or MetaMask
  const signOrderSmart = useCallback(async (order: OrderToSign): Promise<SigningResult> => {
    const agent = loadAgentKey()
    const delegation = getStoredDelegation()

    if (agent && delegation) {
      const signature = await agent.signTypedData(
        EIP712_DOMAIN,
        EIP712_ORDER_TYPES,
        order
      )

      return {
        signature,
        agentMode: true,
        delegationId: `${delegation.wallet.toLowerCase()}-${delegation.nonce}`
      }
    }

    const signature = await signOrder(order)
    return {
      signature,
      agentMode: false
    }
  }, [signOrder])

  // Sign cancel order using EIP-712 (MetaMask only)
  const signCancel = useCallback(async (cancel: CancelToSign): Promise<string> => {
    if (!signer) {
      throw new Error('Wallet not connected')
    }

    const signature = await signer.signTypedData(
      EIP712_DOMAIN,
      EIP712_CANCEL_TYPES,
      cancel
    )

    return signature
  }, [signer])

  // Sign cancel with agent key (if available) or MetaMask
  const signCancelSmart = useCallback(async (cancel: CancelToSign): Promise<SigningResult> => {
    const agent = loadAgentKey()
    const delegation = getStoredDelegation()

    if (agent && delegation) {
      const signature = await agent.signTypedData(
        EIP712_DOMAIN,
        EIP712_CANCEL_TYPES,
        cancel
      )

      return {
        signature,
        agentMode: true,
        delegationId: `${delegation.wallet.toLowerCase()}-${delegation.nonce}`
      }
    }

    const signature = await signCancel(cancel)
    return {
      signature,
      agentMode: false
    }
  }, [signCancel])

  // Sign a trigger order using EIP-712 (MetaMask)
  const signTriggerOrder = useCallback(async (trigger: TriggerOrderToSign): Promise<string> => {
    if (!signer) {
      throw new Error('Wallet not connected')
    }

    const signature = await signer.signTypedData(
      EIP712_DOMAIN,
      EIP712_TRIGGER_ORDER_TYPES,
      trigger
    )

    return signature
  }, [signer])

  // Sign trigger order with agent key (if available) or MetaMask
  const signTriggerOrderSmart = useCallback(async (trigger: TriggerOrderToSign): Promise<SigningResult> => {
    const agent = loadAgentKey()
    const delegation = getStoredDelegation()

    if (agent && delegation) {
      const signature = await agent.signTypedData(
        EIP712_DOMAIN,
        EIP712_TRIGGER_ORDER_TYPES,
        trigger
      )

      return {
        signature,
        agentMode: true,
        delegationId: `${delegation.wallet.toLowerCase()}-${delegation.nonce}`
      }
    }

    const signature = await signTriggerOrder(trigger)
    return {
      signature,
      agentMode: false
    }
  }, [signTriggerOrder])

  // Sign cancel trigger order using EIP-712 (MetaMask)
  const signCancelTriggerOrder = useCallback(async (cancel: CancelTriggerOrderToSign): Promise<string> => {
    if (!signer) {
      throw new Error('Wallet not connected')
    }

    const signature = await signer.signTypedData(
      EIP712_DOMAIN,
      EIP712_CANCEL_TRIGGER_ORDER_TYPES,
      cancel
    )

    return signature
  }, [signer])

  // Sign cancel trigger order with agent key (if available) or MetaMask
  const signCancelTriggerOrderSmart = useCallback(async (cancel: CancelTriggerOrderToSign): Promise<SigningResult> => {
    const agent = loadAgentKey()
    const delegation = getStoredDelegation()

    if (agent && delegation) {
      const signature = await agent.signTypedData(
        EIP712_DOMAIN,
        EIP712_CANCEL_TRIGGER_ORDER_TYPES,
        cancel
      )

      return {
        signature,
        agentMode: true,
        delegationId: `${delegation.wallet.toLowerCase()}-${delegation.nonce}`
      }
    }

    const signature = await signCancelTriggerOrder(cancel)
    return {
      signature,
      agentMode: false
    }
  }, [signCancelTriggerOrder])

  return {
    signOrder,
    signOrderSmart,
    signCancel,
    signCancelSmart,
    signTriggerOrder,
    signTriggerOrderSmart,
    signCancelTriggerOrder,
    signCancelTriggerOrderSmart,
  }
}
