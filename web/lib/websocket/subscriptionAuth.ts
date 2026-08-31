import type { JsonRpcSigner } from 'ethers'
import { normalizeAddress } from '../wallet/canonicalAction'

/**
 * The backend authenticates private subscriptions with an EIP-191
 * personal-sign over this exact message.
 */
export function buildSubscriptionAuthMessage(address: string, timestamp: number): string {
  return `Subscribe to ${address} at ${timestamp}`
}

export interface SubscriptionAuth {
  address: string
  timestamp: number
  signature: string
}

export interface AuthenticatedSubscriptionFrame {
  op: 'subscribe'
  address: string
  signature: string
  timestamp: number
}

/** A signature request tied to the socket that requested it. */
export interface PendingSubscription {
  socket: WebSocket
  address: string
}

/**
 * Create a fresh authentication payload for one WebSocket subscription.
 * The caller must pass the currently connected JsonRpcSigner; address
 * equality is checked again immediately before signing to protect against
 * wallet account changes while a connection is being established.
 */
export async function createSubscriptionAuth(
  signer: Pick<JsonRpcSigner, 'getAddress' | 'signMessage'>,
  address: string,
  timestamp = Math.floor(Date.now() / 1000),
): Promise<SubscriptionAuth> {
  const normalizedAddress = normalizeAddress(address)
  const signerAddress = normalizeAddress(await signer.getAddress())

  if (signerAddress !== normalizedAddress) {
    throw new Error('Connected wallet changed; reconnect before subscribing')
  }

  if (!Number.isSafeInteger(timestamp) || timestamp < 0) {
    throw new RangeError('Subscription timestamp must be a non-negative safe integer')
  }

  const message = buildSubscriptionAuthMessage(normalizedAddress, timestamp)
  const signature = await signer.signMessage(message)

  // Account changes can happen while the wallet approval dialog is open.
  // Do not return a signature if the signer no longer represents the target.
  const signedByAddress = normalizeAddress(await signer.getAddress())
  if (signedByAddress !== normalizedAddress) {
    throw new Error('Connected wallet changed; reconnect before subscribing')
  }

  return {
    address: normalizedAddress,
    timestamp,
    signature,
  }
}

export function buildAuthenticatedSubscriptionFrame(
  auth: SubscriptionAuth,
): AuthenticatedSubscriptionFrame {
  return {
    op: 'subscribe',
    address: auth.address,
    signature: auth.signature,
    timestamp: auth.timestamp,
  }
}
