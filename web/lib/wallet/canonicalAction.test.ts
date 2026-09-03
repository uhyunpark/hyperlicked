import assert from 'node:assert/strict'

import {
  buildCanonicalTypedData,
  canonicalActionHash,
  canonicalSigningDigest,
  encodeCanonicalActionPayload,
  normalizeAddress,
  normalizeChainDomain,
} from './canonicalAction'

const trader = '0x1111111111111111111111111111111111111111'
const chainDomain = `0x${'07'.repeat(32)}`

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
}

const placeOrder = {
  type: 'PlaceOrder' as const,
  trader,
  symbol: 'BTC-USDT',
  side: 'Ask' as const,
  price: '-5000000',
  size: '100',
  orderType: 'Alo' as const,
  reduceOnly: true,
}

const typedData = buildCanonicalTypedData({
  chainDomain,
  signer: trader.toUpperCase().replace('0X', '0x'),
  nonce: '3',
  validAfter: '2',
  validUntil: '100',
  action: placeOrder,
})

assert.equal(
  hex(typedData.actionPayload),
  '000000002a0000000000000030783131313131313131313131313131313131313131313131313131313131313131313131313131313108000000000000004254432d5553445401000000c0b4b3ffffffffff64000000000000000200000001',
)
assert.equal(
  typedData.actionHash,
  '0x910d1b76d0c97401a44bcb0cb5363665a4985dda32432f82c9fcc1b9cdbc3ed7',
)
assert.equal(
  typedData.digest,
  '0x1c2d5fdf66cec701989534bc738bc0692d5e520977c82872183aaa458418425f',
)
assert.equal(canonicalSigningDigest({
  chainDomain,
  signer: trader,
  nonce: '3',
  validAfter: '2',
  validUntil: '100',
  action: placeOrder,
}), typedData.digest)

assert.equal(
  canonicalActionHash({
    type: 'CancelOrder',
    trader,
    orderId: '42',
  }),
  '0xacc76479ddf2bc1981dc0b4de2625d0061c6cbf1cde3d1559863df91a5bc485c',
)
assert.equal(
  canonicalActionHash({
    type: 'PlaceTriggerOrder',
    trader,
    symbol: 'BTC-USDT',
    triggerType: 'TakeProfit',
    triggerPrice: '6000000',
    size: '-100',
    limitPrice: null,
    cloid: 'client-1',
  }),
  '0x93bfe12a7e20050ae21a0f10f96f1c6d7a247bd6f1eaf2c464a2a9f3d0c65c59',
)
assert.equal(
  canonicalActionHash({
    type: 'CancelTriggerOrder',
    trader,
    triggerOrderId: 'trig-42',
  }),
  '0x0e39d22b1d0e329c2243e5a80a426a060ac685bcd608505e95e2fe8ab8e2de71',
)
assert.equal(
  canonicalActionHash({
    type: 'CancelTriggerOrderByCloid',
    trader,
    symbol: 'BTC-USDT',
    cloid: 'client-1',
  }),
  '0x346e3bc34aff59dee676fa8cf3b96edefae4e00626c1b8c12e131260bdc0e6f3',
)

assert.equal(normalizeAddress(trader.toUpperCase().replace('0X', '0x')), trader)
assert.equal(normalizeChainDomain(chainDomain.toUpperCase().replace('0X', '0x')), chainDomain)
assert.throws(() => encodeCanonicalActionPayload({ ...placeOrder, price: '9223372036854775808' }), /int64 range/)
assert.throws(() => buildCanonicalTypedData({
  chainDomain,
  signer: trader,
  nonce: '0',
  validAfter: '1',
  validUntil: '0',
  action: placeOrder,
}), /validUntil/)

console.log('canonicalAction golden vectors passed')
