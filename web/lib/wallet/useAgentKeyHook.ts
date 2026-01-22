'use client'

import { useState, useEffect, useCallback, useRef } from 'react'
import { BrowserProvider, JsonRpcSigner, Wallet, HDNodeWallet } from 'ethers'
import { useWalletStore } from '../store'
import {
  generateAgentKey,
  storeAgentKey,
  loadAgentKey,
  hasValidAgentKey,
  clearAgentKey,
  getDelegationTimeRemaining,
  getAgentKeyWalletAddress,
  type AgentDelegation
} from '../agentKey'
import { registerDelegation } from '../api'
import { EIP712_DOMAIN, EIP712_DELEGATION_TYPES } from './types'

/**
 * Hook for agent key management (enable/disable trading)
 */
export function useAgentKeyHook(
  signer: JsonRpcSigner | null,
  isConnected: boolean,
  address: string | null
) {
  const { tradingEnabled, agentAddress, delegationExpiry, setTradingEnabled } = useWalletStore()

  const [agentWallet, setAgentWallet] = useState<Wallet | HDNodeWallet | null>(null)
  const agentWalletRef = useRef<Wallet | HDNodeWallet | null>(null)

  // Sync ref with state
  useEffect(() => {
    agentWalletRef.current = agentWallet
  }, [agentWallet])

  // Check for existing agent key on mount and when wallet connects
  useEffect(() => {
    if (!address) return

    const storedWalletAddress = getAgentKeyWalletAddress()

    if (hasValidAgentKey() && storedWalletAddress?.toLowerCase() === address.toLowerCase()) {
      const agent = loadAgentKey()
      if (agent) {
        setAgentWallet(agent)
        setTradingEnabled(true, agent.address, getDelegationTimeRemaining())
      }
    }
  }, [address, setTradingEnabled])

  // Enable trading: create agent key and sign delegation
  const enableTrading = useCallback(async (durationDays: number = 7): Promise<void> => {
    let activeSigner = signer

    // Create signer on-demand if needed
    if (!activeSigner && isConnected) {
      const ethereum = (window as any).ethereum
      if (ethereum) {
        const provider = new BrowserProvider(ethereum)
        activeSigner = await provider.getSigner()
      }
    }

    if (!activeSigner) {
      throw new Error('Wallet not connected')
    }

    // Get address directly from signer to avoid race condition
    const signerAddress = await activeSigner.getAddress()

    // Generate new agent key
    const agent = generateAgentKey()

    // Create delegation
    const expiration = BigInt(Math.floor(Date.now() / 1000) + durationDays * 86400)
    const nonce = BigInt(Date.now())

    const delegation: Omit<AgentDelegation, 'signature'> = {
      wallet: signerAddress,
      agent: agent.address,
      expiration: expiration.toString(),
      nonce: nonce.toString()
    }

    // Sign delegation with MetaMask
    const signature = await activeSigner.signTypedData(
      EIP712_DOMAIN,
      EIP712_DELEGATION_TYPES,
      delegation
    )

    const fullDelegation: AgentDelegation = {
      ...delegation,
      signature
    }

    // Store agent key locally
    storeAgentKey(agent, fullDelegation)

    // Register delegation with backend
    const normalizedAddress = signerAddress.toLowerCase()
    await registerDelegation({
      wallet: normalizedAddress,
      agent: agent.address,
      expiration: expiration.toString(),
      nonce: nonce.toString(),
      signature
    })

    // Update state
    setAgentWallet(agent)
    setTradingEnabled(true, agent.address, getDelegationTimeRemaining())
  }, [signer, isConnected, setTradingEnabled])

  // Disable trading: clear agent key
  const disableTrading = useCallback(() => {
    clearAgentKey()
    setAgentWallet(null)
    setTradingEnabled(false)
  }, [setTradingEnabled])

  return {
    tradingEnabled,
    agentAddress,
    delegationExpiry,
    agentWallet,
    enableTrading,
    disableTrading,
  }
}
