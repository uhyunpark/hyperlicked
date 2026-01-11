'use client'

import { useState, useEffect, useCallback, useRef } from 'react'
import { BrowserProvider, JsonRpcSigner, Wallet, HDNodeWallet, hashMessage } from 'ethers'
import { config } from './config'
import { useWalletStore } from './store'
import {
  generateAgentKey,
  storeAgentKey,
  loadAgentKey,
  getStoredDelegation,
  hasValidAgentKey,
  clearAgentKey,
  getDelegationTimeRemaining,
  setExplicitDisconnect,
  wasExplicitlyDisconnected,
  getAgentKeyWalletAddress,
  type AgentDelegation
} from './agentKey'

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

export interface WalletState {
  isConnected: boolean
  address: string | null
  provider: BrowserProvider | null
  signer: JsonRpcSigner | null
  isRabby: boolean
  chainId: number | null
  // Agent key state
  tradingEnabled: boolean // True if agent key is active
  agentAddress: string | null // Agent key address (if enabled)
  delegationExpiry: string | null // Time remaining (e.g., "6d 12h")
  // Error/warning state
  error: string | null // Current error message
  needsReconnect: boolean // True if wallet changed and needs reconnection
}

export interface OrderToSign {
  symbol: string
  side: number // 1=Buy, 2=Sell
  type: number // 1=GTC, 2=IOC, 3=ALO
  price: string // BigInt as string
  qty: string // BigInt as string
  nonce: string // BigInt as string
  deadline: string // BigInt as string (0 = no expiry)
  leverage: number
  owner: string // Address
  reduce_only?: boolean // Only reduce existing position
}

export interface CancelToSign {
  orderId: string
  symbol: string
  nonce: string // BigInt as string
  owner: string // Address
}

