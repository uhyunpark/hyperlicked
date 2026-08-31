import {
  keccak256,
  TypedDataEncoder,
  type TypedDataDomain,
  type TypedDataField,
} from 'ethers'

/** Domain tag prepended to every canonical Rust `Transaction` encoding. */
export const ACTION_DOMAIN_TAG = 'HYPERLICKED-ACTION-V1\0' as const

export const CANONICAL_SIGNATURE_SCHEME = 'eip712-v1' as const
export const HYPERLICKED_DOMAIN_NAME = 'HyperLicked' as const
export const HYPERLICKED_DOMAIN_VERSION = '1' as const
export const CANONICAL_VALIDITY_WINDOW_MS = 60 * 60 * 1000

/** Use a zero lower bound to tolerate small client/node clock skew. */
export function createCanonicalValidity(now = Date.now()): {
  validAfter: string
  deadline: string
} {
  return {
    validAfter: '0',
    deadline: String(now + CANONICAL_VALIDITY_WINDOW_MS),
  }
}

/** Exact EIP-712 type used by Rust `SignedEnvelope::eip712_digest`. */
export const HYPERLICKED_TRANSACTION_TYPES: Record<string, TypedDataField[]> = {
  HyperLickedTransaction: [
    { name: 'chainDomain', type: 'bytes32' },
    { name: 'signer', type: 'address' },
    { name: 'nonce', type: 'uint64' },
    { name: 'validAfter', type: 'uint64' },
    { name: 'validUntil', type: 'uint64' },
    { name: 'actionHash', type: 'bytes32' },
  ],
}

export type CanonicalInteger = bigint | number | string
export type CanonicalChainDomain = string | Uint8Array
export type CanonicalSide = 'Bid' | 'Ask' | 0 | 1
export type CanonicalOrderType = 'Gtc' | 'Ioc' | 'Alo' | 0 | 1 | 2
export type CanonicalTriggerType = 'StopLoss' | 'TakeProfit' | 0 | 1

/** The subset of Rust `Transaction` that can be signed by the current UI. */
export type CanonicalAction =
  | {
      type: 'PlaceOrder'
      trader: string
      symbol: string
      side: CanonicalSide
      price: CanonicalInteger
      size: CanonicalInteger
      orderType: CanonicalOrderType
      reduceOnly: boolean
    }
  | {
      type: 'CancelOrder'
      trader: string
      orderId: string
    }
  | {
      type: 'PlaceTriggerOrder'
      trader: string
      symbol: string
      triggerType: CanonicalTriggerType
      triggerPrice: CanonicalInteger
      size: CanonicalInteger
      limitPrice: CanonicalInteger | null
      cloid?: string | null
    }
  | {
      type: 'CancelTriggerOrder'
      trader: string
      triggerOrderId: string
    }
  | {
      type: 'CancelTriggerOrderByCloid'
      trader: string
      symbol: string
      cloid: string
    }

export interface CanonicalSigningInput {
  chainDomain: CanonicalChainDomain
  signer: string
  nonce: CanonicalInteger
  validAfter: CanonicalInteger
  validUntil: CanonicalInteger
  action: CanonicalAction
}

export interface HyperLickedTransactionValue {
  chainDomain: string
  signer: string
  nonce: string
  validAfter: string
  validUntil: string
  actionHash: string
}

export interface CanonicalTypedData {
  domain: TypedDataDomain
  types: Record<string, TypedDataField[]>
  value: HyperLickedTransactionValue
  /** Raw `bincode::serialize(Transaction)` bytes, without the domain tag. */
  actionPayload: Uint8Array
  /** `ACTION_DOMAIN_TAG || actionPayload`, hashed as `actionHash`. */
  actionBytes: Uint8Array
  actionHash: string
  /** EIP-712 digest that the wallet signs. */
  digest: string
}

export interface CanonicalTypedDataSigner {
  signTypedData(
    domain: TypedDataDomain,
    types: Record<string, TypedDataField[]>,
    value: Record<string, any>,
  ): Promise<string>
}

const ZERO = BigInt(0)
const ONE = BigInt(1)
const BYTE_MASK = BigInt(255)
const MAX_U32 = 0xffff_ffff
const BITS_8 = BigInt(8)
const BITS_63 = BigInt(63)
const BITS_64 = BigInt(64)
const MAX_U64 = (ONE << BITS_64) - ONE
const MIN_I64 = -(ONE << BITS_63)
const MAX_I64 = (ONE << BITS_63) - ONE
const ADDRESS_PATTERN = /^0x[0-9a-fA-F]{40}$/
const BYTES32_PATTERN = /^0x[0-9a-fA-F]{64}$/

function integer(value: CanonicalInteger, label: string): bigint {
  if (typeof value === 'bigint') return value
  if (typeof value === 'number') {
    if (!Number.isSafeInteger(value)) {
      throw new RangeError(`${label} must be a safe integer when passed as a number`)
    }
    return BigInt(value)
  }
  if (typeof value !== 'string' || !/^-?\d+$/.test(value)) {
    throw new TypeError(`${label} must be a base-10 integer`)
  }
  return BigInt(value)
}

