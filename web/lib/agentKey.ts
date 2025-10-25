/**
 * Agent Key Management for "Enable Trading" feature
 *
 * Allows users to sign once (delegation) and then trade without MetaMask popups.
 * Agent keys are ephemeral, stored in localStorage, and expire after 7 days.
 */

import { Wallet, HDNodeWallet } from 'ethers'

const AGENT_KEY_STORAGE = 'hyperlicked_agent_key'
const AGENT_DELEGATION_STORAGE = 'hyperlicked_agent_delegation'

export interface AgentDelegation {
  wallet: string // Main wallet address
  agent: string // Agent key address
  expiration: string // Unix timestamp (BigInt as string)
  nonce: string // Nonce (BigInt as string)
  signature: string // MetaMask signature on delegation
}

export interface StoredAgentKey {
  privateKey: string // Private key (hex string) - stored directly for simplicity
  delegation: AgentDelegation
  createdAt: number // Unix milliseconds
}

/**
 * Generate a new random agent key pair
 */
export function generateAgentKey(): HDNodeWallet {
  return Wallet.createRandom()
}

/**
 * Store agent key in localStorage
 * NOTE: Private key stored unencrypted for simplicity. In production, consider encryption.
 * Agent keys are ephemeral (7 days) and can only place orders, not withdraw funds.
 */
export function storeAgentKey(
  agentWallet: HDNodeWallet,
  delegation: AgentDelegation
): void {
  const stored: StoredAgentKey = {
    privateKey: agentWallet.privateKey,
    delegation,
    createdAt: Date.now()
  }

  localStorage.setItem(AGENT_KEY_STORAGE, JSON.stringify(stored))
  console.log('[agentKey] Stored agent key')
}

/**
 * Load agent key from localStorage
 */
export function loadAgentKey(): Wallet | null {
  try {
    const stored = localStorage.getItem(AGENT_KEY_STORAGE)
    if (!stored) return null

    const data: StoredAgentKey = JSON.parse(stored)

    // Check if delegation expired
    const expiration = BigInt(data.delegation.expiration)
    const now = BigInt(Math.floor(Date.now() / 1000))

    if (now > expiration) {
      console.log('[agentKey] Delegation expired, removing')
      clearAgentKey()
      return null
    }

    // Create wallet from private key
    const wallet = new Wallet(data.privateKey)
    console.log('[agentKey] Loaded agent key:', wallet.address)

    return wallet
  } catch (error) {
    console.error('[agentKey] Failed to load agent key:', error)
    return null
  }
}

/**
 * Get stored delegation (without decrypting private key)
 */
export function getStoredDelegation(): AgentDelegation | null {
  try {
    const stored = localStorage.getItem(AGENT_KEY_STORAGE)
    if (!stored) return null

    const data: StoredAgentKey = JSON.parse(stored)

    // Check if expired
    const expiration = BigInt(data.delegation.expiration)
    const now = BigInt(Math.floor(Date.now() / 1000))

    if (now > expiration) {
      clearAgentKey()
      return null
    }

    return data.delegation
  } catch (error) {
    console.error('[agentKey] Failed to get delegation:', error)
    return null
  }
}

/**
 * Check if agent key is available and not expired
 */
export function hasValidAgentKey(): boolean {
  const delegation = getStoredDelegation()
  return delegation !== null
}

/**
 * Clear stored agent key (revoke trading)
 */
export function clearAgentKey(): void {
  localStorage.removeItem(AGENT_KEY_STORAGE)
  console.log('[agentKey] Cleared agent key')
}

/**
 * Get time remaining until delegation expires
 */
export function getDelegationTimeRemaining(): string | null {
  const delegation = getStoredDelegation()
  if (!delegation) return null

  const expiration = BigInt(delegation.expiration)
  const now = BigInt(Math.floor(Date.now() / 1000))
  const remaining = Number(expiration - now)

  if (remaining <= 0) return null

  const days = Math.floor(remaining / 86400)
  const hours = Math.floor((remaining % 86400) / 3600)
  const minutes = Math.floor((remaining % 3600) / 60)

  if (days > 0) return `${days}d ${hours}h`
  if (hours > 0) return `${hours}h ${minutes}m`
  return `${minutes}m`
}

