'use client'

import { useState, useEffect, useCallback } from 'react'
import { BrowserProvider, JsonRpcSigner } from 'ethers'
import { config } from './config'

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

export interface WalletState {
  isConnected: boolean
  address: string | null
  provider: BrowserProvider | null
  signer: JsonRpcSigner | null
  isRabby: boolean
  chainId: number | null
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

export function useWallet() {
  const [wallet, setWallet] = useState<WalletState>({
    isConnected: false,
    address: null,
    provider: null,
    signer: null,
    isRabby: false,
    chainId: null
  })

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
        chainId
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
    setWallet({
      isConnected: false,
      address: null,
      provider: null,
      signer: null,
      isRabby: false,
      chainId: null
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
      if (accounts.length === 0) {
        disconnect()
      } else {
        // Reconnect with new account
        connect()
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

  return {
    ...wallet,
    connect,
    disconnect,
    signOrder,
    switchNetwork
  }
}
