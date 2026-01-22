/**
 * Agent Key Management for "Enable Trading" feature
 *
 * Allows users to sign once (delegation) and then trade without MetaMask popups.
 * Agent keys are ephemeral, stored in localStorage, and expire after 7 days.
 *
 * ## Security Considerations
 *
 * **WARNING: Agent keys are stored UNENCRYPTED in localStorage.**
 *
 * This is an intentional trade-off for UX simplicity with the following mitigations:
 *
 * ### Risk Assessment
 * - XSS attacks could steal the agent key private key from localStorage
 * - Stolen keys could be used to place unauthorized trades
 *
 * ### Mitigations
 * 1. **Limited scope**: Agent keys can ONLY sign orders - they cannot:
 *    - Withdraw funds
 *    - Transfer assets
 *    - Modify account settings
 * 2. **Short lifespan**: Keys expire after 7 days (configurable)
 * 3. **Revocable**: Users can revoke at any time via "Disable Trading"
 * 4. **Backend verification**: All orders must have valid delegation on file
 * 5. **Nonce protection**: Each order requires a fresh nonce (prevents replay)
 *
 * ### Production Recommendations
 * For production deployments with higher security requirements, consider:
 * - Encrypting the private key with a user-provided password
 * - Using IndexedDB with encryption (e.g., Web Crypto API)
 * - Implementing session-based storage (cleared on browser close)
 * - Adding IP-based restrictions on the backend
 *
 * @see https://eips.ethereum.org/EIPS/eip-712 for EIP-712 signed delegation
 */

import { Wallet, HDNodeWallet } from 'ethers'

const AGENT_KEY_STORAGE = 'hyperlicked_agent_key'
const AGENT_DELEGATION_STORAGE = 'hyperlicked_agent_delegation'
const EXPLICIT_DISCONNECT_KEY = 'hyperlicked_explicit_disconnect'

/**
 * Mark that user explicitly disconnected (skip auto-connect)
 */
export function setExplicitDisconnect(value: boolean): void {
  if (value) {
    sessionStorage.setItem(EXPLICIT_DISCONNECT_KEY, 'true')
  } else {
    sessionStorage.removeItem(EXPLICIT_DISCONNECT_KEY)
  }
}

/**
 * Check if user explicitly disconnected (should skip auto-connect)
 */
export function wasExplicitlyDisconnected(): boolean {
  return sessionStorage.getItem(EXPLICIT_DISCONNECT_KEY) === 'true'
}

/**
 * Get the wallet address associated with stored agent key
 */
export function getAgentKeyWalletAddress(): string | null {
  try {
    const stored = localStorage.getItem(AGENT_KEY_STORAGE)
    if (!stored) return null
    const data: StoredAgentKey = JSON.parse(stored)
    return data.delegation.wallet
  } catch {
    return null
  }
}

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
      clearAgentKey()
      return null
    }

    // Create wallet from private key
    const wallet = new Wallet(data.privateKey)

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

