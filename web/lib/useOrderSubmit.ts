'use client'

import { useCallback } from 'react'
import { useTradingStore } from '@/lib/store'
import { useWallet, type OrderToSign, type TriggerOrderToSign } from '@/lib/useWallet'
import { toast } from '@/components/ui/Toast'
import type { Side, OrderType, TimeInForce } from '@/lib/types'
import { TIF_CODES } from '@/lib/types'
import {
  submitSignedTransaction,
  convertToApiPrice,
  convertToApiSize,
  getNonce,
  getOrders,
  convertPrice,
  convertSize,
  placeTriggerOrder,
} from '@/lib/api'
import {
  CANONICAL_SIGNATURE_SCHEME,
  canonicalU64,
  createCanonicalValidity,
  incrementCanonicalNonce,
} from '@/lib/wallet/canonicalAction'

interface OrderParams {
  side: Side
  orderType: OrderType
  tif: TimeInForce
  price: string
  size: string
  leverage: number
  reduceOnly: boolean
  tpSlEnabled: boolean
  tpPrice: string
  slPrice: string
}

interface OrderCallbacks {
  onSuccess: () => void
}

/**
 * Hook for handling order submission logic
 */
export function useOrderSubmit() {
  const selectedSymbol = useTradingStore((s) => s.selectedSymbol)
  const currentPrice = useTradingStore((s) => s.currentPrice)
  const wallet = useWallet()

  const submitOrder = useCallback(async (
    params: OrderParams,
    callbacks: OrderCallbacks
  ) => {
    const { side, orderType, tif, price, size, leverage, reduceOnly, tpSlEnabled, tpPrice, slPrice } = params

    // Validation
    if (!size || parseFloat(size) <= 0) {
      toast.warning('Invalid Size', 'Please enter a valid size')
      return
    }

    if (orderType === 'limit' && (!price || parseFloat(price) <= 0)) {
      toast.warning('Invalid Price', 'Please enter a valid price')
      return
    }

    if (!wallet.isConnected || !wallet.address) {
      toast.warning('Not Connected', 'Please connect your wallet first')
      return
    }

    try {
      // Always fetch fresh nonce from server before submitting
      const nonceData = await getNonce(wallet.address!)
      const currentNonce = canonicalU64(nonceData.nonce, 'nonce')

      // For market orders, use sweep prices to ensure execution
      let orderPrice: number
      if (orderType === 'market') {
        orderPrice = side === 'buy' ? currentPrice * 2 : 0.01
      } else {
        orderPrice = parseFloat(price)
      }
      const orderSize = parseFloat(size)

      // For market orders, always use IOC (immediate-or-cancel)
      const tifCode = orderType === 'market' ? TIF_CODES.ioc : TIF_CODES[tif]
      const { validAfter, deadline } = createCanonicalValidity()

      const orderToSign: OrderToSign = {
        symbol: selectedSymbol,
        side: side === 'buy' ? 1 : 2,
        type: tifCode,
        price: convertToApiPrice(orderPrice).toString(),
        qty: convertToApiSize(orderSize).toString(),
        nonce: currentNonce,
        deadline,
        validAfter,
        leverage,
        owner: wallet.address,
        reduce_only: reduceOnly
      }

      const signature = await wallet.signCanonicalOrder(orderToSign)

      const signedTx = {
        type: 'order' as const,
        order: orderToSign,
        signature,
        signatureScheme: CANONICAL_SIGNATURE_SCHEME,
      }

      const response = await submitSignedTransaction(signedTx)

      if (response.status === 'pending') {
        const method = wallet.isRabby ? 'Rabby' : 'MetaMask'
        toast.success('Order Accepted', `Transaction ${response.tx_hash.slice(0, 10)}… is pending (${method})`)

        // Place TP/SL orders if enabled and this is not a reduce-only order
        if (tpSlEnabled && !reduceOnly) {
          const orderSizeApi = convertToApiSize(parseFloat(size))
          await placeTpSlOrders(
            wallet,
            selectedSymbol,
            orderSizeApi,
            tpPrice,
            slPrice,
            incrementCanonicalNonce(currentNonce),
          )
        }

        callbacks.onSuccess()

        // Immediately refresh open orders
        await refreshOpenOrders(wallet.address!)
      }
    } catch (error) {
      console.error('[order] Error:', error)
      toast.error('Order Failed', error instanceof Error ? error.message : 'Unknown error')
    }
  }, [wallet, selectedSymbol, currentPrice])

  return { submitOrder }
}