export function useWallet() {
  // Shared state from Zustand store (syncs across all components)
  const {
    isConnected,
    address,
    tradingEnabled,
    agentAddress,
    delegationExpiry,
    setTradingEnabled
  } = useWalletStore()

  // Local state for non-serializable objects (provider, signer)
  const [wallet, setWallet] = useState<{
    provider: BrowserProvider | null
    signer: JsonRpcSigner | null
    isRabby: boolean
    chainId: number | null
    error: string | null
    needsReconnect: boolean
  }>({
    provider: null,
    signer: null,
    isRabby: false,
    chainId: null,
    error: null,
    needsReconnect: false
  })

  const [agentWallet, setAgentWallet] = useState<Wallet | HDNodeWallet | null>(null)

  // CRITICAL FIX: Use ref to avoid stale closure in signOrderSmart
  const agentWalletRef = useRef<Wallet | HDNodeWallet | null>(null)

  // Track if we've attempted auto-connect this session (prevents race condition)
  const hasAutoConnectAttempted = useRef(false)

  // Sync ref with state
  useEffect(() => {
    agentWalletRef.current = agentWallet
  }, [agentWallet])

  // Detect if Rabby Wallet is installed
  const detectRabby = useCallback(() => {
    if (typeof window === 'undefined') return false

    // Rabby injects window.ethereum with a special flag
    const ethereum = (window as any).ethereum
    if (!ethereum) return false

    // Rabby sets isRabby = true
    return ethereum.isRabby === true
  }, [])

  // Connect to wallet (supports both Rabby and MetaMask)
  const connect = useCallback(async () => {
    try {
      if (typeof window === 'undefined') {
        throw new Error('Window is undefined - not running in browser')
      }

      const ethereum = (window as any).ethereum
      if (!ethereum) {
        throw new Error('No Ethereum wallet detected. Please install Rabby Wallet or MetaMask.')
      }

      // Clear explicit disconnect flag since user is intentionally connecting
      setExplicitDisconnect(false)

      // Request permissions - this forces wallet to show account selection popup (EIP-2255)
      // Unlike eth_requestAccounts which silently returns if already authorized
      await ethereum.request({
        method: 'wallet_requestPermissions',
        params: [{ eth_accounts: {} }]
      })

      // Now get the accounts after permission is granted
      const accounts = await ethereum.request({ method: 'eth_accounts' })
      if (!accounts || accounts.length === 0) {
        throw new Error('No accounts found')
      }

      // Create ethers provider and signer
      const provider = new BrowserProvider(ethereum)
      const signer = await provider.getSigner()
      const connectedAddress = await signer.getAddress()
      const network = await provider.getNetwork()
      const chainId = Number(network.chainId)

      const isRabby = detectRabby()

      // Update local state (provider, signer, etc - non-serializable)
      setWallet({
        provider,
        signer,
        isRabby,
        chainId,
        error: null,
        needsReconnect: false
      })

      // Sync to Zustand store (isConnected, address - serializable, shared)
      useWalletStore.getState().setConnected(connectedAddress, chainId, isRabby)

      return { address: connectedAddress, isRabby, chainId }
    } catch (error: any) {
      console.error('[wallet] Connection failed:', error)
      throw error
    }
  }, [detectRabby])

  // Disconnect wallet
  const disconnect = useCallback(async (reason?: string) => {

    // Mark explicit disconnect so we don't auto-connect on page load
    // Only set if this is user-initiated (no error reason)
    if (!reason) {
      setExplicitDisconnect(true)
    }

    // Revoke wallet permissions so next connect shows popup (EIP-2255)
    try {
      const ethereum = (window as any).ethereum
      if (ethereum) {
        await ethereum.request({
          method: 'wallet_revokePermissions',
          params: [{ eth_accounts: {} }]
        })
      }
    } catch (e) {
      // Silent fail - not all wallets support this
    }

    // DON'T clear agent key - keep it for reconnection
    // Just clear the in-memory wallet reference
    setAgentWallet(null)
    setTradingEnabled(false) // Clear shared trading state (will be restored on reconnect)

    // Clear local state (provider, signer, etc)
    setWallet({
      provider: null,
      signer: null,
      isRabby: false,
      chainId: null,
      error: reason || null,
      needsReconnect: !!reason
    })

    // Sync to Zustand store (isConnected, address)
    useWalletStore.getState().setDisconnected()
  }, [setTradingEnabled])

  // Clear error
  const clearError = useCallback(() => {
    setWallet(prev => ({ ...prev, error: null, needsReconnect: false }))
  }, [])

  // Sign an order using EIP-712
  const signOrder = useCallback(async (order: OrderToSign): Promise<string> => {
    if (!wallet.signer) {
      throw new Error('Wallet not connected')
    }

    try {

      // Sign typed data (EIP-712)
      const signature = await wallet.signer.signTypedData(
        EIP712_DOMAIN,
        EIP712_ORDER_TYPES,
        order
      )

      return signature
    } catch (error: any) {
      console.error('[wallet] Signing failed:', error)
      throw error
    }
  }, [wallet.signer])

  // Switch to correct network (if needed)
  const switchNetwork = useCallback(async (targetChainId: number) => {
    try {
      const ethereum = (window as any).ethereum
      if (!ethereum) {
        throw new Error('No wallet detected')
      }

      await ethereum.request({
        method: 'wallet_switchEthereumChain',
        params: [{ chainId: `0x${targetChainId.toString(16)}` }]
      })

    } catch (error: any) {
      // Chain doesn't exist, add it
      if (error.code === 4902) {
        await addNetwork(targetChainId)
      } else {
        throw error
      }
    }
  }, [])

  // Add custom network (for local devnet)
  const addNetwork = useCallback(async (chainId: number) => {
    try {
      const ethereum = (window as any).ethereum
      if (!ethereum) {
        throw new Error('No wallet detected')
      }

      await ethereum.request({
        method: 'wallet_addEthereumChain',
        params: [{
          chainId: `0x${chainId.toString(16)}`,
          chainName: config.network.chainName,
          nativeCurrency: {
            name: config.currency.name,
            symbol: config.currency.symbol,
            decimals: config.currency.decimals
          },
          rpcUrls: [config.network.rpcUrl],
          blockExplorerUrls: config.network.blockExplorerUrl ? [config.network.blockExplorerUrl] : null
        }]
      })

    } catch (error: any) {
      console.error('[wallet] Failed to add network:', error)
      throw error
    }
  }, [])

  // Track previous address to detect actual changes
  const prevAddressRef = useRef<string | null>(null)

  // Sync prev address ref (use address from store)
  useEffect(() => {
    prevAddressRef.current = address
  }, [address])

  // Listen for account and chain changes
  useEffect(() => {
    if (typeof window === 'undefined') return

    const ethereum = (window as any).ethereum
    if (!ethereum) return

    const handleAccountsChanged = (accounts: string[]) => {

      if (accounts.length === 0) {
        // User disconnected wallet
        disconnect('Wallet disconnected')
        return
      }

      const newAddress = accounts[0].toLowerCase()
      const prevAddress = prevAddressRef.current?.toLowerCase()

      // Only disconnect if address actually changed (not just reconnecting)
      if (prevAddress && prevAddress !== newAddress) {
        disconnect('Wallet address changed. Please reconnect.')
      } else if (!prevAddress) {
        // No previous address - this is initial connection or after page load
        // Let auto-connect handle it
      }
    }

    const handleChainChanged = (chainIdHex: string) => {
      const newChainId = parseInt(chainIdHex, 16)

      // Check if connected (from store)
      const storeState = useWalletStore.getState()
      if (!storeState.isConnected) return

      // Check if this is the expected chain
      const expectedChainId = config.network.chainId
      if (newChainId !== expectedChainId) {
        setWallet(prev => ({
          ...prev,
          chainId: newChainId,
          error: `Wrong network. Please switch to ${config.network.chainName} (Chain ID: ${expectedChainId})`,
          needsReconnect: false
        }))
      } else {
        // Correct chain - clear any network errors
        setWallet(prev => ({
          ...prev,
          chainId: newChainId,
          error: null,
          needsReconnect: false
        }))
      }

      // Update chainId in store
      useWalletStore.getState().setChainId(newChainId)
    }

    ethereum.on('accountsChanged', handleAccountsChanged)
    ethereum.on('chainChanged', handleChainChanged)

    return () => {
      ethereum.removeListener('accountsChanged', handleAccountsChanged)
      ethereum.removeListener('chainChanged', handleChainChanged)
    }
  }, [disconnect])

  // Auto-connect if previously connected (unless user explicitly disconnected)
  // NOTE: Empty dependency array - only runs ONCE on mount to prevent race conditions
  useEffect(() => {
    // Only attempt auto-connect once per mount
    if (hasAutoConnectAttempted.current) return
    hasAutoConnectAttempted.current = true

    if (typeof window === 'undefined') return

    const ethereum = (window as any).ethereum
    if (!ethereum) return

    // Skip auto-connect if user explicitly disconnected
    if (wasExplicitlyDisconnected()) {
      return
    }

    // Check if already connected - use eth_accounts (silent) not eth_requestAccounts (popup)
    ethereum.request({ method: 'eth_accounts' })
      .then(async (accounts: string[]) => {
        if (accounts.length > 0) {
          try {
            const provider = new BrowserProvider(ethereum)
            const signer = await provider.getSigner()
            const connectedAddress = await signer.getAddress()
            const network = await provider.getNetwork()
            const chainId = Number(network.chainId)
            const isRabby = ethereum.isRabby === true


            // Update local state (non-serializable)
            setWallet({
              provider,
              signer,
              isRabby,
              chainId,
              error: null,
              needsReconnect: false
            })

            // Sync to Zustand store (serializable, shared)
            useWalletStore.getState().setConnected(connectedAddress, chainId, isRabby)
          } catch (error) {
            console.error('[wallet] Auto-connect setup failed:', error)
          }
        }
      })
      .catch((error: any) => {
        console.error('[wallet] Auto-connect failed:', error)
      })
  }, []) // Empty deps - runs once on mount

  // Check for existing agent key on mount and when wallet connects
  useEffect(() => {
    if (!address) return

    // Check if there's a valid agent key for THIS wallet address
    const storedWalletAddress = getAgentKeyWalletAddress()

    if (hasValidAgentKey() && storedWalletAddress?.toLowerCase() === address.toLowerCase()) {
      const agent = loadAgentKey()

      if (agent) {
        setAgentWallet(agent)
        setTradingEnabled(true, agent.address, getDelegationTimeRemaining())
      }
    } else if (storedWalletAddress && storedWalletAddress.toLowerCase() !== address.toLowerCase()) {
      // Different wallet connected - clear the old agent key to prevent stale state
      clearAgentKey()
    }
  }, [address, setTradingEnabled])

  // Enable trading: create agent key and sign delegation
  const enableTrading = useCallback(async (durationDays: number = 7): Promise<void> => {
    // If no local signer but Zustand says connected, create signer on-demand
    // This handles the case where a different component (e.g., Header) called connect()
    // but this component's local signer state is still null
    let signer = wallet.signer
    if (!signer && isConnected) {
      const ethereum = (window as any).ethereum
      if (ethereum) {
        const provider = new BrowserProvider(ethereum)
        signer = await provider.getSigner()
      }
    }

    if (!signer) {
      throw new Error('Wallet not connected')
    }

    try {
      // IMPORTANT: Get address directly from signer to avoid race condition
      // The `address` from Zustand store might be stale if user just switched wallets
      const signerAddress = await signer.getAddress()

      // Generate new agent key
      const agent = generateAgentKey()

      // Create delegation
      const expiration = BigInt(Math.floor(Date.now() / 1000) + durationDays * 86400)
      const nonce = BigInt(Date.now()) // Simple nonce (timestamp)

      const delegation: Omit<AgentDelegation, 'signature'> = {
        wallet: signerAddress,
        agent: agent.address,
        expiration: expiration.toString(),
        nonce: nonce.toString()
      }


      // Sign delegation with MetaMask (ONE-TIME signature)
      const signature = await signer.signTypedData(
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
      const { registerDelegation } = await import('@/lib/api')

      const delegationId = `${signerAddress}-${nonce.toString()}`
      const response = await registerDelegation({
        wallet: signerAddress,
        agent: agent.address,
        expiration: expiration.toString(),
        nonce: nonce.toString(),
        signature
      })


      // Update state
      setAgentWallet(agent)
      setTradingEnabled(true, agent.address, getDelegationTimeRemaining())

    } catch (error: any) {
      console.error('[wallet] Enable trading failed:', error)
      throw error
    }
  }, [wallet.signer, isConnected, setTradingEnabled])

  // Disable trading: clear agent key
  const disableTrading = useCallback(() => {
    clearAgentKey()
    setAgentWallet(null)
    setTradingEnabled(false)
  }, [setTradingEnabled])

  // Sign order with agent key (if enabled) or MetaMask (if not)
  const signOrderSmart = useCallback(async (order: OrderToSign): Promise<{ signature: string; agentMode: boolean; delegationId?: string }> => {
    // Use ref to get latest agent wallet (avoids stale closure)
    const currentAgentWallet = agentWalletRef.current


    // If trading enabled, use agent key
    if (tradingEnabled && currentAgentWallet) {

      const orderMessage = JSON.stringify(order)
      const signature = await currentAgentWallet.signMessage(orderMessage)

      const delegation = getStoredDelegation()!

      return {
        signature,
        agentMode: true,
        delegationId: `${delegation.wallet}-${delegation.nonce}`
      }
    }

    // Otherwise, use MetaMask
    const signature = await signOrder(order)
    return {
      signature,
      agentMode: false
    }
  }, [tradingEnabled, signOrder])

  // Sign cancel order using EIP-712 (MetaMask only, no agent key)
  const signCancel = useCallback(async (cancel: CancelToSign): Promise<string> => {
    if (!wallet.signer) {
      throw new Error('Wallet not connected')
    }

    try {

      // Sign typed data (EIP-712)
      const signature = await wallet.signer.signTypedData(
        EIP712_DOMAIN,
        EIP712_CANCEL_TYPES,
        cancel
      )

      return signature
    } catch (error: any) {
      console.error('[wallet] Cancel signing failed:', error)
      throw error
    }
  }, [wallet.signer])

  // Sign cancel order with agent key (if enabled) or MetaMask (if not)
  const signCancelSmart = useCallback(async (cancel: CancelToSign): Promise<{ signature: string; agentMode: boolean; delegationId?: string }> => {
    // Use ref to get latest agent wallet (avoids stale closure)
    const currentAgentWallet = agentWalletRef.current

    // If trading enabled, use agent key
    if (tradingEnabled && currentAgentWallet) {

      const cancelMessage = JSON.stringify(cancel)
      const signature = await currentAgentWallet.signMessage(cancelMessage)

      const delegation = getStoredDelegation()!

      return {
        signature,
        agentMode: true,
        delegationId: `${delegation.wallet}-${delegation.nonce}`
      }
    }

    // Otherwise, use MetaMask
    const signature = await signCancel(cancel)
    return {
      signature,
      agentMode: false
    }
  }, [tradingEnabled, signCancel])

  return {
    // Connection state from Zustand store (shared across all components)
    isConnected,
    address,
    // Local state (provider, signer - not serializable)
    ...wallet,
    // Trading state from shared store (syncs across components)
    tradingEnabled,
    agentAddress,
    delegationExpiry,
    // Methods
    connect,
    disconnect,
    signOrder,
    signCancel,
    switchNetwork,
    clearError,
    // Agent key methods
    enableTrading,
    disableTrading,
    signOrderSmart,
    signCancelSmart,
    agentWallet
  }
}
