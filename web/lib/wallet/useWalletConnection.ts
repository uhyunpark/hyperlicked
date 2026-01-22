'use client'

import { useState, useEffect, useCallback, useRef } from 'react'
import { BrowserProvider } from 'ethers'
import { config } from '../config'
import { useWalletStore } from '../store'
import {
  setExplicitDisconnect,
  wasExplicitlyDisconnected,
  clearAgentKey,
  getAgentKeyWalletAddress,
} from '../agentKey'
import type { LocalWalletState } from './types'

/**
 * Hook for wallet connection and disconnect logic
 */
export function useWalletConnection() {
  const { isConnected, address, setTradingEnabled } = useWalletStore()

  const [wallet, setWallet] = useState<LocalWalletState>({
    provider: null,
    signer: null,
    isRabby: false,
    chainId: null,
    error: null,
    needsReconnect: false
  })

  const hasAutoConnectAttempted = useRef(false)
  const prevAddressRef = useRef<string | null>(null)

  // Sync prev address ref
  useEffect(() => {
    prevAddressRef.current = address
  }, [address])

  // Detect if Rabby Wallet is installed
  const detectRabby = useCallback(() => {
    if (typeof window === 'undefined') return false
    const ethereum = (window as any).ethereum
    return ethereum?.isRabby === true
  }, [])

  // Connect to wallet
  const connect = useCallback(async () => {
    if (typeof window === 'undefined') {
      throw new Error('Window is undefined - not running in browser')
    }

    const ethereum = (window as any).ethereum
    if (!ethereum) {
      throw new Error('No Ethereum wallet detected. Please install Rabby Wallet or MetaMask.')
    }

    setExplicitDisconnect(false)

    // Request permissions (EIP-2255)
    await ethereum.request({
      method: 'wallet_requestPermissions',
      params: [{ eth_accounts: {} }]
    })

    const accounts = await ethereum.request({ method: 'eth_accounts' })
    if (!accounts || accounts.length === 0) {
      throw new Error('No accounts found')
    }

    const provider = new BrowserProvider(ethereum)
    const signer = await provider.getSigner()
    const connectedAddress = await signer.getAddress()
    const network = await provider.getNetwork()
    const chainId = Number(network.chainId)
    const isRabby = detectRabby()

    setWallet({
      provider,
      signer,
      isRabby,
      chainId,
      error: null,
      needsReconnect: false
    })

    useWalletStore.getState().setConnected(connectedAddress, chainId, isRabby)

    return { address: connectedAddress, isRabby, chainId }
  }, [detectRabby])

  // Disconnect wallet
  const disconnect = useCallback(async (reason?: string) => {
    if (!reason) {
      setExplicitDisconnect(true)
    }

    // Revoke wallet permissions (EIP-2255)
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

    setTradingEnabled(false)

    setWallet({
      provider: null,
      signer: null,
      isRabby: false,
      chainId: null,
      error: reason || null,
      needsReconnect: !!reason
    })

    useWalletStore.getState().setDisconnected()
  }, [setTradingEnabled])

  // Clear error
  const clearError = useCallback(() => {
    setWallet(prev => ({ ...prev, error: null, needsReconnect: false }))
  }, [])

  // Switch network
  const switchNetwork = useCallback(async (targetChainId: number) => {
    const ethereum = (window as any).ethereum
    if (!ethereum) throw new Error('No wallet detected')

    try {
      await ethereum.request({
        method: 'wallet_switchEthereumChain',
        params: [{ chainId: `0x${targetChainId.toString(16)}` }]
      })
    } catch (error: any) {
      if (error.code === 4902) {
        await addNetwork(targetChainId)
      } else {
        throw error
      }
    }
  }, [])

  // Add custom network
  const addNetwork = useCallback(async (chainId: number) => {
    const ethereum = (window as any).ethereum
    if (!ethereum) throw new Error('No wallet detected')

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
  }, [])

  // Listen for account and chain changes
  useEffect(() => {
    if (typeof window === 'undefined') return

    const ethereum = (window as any).ethereum
    if (!ethereum) return

    const handleAccountsChanged = (accounts: string[]) => {
      if (accounts.length === 0) {
        disconnect('Wallet disconnected')
        return
      }

      const newAddress = accounts[0].toLowerCase()
      const prevAddress = prevAddressRef.current?.toLowerCase()

      if (prevAddress && prevAddress !== newAddress) {
        disconnect('Wallet address changed. Please reconnect.')
      }
    }

    const handleChainChanged = (chainIdHex: string) => {
      const newChainId = parseInt(chainIdHex, 16)
      const storeState = useWalletStore.getState()

      if (!storeState.isConnected) return

      const expectedChainId = config.network.chainId
      if (newChainId !== expectedChainId) {
        setWallet(prev => ({
          ...prev,
          chainId: newChainId,
          error: `Wrong network. Please switch to ${config.network.chainName} (Chain ID: ${expectedChainId})`,
          needsReconnect: false
        }))
      } else {
        setWallet(prev => ({
          ...prev,
          chainId: newChainId,
          error: null,
          needsReconnect: false
        }))
      }

      useWalletStore.getState().setChainId(newChainId)
    }

    ethereum.on('accountsChanged', handleAccountsChanged)
    ethereum.on('chainChanged', handleChainChanged)

    return () => {
      ethereum.removeListener('accountsChanged', handleAccountsChanged)
      ethereum.removeListener('chainChanged', handleChainChanged)
    }
  }, [disconnect])

  // Auto-connect on mount
  useEffect(() => {
    if (hasAutoConnectAttempted.current) return
    hasAutoConnectAttempted.current = true

    if (typeof window === 'undefined') return

    const ethereum = (window as any).ethereum
    if (!ethereum) return

    if (wasExplicitlyDisconnected()) return

    ethereum.request({ method: 'eth_accounts' })
      .then(async (accounts: string[]) => {
        if (accounts.length > 0) {
          const provider = new BrowserProvider(ethereum)
          const signer = await provider.getSigner()
          const connectedAddress = await signer.getAddress()
          const network = await provider.getNetwork()
          const chainId = Number(network.chainId)
          const isRabby = ethereum.isRabby === true

          setWallet({
            provider,
            signer,
            isRabby,
            chainId,
            error: null,
            needsReconnect: false
          })

          useWalletStore.getState().setConnected(connectedAddress, chainId, isRabby)
        }
      })
      .catch((error: any) => {
        console.error('[wallet] Auto-connect failed:', error)
      })
  }, [])

  // Clear agent key if wallet changed
  useEffect(() => {
    if (!address) return

    const storedWalletAddress = getAgentKeyWalletAddress()
    if (storedWalletAddress && storedWalletAddress.toLowerCase() !== address.toLowerCase()) {
      clearAgentKey()
    }
  }, [address])

  return {
    ...wallet,
    isConnected,
    address,
    connect,
    disconnect,
    clearError,
    switchNetwork,
  }
}
