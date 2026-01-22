'use client'

import { useState, useEffect, useCallback } from 'react'
import { useTradingStore } from '@/lib/store'
import { toast } from '@/components/ui/Toast'
import { requestFaucet, getAccount } from '@/lib/api'

export interface AccountData {
  balance: number
  lockedCollateral: number
  availableBalance: number
  unrealizedPnL: number
  totalEquity: number
}

/**
 * Hook for managing account data fetching and faucet requests
 */
export function useAccountData(address: string | null) {
  const { balanceRefreshTrigger, positions, markPrices } = useTradingStore()
  const [account, setAccount] = useState<AccountData | null>(null)
  const [isLoadingAccount, setIsLoadingAccount] = useState(false)
  const [isFaucetLoading, setIsFaucetLoading] = useState(false)

  // Calculate realtime unrealized PnL from positions and mark prices
  const getRealtimeUnrealizedPnL = useCallback(() => {
    if (positions.length === 0) return 0
    return positions.reduce((sum, pos) => {
      const currentMarkPrice = markPrices[pos.symbol] ?? pos.markPrice
      const pnl = (currentMarkPrice - pos.entryPrice) * pos.size
      return sum + pnl
    }, 0)
  }, [positions, markPrices])

  // Account fetch function (reusable)
  const fetchAccount = useCallback(async () => {
    if (!address) return
    setIsLoadingAccount(true)
    try {
      const data = await getAccount(address)
      setAccount({
        balance: data.balance / 100, // cents to dollars
        lockedCollateral: data.lockedCollateral / 100,
        availableBalance: data.availableBalance / 100,
        unrealizedPnL: data.unrealizedPnL / 100,
        totalEquity: data.totalEquity / 100
      })
    } catch (error) {
      console.error('[account] Failed to fetch:', error)
    } finally {
      setIsLoadingAccount(false)
    }
  }, [address])

  // Fetch account data when address changes
  useEffect(() => {
    if (!address) {
      setAccount(null)
      return
    }

    fetchAccount()
    // Refresh every 10 seconds
    const interval = setInterval(fetchAccount, 10000)
    return () => clearInterval(interval)
  }, [address, fetchAccount])

  // Refetch account when WebSocket balance update is received
  useEffect(() => {
    if (balanceRefreshTrigger > 0 && address) {
      console.log('[account] Balance refresh triggered via WebSocket')
      fetchAccount()
    }
  }, [balanceRefreshTrigger, address, fetchAccount])

  // Faucet handler (dev mode only)
  const handleFaucet = useCallback(async () => {
    if (!address || isFaucetLoading) return
    setIsFaucetLoading(true)
    try {
      const success = await requestFaucet(address)
      if (success) {
        toast.success('Faucet', 'Received $100,000 test USDT')
        // Wait for block to be processed then refresh
        setTimeout(() => fetchAccount(), 500)
      } else {
        toast.error('Faucet Failed', 'Could not get test USDT')
      }
    } catch (error: any) {
      toast.error('Faucet Failed', error.message)
    } finally {
      setIsFaucetLoading(false)
    }
  }, [address, isFaucetLoading, fetchAccount])

  const accountBalance = account?.totalEquity ?? 0
  const availableBalance = account?.availableBalance ?? 0

  // Calculate margin ratio
  const marginRatio = account && account.lockedCollateral > 0
    ? ((account.totalEquity / account.lockedCollateral) * 100)
    : 0

  return {
    account,
    accountBalance,
    availableBalance,
    marginRatio,
    isLoadingAccount,
    isFaucetLoading,
    handleFaucet,
    getRealtimeUnrealizedPnL,
  }
}