function uint64(value: CanonicalInteger, label: string): bigint {
  const parsed = integer(value, label)
  if (parsed < ZERO || parsed > MAX_U64) {
    throw new RangeError(`${label} is outside uint64 range`)
  }
  return parsed
}

/** Normalize an API nonce/timestamp without losing uint64 precision. */
export function canonicalU64(value: CanonicalInteger, label = 'value'): string {
  return uint64(value, label).toString()
}

/** Allocate the next contiguous nonce for composed user submissions. */
export function incrementCanonicalNonce(value: CanonicalInteger): string {
  const parsed = uint64(value, 'nonce')
  if (parsed === MAX_U64) throw new RangeError('nonce cannot be incremented')
  return (parsed + ONE).toString()
}

function int64(value: CanonicalInteger, label: string): bigint {
  const parsed = integer(value, label)
  if (parsed < MIN_I64 || parsed > MAX_I64) {
    throw new RangeError(`${label} is outside int64 range`)
  }
  return parsed
}

/** Normalize an Ethereum owner/signer to the exact lowercase Rust string form. */
export function normalizeAddress(value: string): string {
  if (typeof value !== 'string' || !ADDRESS_PATTERN.test(value)) {
    throw new TypeError('address must be 0x followed by exactly 40 hexadecimal characters')
  }
  return value.toLowerCase()
}

/** Normalize a chain domain to a lowercase `bytes32` hex string. */
export function normalizeChainDomain(value: CanonicalChainDomain): string {
  if (typeof value === 'string') {
    if (!BYTES32_PATTERN.test(value)) {
      throw new TypeError('chainDomain must be 0x followed by exactly 64 hexadecimal characters')
    }
    return value.toLowerCase()
  }
  if (!(value instanceof Uint8Array) || value.length !== 32) {
    throw new TypeError('chainDomain must contain exactly 32 bytes')
  }
  return bytesToHex(value)
}

function bytesToHex(value: Uint8Array): string {
  let result = '0x'
  for (const byte of value) result += byte.toString(16).padStart(2, '0')
  return result
}

class BincodeEncoder {
  private readonly bytes: number[] = []

  byte(value: number): void {
    this.bytes.push(value & 0xff)
  }

  bytesValue(value: Uint8Array): void {
    for (const byte of value) this.byte(byte)
  }

  u32(value: number): void {
    if (!Number.isInteger(value) || value < 0 || value > MAX_U32) {
      throw new RangeError('canonical enum discriminant is out of range')
    }
    this.byte(value)
    this.byte(value >>> 8)
    this.byte(value >>> 16)
    this.byte(value >>> 24)
  }

  u64(value: bigint): void {
    if (value < ZERO || value > MAX_U64) {
      throw new RangeError('canonical unsigned integer is out of range')
    }
    for (let offset = ZERO; offset < BITS_64; offset += BITS_8) {
      this.byte(Number((value >> offset) & BYTE_MASK))
    }
  }

  i64(value: bigint): void {
    if (value < MIN_I64 || value > MAX_I64) {
      throw new RangeError('canonical signed integer is out of range')
    }
    const unsigned = BigInt.asUintN(64, value)
    for (let offset = ZERO; offset < BITS_64; offset += BITS_8) {
      this.byte(Number((unsigned >> offset) & BYTE_MASK))
    }
  }

  string(value: string, label: string): void {
    if (typeof value !== 'string') throw new TypeError(`${label} must be a string`)
    const encoded = new TextEncoder().encode(value)
    this.u64(BigInt(encoded.length))
    this.bytesValue(encoded)
  }

  optionString(value: string | null | undefined, label: string): void {
    if (value == null) {
      this.byte(0)
      return
    }
    this.byte(1)
    this.string(value, label)
  }

  optionI64(value: CanonicalInteger | null | undefined, label: string): void {
    if (value == null) {
      this.byte(0)
      return
    }
    this.byte(1)
    this.i64(int64(value, label))
  }

  finish(): Uint8Array {
    return Uint8Array.from(this.bytes)
  }
}

function side(value: CanonicalSide): number {
  if (value === 'Bid' || value === 0) return 0
  if (value === 'Ask' || value === 1) return 1
  throw new RangeError('side must be Bid (0) or Ask (1)')
}

function orderType(value: CanonicalOrderType): number {
  if (value === 'Gtc' || value === 0) return 0
  if (value === 'Ioc' || value === 1) return 1
  if (value === 'Alo' || value === 2) return 2
  throw new RangeError('orderType must be Gtc (0), Ioc (1), or Alo (2)')
}

function triggerType(value: CanonicalTriggerType): number {
  if (value === 'StopLoss' || value === 0) return 0
  if (value === 'TakeProfit' || value === 1) return 1
  throw new RangeError('triggerType must be StopLoss (0) or TakeProfit (1)')
}

function actionTrader(action: CanonicalAction): string {
  return normalizeAddress(action.trader)
}

