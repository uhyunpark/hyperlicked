import assert from 'node:assert/strict'
import { verifyMessage, Wallet } from 'ethers'

import {
  buildAuthenticatedSubscriptionFrame,
  buildSubscriptionAuthMessage,
  createSubscriptionAuth,
} from './subscriptionAuth'

const auth = {
  address: '0x1111111111111111111111111111111111111111',
  timestamp: 1_700_000_000,
  signature: `0x${'ab'.repeat(65)}`,
}

assert.equal(
  buildSubscriptionAuthMessage(auth.address, auth.timestamp),
  'Subscribe to 0x1111111111111111111111111111111111111111 at 1700000000',
)
assert.deepEqual(buildAuthenticatedSubscriptionFrame(auth), {
  op: 'subscribe',
  address: auth.address,
  signature: auth.signature,
  timestamp: auth.timestamp,
})

const signedMessages: string[] = []
const mockSigner = {
  getAddress: async () => '0x1111111111111111111111111111111111111111',
  signMessage: async (message: string) => {
    signedMessages.push(message)
    return auth.signature
  },
}

async function runSignerAssertions() {
  const signedAuth = await createSubscriptionAuth(
    mockSigner,
    auth.address.toUpperCase().replace('0X', '0x'),
    auth.timestamp,
  )
  assert.deepEqual(signedAuth, auth)
  assert.deepEqual(signedMessages, [buildSubscriptionAuthMessage(auth.address, auth.timestamp)])

  const wallet = new Wallet(`0x${'11'.repeat(32)}`)
  const walletAuth = await createSubscriptionAuth(wallet, wallet.address, auth.timestamp)
  assert.equal(
    verifyMessage(buildSubscriptionAuthMessage(wallet.address.toLowerCase(), auth.timestamp), walletAuth.signature),
    wallet.address,
  )

  await assert.rejects(
    createSubscriptionAuth(
      { ...mockSigner, getAddress: async () => '0x2222222222222222222222222222222222222222' },
      auth.address,
      auth.timestamp,
    ),
    /Connected wallet changed/,
  )
}

runSignerAssertions().then(() => {
  console.log('subscription auth message/frame vectors passed')
}).catch((error: unknown) => {
  console.error(error)
  process.exitCode = 1
})
