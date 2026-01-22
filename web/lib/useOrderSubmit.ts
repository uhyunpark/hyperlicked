'use client'

import { useCallback } from 'react'
import { useTradingStore } from '@/lib/store'
import { useWallet, type OrderToSign } from '@/lib/useWallet'
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
  const { selectedSymbol, currentPrice } = useTradingStore()
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
      const currentNonce = nonceData.nonce

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

      const orderToSign: OrderToSign = {
        symbol: selectedSymbol,
        side: side === 'buy' ? 1 : 2,
        type: tifCode,
        price: convertToApiPrice(orderPrice).toString(),
        qty: convertToApiSize(orderSize).toString(),
        nonce: currentNonce.toString(),
        deadline: '0',
        leverage,
        owner: wallet.address,
        reduce_only: reduceOnly
      }

      const { signature, agentMode, delegationId } = await wallet.signOrderSmart(orderToSign)

      const signedTx = {
        type: 'order' as const,
        order: orderToSign,
        signature,
        agent_mode: agentMode,
        delegation_id: delegationId
      }

      const response = await submitSignedTransaction(signedTx)

      if (response.status === 'submitted') {
        const method = agentMode ? 'Agent Key' : (wallet.isRabby ? 'Rabby' : 'MetaMask')
        toast.success('Order Submitted', `Order #${response.orderId} signed with ${method}`)

        // Place TP/SL orders if enabled and this is not a reduce-only order
        if (tpSlEnabled && !reduceOnly) {
          const orderSizeApi = convertToApiSize(parseFloat(size))
          await placeTpSlOrders(wallet.address!, selectedSymbol, orderSizeApi, tpPrice, slPrice)
        }

        callbacks.onSuccess()

        // Immediately refresh open orders
        await refreshOpenOrders(wallet.address!)
      } else {
        toast.error('Order Rejected', response.message || 'Unknown error')
      }
    } catch (error) {
      console.error('[order] Error:', error)
      toast.error('Order Failed', error instanceof Error ? error.message : 'Unknown error')
    }
  }, [wallet, selectedSymbol, currentPrice])

  return { submitOrder }
}

// Helper to place TP/SL orders
async function placeTpSlOrders(
  address: string,
  symbol: string,
  size: number,
  tpPrice: string,
  slPrice: string
) {
  // Place Take Profit if set
  if (tpPrice && parseFloat(tpPrice) > 0) {
    try {
      await placeTriggerOrder({
        trader: address,
        symbol,
        triggerType: 'tp',
        triggerPrice: convertToApiPrice(parseFloat(tpPrice)),
        size,
      })
      toast.success('Take Profit Set', `TP at $${parseFloat(tpPrice).toLocaleString()}`)
    } catch (err: any) {
      toast.warning('TP Failed', err.message)
    }
  }

  // Place Stop Loss if set
  if (slPrice && parseFloat(slPrice) > 0) {
    try {
      await placeTriggerOrder({
        trader: address,
        symbol,
        triggerType: 'sl',
        triggerPrice: convertToApiPrice(parseFloat(slPrice)),
        size,
      })
      toast.success('Stop Loss Set', `SL at $${parseFloat(slPrice).toLocaleString()}`)
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
