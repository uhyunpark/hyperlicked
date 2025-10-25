'use client'

import { useState, useEffect, useCallback, useRef } from 'react'
import { BrowserProvider, JsonRpcSigner, Wallet, HDNodeWallet, hashMessage } from 'ethers'
import { config } from './config'
import {
  generateAgentKey,
  storeAgentKey,
  loadAgentKey,
  getStoredDelegation,
  hasValidAgentKey,
  clearAgentKey,
  getDelegationTimeRemaining,
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
}

export interface CancelToSign {
  orderId: string
  symbol: string
  nonce: string // BigInt as string
  owner: string // Address
}

export function useWallet() {
  const [wallet, setWallet] = useState<WalletState>({
    isConnected: false,
    address: null,
    provider: null,
    signer: null,
    isRabby: false,
    chainId: null,
    tradingEnabled: false,
    agentAddress: null,
    delegationExpiry: null
  })

  const [agentWallet, setAgentWallet] = useState<Wallet | HDNodeWallet | null>(null)

  // CRITICAL FIX: Use ref to avoid stale closure in signOrderSmart
  const agentWalletRef = useRef<Wallet | HDNodeWallet | null>(null)

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

      console.log('[wallet] Requesting account access...')

      // Request account access
      const accounts = await ethereum.request({ method: 'eth_requestAccounts' })
      if (!accounts || accounts.length === 0) {
        throw new Error('No accounts found')
      }

      // Create ethers provider and signer
      const provider = new BrowserProvider(ethereum)
      const signer = await provider.getSigner()
      const address = await signer.getAddress()
      const network = await provider.getNetwork()
      const chainId = Number(network.chainId)

      const isRabby = detectRabby()
      console.log(`[wallet] Connected! Address: ${address}, Wallet: ${isRabby ? 'Rabby' : 'MetaMask/Other'}, Chain ID: ${chainId}`)

      setWallet({
        isConnected: true,
        address,
        provider,
        signer,
        isRabby,
        chainId,
        tradingEnabled: false,
        agentAddress: null,
        delegationExpiry: null
      })

      return { address, isRabby, chainId }
    } catch (error: any) {
      console.error('[wallet] Connection failed:', error)
      throw error
    }
  }, [detectRabby])

  // Disconnect wallet
  const disconnect = useCallback(() => {
    console.log('[wallet] Disconnected')
    // Also clear agent key on disconnect
    clearAgentKey()
    setAgentWallet(null)
    setWallet({
      isConnected: false,
      address: null,
      provider: null,
      signer: null,
      isRabby: false,
      chainId: null,
      tradingEnabled: false,
      agentAddress: null,
      delegationExpiry: null
    })
  }, [])

  // Sign an order using EIP-712
  const signOrder = useCallback(async (order: OrderToSign): Promise<string> => {
    if (!wallet.signer) {
      throw new Error('Wallet not connected')
    }

    try {
      console.log('[wallet] Signing order with EIP-712...', order)

      // Sign typed data (EIP-712)
      const signature = await wallet.signer.signTypedData(
        EIP712_DOMAIN,
        EIP712_ORDER_TYPES,
        order
      )

      console.log('[wallet] Order signed successfully!')
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

      console.log(`[wallet] Switched to chain ${targetChainId}`)
    } catch (error: any) {
      // Chain doesn't exist, add it
      if (error.code === 4902) {
        console.log('[wallet] Network not found, adding...')
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

      console.log(`[wallet] Added network ${chainId} (${config.network.chainName}) with RPC ${config.network.rpcUrl}`)
    } catch (error: any) {
      console.error('[wallet] Failed to add network:', error)
      throw error
    }
  }, [])

  // Listen for account changes
  useEffect(() => {
    if (typeof window === 'undefined') return

    const ethereum = (window as any).ethereum
    if (!ethereum) return

    const handleAccountsChanged = (accounts: string[]) => {
      console.log('[wallet] Accounts changed:', accounts)

      // SECURITY: Always disconnect when wallet changes
      // New wallet must explicitly connect and sign delegation
      disconnect()

      if (accounts.length > 0) {
        // Show notification to user
        alert('Wallet changed. Please reconnect to continue trading.')
      }
    }

    const handleChainChanged = (chainId: string) => {
      console.log('[wallet] Chain changed:', chainId)
      // Reload page on chain change (recommended by MetaMask/Rabby)
      window.location.reload()
    }

    ethereum.on('accountsChanged', handleAccountsChanged)
    ethereum.on('chainChanged', handleChainChanged)

    return () => {
      ethereum.removeListener('accountsChanged', handleAccountsChanged)
      ethereum.removeListener('chainChanged', handleChainChanged)
    }
  }, [connect, disconnect])

  // Auto-connect if previously connected
  useEffect(() => {
    if (typeof window === 'undefined') return

    const ethereum = (window as any).ethereum
    if (!ethereum) return

    // Check if already connected
    ethereum.request({ method: 'eth_accounts' })
      .then((accounts: string[]) => {
        if (accounts.length > 0) {
          console.log('[wallet] Auto-connecting to previous session...')
          connect()
        }
      })
      .catch((error: any) => {
        console.error('[wallet] Auto-connect failed:', error)
      })
  }, [connect])

  // Check for existing agent key on mount and when wallet connects
  useEffect(() => {
    if (!wallet.address) return

    if (hasValidAgentKey()) {
      const agent = loadAgentKey()

      if (agent) {
        setAgentWallet(agent)
        setWallet(prev => ({
          ...prev,
          tradingEnabled: true,
          agentAddress: agent.address,
          delegationExpiry: getDelegationTimeRemaining()
        }))
        console.log('[wallet] Loaded existing agent key:', agent.address)
      }
    }
  }, [wallet.address])

  // Enable trading: create agent key and sign delegation
  const enableTrading = useCallback(async (durationDays: number = 7): Promise<void> => {
    if (!wallet.signer || !wallet.address) {
      throw new Error('Wallet not connected')
    }

    try {
      console.log(`[wallet] Enabling trading for ${durationDays} days...`)

      // Generate new agent key
      const agent = generateAgentKey()
      console.log('[wallet] Generated agent key:', agent.address)

      // Create delegation
      const expiration = BigInt(Math.floor(Date.now() / 1000) + durationDays * 86400)
      const nonce = BigInt(Date.now()) // Simple nonce (timestamp)

      const delegation: Omit<AgentDelegation, 'signature'> = {
        wallet: wallet.address,
        agent: agent.address,
        expiration: expiration.toString(),
        nonce: nonce.toString()
      }

      console.log('[wallet] Requesting MetaMask signature for delegation...')

      // Sign delegation with MetaMask (ONE-TIME signature)
      const signature = await wallet.signer.signTypedData(
        EIP712_DOMAIN,
        EIP712_DELEGATION_TYPES,
        delegation
      )

      console.log('[wallet] Delegation signed!')

      const fullDelegation: AgentDelegation = {
        ...delegation,
        signature
      }

      // Store agent key locally
      storeAgentKey(agent, fullDelegation)

      // Register delegation with backend
      console.log('[wallet] Registering delegation with backend...')
      const { registerDelegation } = await import('@/lib/api')

      const delegationId = `${wallet.address}-${nonce.toString()}`
      const response = await registerDelegation({
        wallet: wallet.address,
        agent: agent.address,
        expiration: expiration.toString(),
        nonce: nonce.toString(),
        signature
      })

      console.log('[wallet] Backend registration successful:', response.delegationId)

      // Update state
      setAgentWallet(agent)
      setWallet(prev => ({
        ...prev,
        tradingEnabled: true,
        agentAddress: agent.address,
        delegationExpiry: getDelegationTimeRemaining()
      }))

      console.log('[wallet] Trading enabled! Agent key stored and delegation registered.')
    } catch (error: any) {
      console.error('[wallet] Enable trading failed:', error)
      throw error
    }
  }, [wallet.signer, wallet.address, setAgentWallet, setWallet])

  // Disable trading: clear agent key
  const disableTrading = useCallback(() => {
    clearAgentKey()
    setAgentWallet(null)
    setWallet(prev => ({
      ...prev,
      tradingEnabled: false,
      agentAddress: null,
      delegationExpiry: null
    }))
    console.log('[wallet] Trading disabled')
  }, [])

  // Sign order with agent key (if enabled) or MetaMask (if not)
  const signOrderSmart = useCallback(async (order: OrderToSign): Promise<{ signature: string; agentMode: boolean; delegationId?: string }> => {
    // Use ref to get latest agent wallet (avoids stale closure)
    const currentAgentWallet = agentWalletRef.current

    // Debug: Log current state
    console.log('[wallet] signOrderSmart called:', {
      tradingEnabled: wallet.tradingEnabled,
      agentWalletExists: !!currentAgentWallet,
      agentAddress: currentAgentWallet?.address
    })

    // If trading enabled, use agent key
    if (wallet.tradingEnabled && currentAgentWallet) {
      console.log('[wallet] Signing order with agent key (no MetaMask popup!)')

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
    console.log('[wallet] Signing order with MetaMask (popup required)')
    const signature = await signOrder(order)
    return {
      signature,
      agentMode: false
    }
  }, [wallet.tradingEnabled, signOrder])

  // Sign cancel order using EIP-712 (MetaMask only, no agent key)
  const signCancel = useCallback(async (cancel: CancelToSign): Promise<string> => {
    if (!wallet.signer) {
      throw new Error('Wallet not connected')
    }

    try {
      console.log('[wallet] Signing cancel order with EIP-712...', cancel)

      // Sign typed data (EIP-712)
      const signature = await wallet.signer.signTypedData(
        EIP712_DOMAIN,
        EIP712_CANCEL_TYPES,
        cancel
      )

      console.log('[wallet] Cancel order signed successfully!')
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
    if (wallet.tradingEnabled && currentAgentWallet) {
      console.log('[wallet] Signing cancel with agent key (no MetaMask popup!)')

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
    console.log('[wallet] Signing cancel with MetaMask (popup required)')
    const signature = await signCancel(cancel)
    return {
      signature,
      agentMode: false
    }
  }, [wallet.tradingEnabled, signCancel])

  return {
    ...wallet,
    connect,
    disconnect,
    signOrder,
    signCancel,
    switchNetwork,
    // Agent key methods
    enableTrading,
    disableTrading,
    signOrderSmart,
    signCancelSmart,
    agentWallet
  }
}