function encodePayload(action: CanonicalAction): Uint8Array {
  const encoded = new BincodeEncoder()
  const trader = actionTrader(action)

  switch (action.type) {
    case 'PlaceOrder':
      encoded.u32(0)
      encoded.string(trader, 'trader')
      encoded.string(action.symbol, 'symbol')
      encoded.u32(side(action.side))
      encoded.i64(int64(action.price, 'price'))
      encoded.i64(int64(action.size, 'size'))
      encoded.u32(orderType(action.orderType))
      if (typeof action.reduceOnly !== 'boolean') {
        throw new TypeError('reduceOnly must be a boolean')
      }
      encoded.byte(action.reduceOnly ? 1 : 0)
      break
    case 'CancelOrder':
      encoded.u32(1)
      encoded.string(trader, 'trader')
      encoded.string(action.orderId, 'orderId')
      break
    case 'PlaceTriggerOrder':
      encoded.u32(13)
      encoded.string(trader, 'trader')
      encoded.string(action.symbol, 'symbol')
      encoded.u32(triggerType(action.triggerType))
      encoded.i64(int64(action.triggerPrice, 'triggerPrice'))
      encoded.i64(int64(action.size, 'size'))
      encoded.optionI64(action.limitPrice, 'limitPrice')
      encoded.optionString(action.cloid, 'cloid')
      break
    case 'CancelTriggerOrder':
      encoded.u32(14)
      encoded.string(trader, 'trader')
      encoded.string(action.triggerOrderId, 'triggerOrderId')
      break
    case 'CancelTriggerOrderByCloid':
      encoded.u32(15)
      encoded.string(trader, 'trader')
      encoded.string(action.symbol, 'symbol')
      encoded.string(action.cloid, 'cloid')
      break
    default:
      throw new TypeError('unsupported canonical transaction variant')
  }

  return encoded.finish()
}

function tagAction(payload: Uint8Array): Uint8Array {
  const tag = new TextEncoder().encode(ACTION_DOMAIN_TAG)
  const result = new Uint8Array(tag.length + payload.length)
  result.set(tag)
  result.set(payload, tag.length)
  return result
}

/** Return exact `bincode::serialize(Transaction)` bytes for a supported action. */
export function encodeCanonicalActionPayload(action: CanonicalAction): Uint8Array {
  return encodePayload(action)
}

/** Return exact Rust `SignedEnvelope::action_bytes()` (`tag || bincode(action)`). */
export function encodeCanonicalAction(action: CanonicalAction): Uint8Array {
  return tagAction(encodePayload(action))
}

/** Compute the exact Rust `SignedEnvelope::action_hash()`. */
export function canonicalActionHash(action: CanonicalAction): string {
  return keccak256(encodeCanonicalAction(action))
}

function buildValue(input: CanonicalSigningInput): {
  domain: TypedDataDomain
  value: HyperLickedTransactionValue
  actionPayload: Uint8Array
  actionBytes: Uint8Array
  actionHash: string
  digest: string
} {
  const chainDomain = normalizeChainDomain(input.chainDomain)
  const signer = normalizeAddress(input.signer)
  const nonce = uint64(input.nonce, 'nonce')
  const validAfter = uint64(input.validAfter, 'validAfter')
  const validUntil = uint64(input.validUntil, 'validUntil')
  if (validUntil === ZERO || validAfter > validUntil) {
    throw new RangeError('validUntil must be non-zero and validAfter must not exceed it')
  }

  const actionPayload = encodeCanonicalActionPayload(input.action)
  if (actionTrader(input.action) !== signer) {
    throw new Error('action trader must match the typed-data signer')
  }
  const actionBytes = tagAction(actionPayload)
  const actionHash = keccak256(actionBytes)
  const domain: TypedDataDomain = {
    name: HYPERLICKED_DOMAIN_NAME,
    version: HYPERLICKED_DOMAIN_VERSION,
    salt: chainDomain,
  }
  const value: HyperLickedTransactionValue = {
    chainDomain,
    signer,
    nonce: nonce.toString(),
    validAfter: validAfter.toString(),
    validUntil: validUntil.toString(),
    actionHash,
  }

  return {
    domain,
    value,
    actionPayload,
    actionBytes,
    actionHash,
    digest: TypedDataEncoder.hash(domain, HYPERLICKED_TRANSACTION_TYPES, value),
  }
}

/** Build the complete canonical payload that a wallet must sign. */
export function buildCanonicalTypedData(input: CanonicalSigningInput): CanonicalTypedData {
  return {
    ...buildValue(input),
    types: HYPERLICKED_TRANSACTION_TYPES,
  }
}

/** Return the EIP-712 digest signed by Rust `SignedEnvelope`. */
export function canonicalSigningDigest(input: CanonicalSigningInput): string {
  return buildValue(input).digest
}

/** Sign canonical transaction data with an ethers-compatible signer. */
export async function signCanonicalTransaction(
  signer: CanonicalTypedDataSigner,
  input: CanonicalSigningInput,
): Promise<string> {
  const typedData = buildCanonicalTypedData(input)
  return signer.signTypedData(typedData.domain, typedData.types, typedData.value)
}