// Helper to place TP/SL orders with signed requests
async function placeTpSlOrders(
  wallet: ReturnType<typeof useWallet>,
  symbol: string,
  size: number,
  tpPrice: string,
  slPrice: string,
  firstNonce: string,
) {
  if (!wallet.address) return
  let nextNonce = firstNonce

  // Place Take Profit if set
  if (tpPrice && parseFloat(tpPrice) > 0) {
    try {
      const triggerToSign: TriggerOrderToSign = {
        symbol,
        triggerType: 2, // TakeProfit
        triggerPrice: convertToApiPrice(parseFloat(tpPrice)).toString(),
        size: size.toString(),
        limitPrice: '0',
        nonce: nextNonce,
        owner: wallet.address,
        ...createCanonicalValidity(),
      }
      const signature = await wallet.signCanonicalTriggerOrder(triggerToSign)
      const response = await placeTriggerOrder({
        trigger: triggerToSign,
        signature,
        signatureScheme: CANONICAL_SIGNATURE_SCHEME,
      })
      if (response.status === 'pending') {
        toast.success(
          'Take Profit Accepted',
          `Transaction ${response.tx_hash.slice(0, 10)}… is pending (TP at $${parseFloat(tpPrice).toLocaleString()})`,
        )
        nextNonce = incrementCanonicalNonce(nextNonce)
      }
    } catch (err: any) {
      toast.warning('TP Failed', err.message)
    }
  }

  // Place Stop Loss if set
  if (slPrice && parseFloat(slPrice) > 0) {
    try {
      const triggerToSign: TriggerOrderToSign = {
        symbol,
        triggerType: 1, // StopLoss
        triggerPrice: convertToApiPrice(parseFloat(slPrice)).toString(),
        size: size.toString(),
        limitPrice: '0',
        nonce: nextNonce,
        owner: wallet.address,
        ...createCanonicalValidity(),
      }
      const signature = await wallet.signCanonicalTriggerOrder(triggerToSign)
      const response = await placeTriggerOrder({
        trigger: triggerToSign,
        signature,
        signatureScheme: CANONICAL_SIGNATURE_SCHEME,
      })
      if (response.status === 'pending') {
        toast.success(
          'Stop Loss Accepted',
          `Transaction ${response.tx_hash.slice(0, 10)}… is pending (SL at $${parseFloat(slPrice).toLocaleString()})`,
        )
        nextNonce = incrementCanonicalNonce(nextNonce)
      }
    } catch (err: any) {
      toast.warning('SL Failed', err.message)
    }
  }
}

// Helper to refresh open orders
async function refreshOpenOrders(address: string) {
  try {
    const ordersData = await getOrders(address)
    const orders = ordersData
      .filter(o => o.status === 'open' || o.status === 'partial')
      .map(o => {
        const size = convertSize(o.size)
        const filled = convertSize(o.filled)
        return {
          id: o.id,
          symbol: o.symbol,
          side: o.side as 'buy' | 'sell',
          type: (o.type === 'market' ? 'market' : 'limit') as 'limit' | 'market',
          price: convertPrice(o.price),
          size,
          filled,
          remaining: size - filled,
          status: (o.status === 'partial' ? 'open' : o.status) as 'open' | 'filled' | 'cancelled',
          timestamp: o.timestamp
        }
      })
    useTradingStore.getState().setOpenOrders(orders)
  } catch (err) {
    console.error('[order] Failed to refresh orders:', err)
  }
}
