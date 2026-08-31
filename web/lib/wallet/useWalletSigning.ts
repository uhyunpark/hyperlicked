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
import {
  normalizeAddress,
  signCanonicalTransaction,
  type CanonicalAction,
} from './canonicalAction'
import { getCanonicalChainDomain } from '../api'

interface SigningResult {
  signature: string
  agentMode: boolean
  delegationId?: string
}

function canonicalSide(side: number): 0 | 1 {
  if (side === 1) return 0 // Side::Bid
  if (side === 2) return 1 // Side::Ask
  throw new Error('side must be 1 (buy) or 2 (sell)')
}

function canonicalOrderType(orderType: number): 0 | 1 | 2 {
  if (orderType === 1) return 0 // OrderType::Gtc
  if (orderType === 2) return 1 // OrderType::Ioc
  if (orderType === 3) return 2 // OrderType::Alo
  throw new Error('order type must be 1 (GTC), 2 (IOC), or 3 (ALO)')
}

function canonicalTriggerType(triggerType: number): 0 | 1 {
  if (triggerType === 1) return 0 // TriggerType::StopLoss
  if (triggerType === 2) return 1 // TriggerType::TakeProfit
  throw new Error('trigger type must be 1 (stop loss) or 2 (take profit)')
}

function canonicalOrderAction(order: OrderToSign): CanonicalAction {
  return {
    type: 'PlaceOrder',
    trader: order.owner,
    symbol: order.symbol,
    side: canonicalSide(order.side),
    price: order.price,
    size: order.qty,
    orderType: canonicalOrderType(order.type),
    reduceOnly: order.reduce_only === true,
  }
}

function canonicalCancelAction(cancel: CancelToSign): CanonicalAction {
  return {
    type: 'CancelOrder',
    trader: cancel.owner,
    orderId: cancel.orderId,
  }
}

function canonicalTriggerAction(trigger: TriggerOrderToSign): CanonicalAction {
  return {
    type: 'PlaceTriggerOrder',
    trader: trigger.owner,
    symbol: trigger.symbol,
    triggerType: canonicalTriggerType(trigger.triggerType),
    triggerPrice: trigger.triggerPrice,
    size: trigger.size,
    limitPrice: trigger.limitPrice === '0' ? null : trigger.limitPrice,
    cloid: trigger.cloid,
  }
}

function canonicalCancelTriggerAction(cancel: CancelTriggerOrderToSign): CanonicalAction {
  if (cancel.triggerOrderId !== undefined) {
    return {
      type: 'CancelTriggerOrder',
      trader: cancel.owner,
      triggerOrderId: cancel.triggerOrderId,
    }
  }
  if (cancel.symbol !== undefined && cancel.cloid !== undefined) {
    return {
      type: 'CancelTriggerOrderByCloid',
      trader: cancel.owner,
      symbol: cancel.symbol,
      cloid: cancel.cloid,
    }
  }
  throw new Error('trigger cancellation needs triggerOrderId or symbol + cloid')
}

async function signCanonicalAction(
  signer: JsonRpcSigner | null,
  owner: string,
  nonce: string,
  validAfter: string | undefined,
  deadline: string | undefined,
  action: CanonicalAction,
): Promise<string> {
  if (!signer) throw new Error('Wallet not connected')
  if (!deadline) throw new Error('Canonical envelope signing requires a deadline')

  const signerAddress = normalizeAddress(owner)
  const connectedAddress = normalizeAddress(await signer.getAddress())
  if (connectedAddress !== signerAddress) {
    throw new Error('Connected wallet changed; reconnect before signing')
  }

  const chainDomain = await getCanonicalChainDomain()
  return signCanonicalTransaction(signer, {
    chainDomain,
    signer: signerAddress,
    nonce,
    validAfter: validAfter ?? '0',
    validUntil: deadline,
    action,
  })
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

  // Canonical envelope signing always uses the connected wallet.  The agent
  // key path above signs a legacy typed message and cannot satisfy the Rust
  // envelope's signer == action.trader invariant.
  const signCanonicalOrder = useCallback(async (order: OrderToSign): Promise<string> => {
    return signCanonicalAction(
      signer,
      order.owner,
      order.nonce,
      order.validAfter,
      order.deadline,
      canonicalOrderAction(order),
    )
  }, [signer])

  const signCanonicalCancel = useCallback(async (cancel: CancelToSign): Promise<string> => {
    return signCanonicalAction(
      signer,
      cancel.owner,
      cancel.nonce,
      cancel.validAfter,
      cancel.deadline,
      canonicalCancelAction(cancel),
    )
  }, [signer])

  const signCanonicalTriggerOrder = useCallback(async (trigger: TriggerOrderToSign): Promise<string> => {
    return signCanonicalAction(
      signer,
      trigger.owner,
      trigger.nonce,
      trigger.validAfter,
      trigger.deadline,
      canonicalTriggerAction(trigger),
    )
  }, [signer])

  const signCanonicalCancelTriggerOrder = useCallback(async (cancel: CancelTriggerOrderToSign): Promise<string> => {
    return signCanonicalAction(
      signer,
      cancel.owner,
      cancel.nonce,
      cancel.validAfter,
      cancel.deadline,
      canonicalCancelTriggerAction(cancel),
    )
  }, [signer])

  return {
    signOrder,
    signOrderSmart,
    signCancel,
    signCancelSmart,
    signTriggerOrder,
    signTriggerOrderSmart,
    signCancelTriggerOrder,
    signCancelTriggerOrderSmart,
    signCanonicalOrder,
    signCanonicalCancel,
    signCanonicalTriggerOrder,
    signCanonicalCancelTriggerOrder,
  }
}
