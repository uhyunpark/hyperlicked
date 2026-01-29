# Wallet Integration

Reference for wallet connection and signing patterns.

## Table of Contents

- [useWallet Hook](#usewallet-hook)
- [Connection Flow](#connection-flow)
- [EIP-712 Signing](#eip-712-signing)
- [Agent Keys](#agent-keys)

---

## useWallet Hook

### Interface

```typescript
interface UseWalletReturn {
  // State (from Zustand)
  isConnected: boolean
  address: string | null
  chainId: number | null
  tradingEnabled: boolean
  error: string | null

  // Methods
  connect: () => Promise<void>
  disconnect: () => void
  signOrder: (order: OrderToSign) => Promise<string>
  signOrderSmart: (order: OrderToSign) => Promise<string>
  enableTrading: () => Promise<void>
  clearError: () => void
}
```

### Basic Usage

```typescript
const {
  isConnected,
  address,
  connect,
  disconnect,
  signOrderSmart,
} = useWallet()

// Connect
<button onClick={connect}>Connect Wallet</button>

// Sign order
const signature = await signOrderSmart(order)
```

---

## Connection Flow

### Detection

```typescript
const detectWallet = () => {
  if (typeof window === 'undefined') return null
  if (!window.ethereum) return null

  return {
    provider: window.ethereum,
    isRabby: window.ethereum.isRabby ?? false,
  }
}
```

### Connect

```typescript
const connect = async () => {
  const wallet = detectWallet()
  if (!wallet) {
    setError('No wallet detected')
    return
  }

  try {
    // Request permission (EIP-2255)
    await wallet.provider.request({
      method: 'wallet_requestPermissions',
      params: [{ eth_accounts: {} }],
    })

    // Get accounts
    const accounts = await wallet.provider.request({
      method: 'eth_accounts',
    })

    if (accounts.length === 0) {
      throw new Error('No accounts found')
    }

    const address = accounts[0]
    const chainId = await wallet.provider.request({ method: 'eth_chainId' })

    // Create provider/signer
    const provider = new BrowserProvider(wallet.provider)
    const signer = await provider.getSigner()

    // Update store
    useWalletStore.getState().setConnected(address, wallet.isRabby, parseInt(chainId))

    // Check for existing agent key
    await checkAgentKey(address)

  } catch (error) {
    setError(error.message)
  }
}
```

### Auto-Connect

```typescript
useEffect(() => {
  // Skip if explicitly disconnected
  if (localStorage.getItem('wallet-disconnected')) return

  // Try to auto-connect
  const autoConnect = async () => {
    const accounts = await wallet.provider.request({ method: 'eth_accounts' })
    if (accounts.length > 0) {
      await connect()
    }
  }

  autoConnect()
}, [])
```

### Event Listeners

```typescript
useEffect(() => {
  if (!wallet) return

  const handleAccountsChanged = (accounts: string[]) => {
    if (accounts.length === 0) {
      disconnect()
    } else if (accounts[0] !== address) {
      // Address changed - reconnect
      connect()
    }
  }

  const handleChainChanged = () => {
    // Reload on chain change
    window.location.reload()
  }

  wallet.provider.on('accountsChanged', handleAccountsChanged)
  wallet.provider.on('chainChanged', handleChainChanged)

  return () => {
    wallet.provider.removeListener('accountsChanged', handleAccountsChanged)
    wallet.provider.removeListener('chainChanged', handleChainChanged)
  }
}, [wallet, address])
```

---

## EIP-712 Signing

### Domain

```typescript
// lib/config.ts
export const EIP712_DOMAIN = {
  name: 'Hyperlicked',
  version: '1',
  chainId: config.network.chainId,
}
```

### Order Types

```typescript
export const EIP712_ORDER_TYPES = {
  Order: [
    { name: 'trader', type: 'address' },
    { name: 'symbol', type: 'string' },
    { name: 'side', type: 'uint8' },
    { name: 'price', type: 'int64' },
    { name: 'size', type: 'int64' },
    { name: 'orderType', type: 'uint8' },
    { name: 'reduceOnly', type: 'bool' },
    { name: 'nonce', type: 'uint64' },
  ],
}
```

### Signing

```typescript
const signOrder = async (order: OrderToSign): Promise<string> => {
  if (!signer) throw new Error('Not connected')

  const signature = await signer.signTypedData(
    EIP712_DOMAIN,
    EIP712_ORDER_TYPES,
    order
  )

  return signature
}
```

---

## Agent Keys

### Overview

Agent keys enable gasless trading:
1. User generates random keypair (stored in localStorage)
2. User signs delegation with main wallet (one-time popup)
3. Future orders signed with agent key (no popup)

### Storage

```typescript
// lib/agentKey.ts

interface StoredAgentKey {
  address: string
  privateKey: string
  delegationSignature: string
  expiry: number
}

export function getAgentKey(masterAddress: string): StoredAgentKey | null {
  const key = `agent-key-${masterAddress}`
  const stored = localStorage.getItem(key)
  if (!stored) return null

  const parsed = JSON.parse(stored)
  if (parsed.expiry < Date.now()) {
    localStorage.removeItem(key)
    return null
  }

  return parsed
}

export function storeAgentKey(masterAddress: string, agentKey: StoredAgentKey) {
  const key = `agent-key-${masterAddress}`
  localStorage.setItem(key, JSON.stringify(agentKey))
}
```

### Enable Trading

```typescript
const enableTrading = async () => {
  if (!signer || !address) throw new Error('Not connected')

  // Generate random keypair
  const agentWallet = Wallet.createRandom()

  // Create delegation
  const expiry = Math.floor(Date.now() / 1000) + 7 * 24 * 60 * 60  // 7 days
  const delegation = {
    master: address,
    agent: agentWallet.address,
    expiry,
    nonce: Date.now(),
  }

  // Sign delegation with main wallet
  const signature = await signer.signTypedData(
    EIP712_DOMAIN,
    EIP712_DELEGATION_TYPES,
    delegation
  )

  // Store agent key
  storeAgentKey(address, {
    address: agentWallet.address,
    privateKey: agentWallet.privateKey,
    delegationSignature: signature,
    expiry: expiry * 1000,
  })

  // Register with backend
  await registerDelegation({
    ...delegation,
    signature,
  })

  useWalletStore.getState().setTradingEnabled(true, agentWallet.address)
}
```

### Smart Signing

```typescript
const signOrderSmart = async (order: OrderToSign): Promise<string> => {
  // Try agent key first
  const agentKey = getAgentKey(address!)
  if (agentKey && agentKey.expiry > Date.now()) {
    const agentWallet = new Wallet(agentKey.privateKey)
    return agentWallet.signTypedData(
      EIP712_DOMAIN,
      EIP712_ORDER_TYPES,
      order
    )
  }

  // Fall back to main wallet
  return signOrder(order)
}
```

---

## Network Switching

```typescript
const switchNetwork = async () => {
  try {
    await wallet.provider.request({
      method: 'wallet_switchEthereumChain',
      params: [{ chainId: `0x${config.network.chainId.toString(16)}` }],
    })
  } catch (error) {
    // Chain not added, try to add it
    if (error.code === 4902) {
      await wallet.provider.request({
        method: 'wallet_addEthereumChain',
        params: [{
          chainId: `0x${config.network.chainId.toString(16)}`,
          chainName: config.network.chainName,
          rpcUrls: [config.network.rpcUrl],
        }],
      })
    }
  }
}
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [API.md](API.md) - Submitting signed orders
- [COMPONENTS.md](COMPONENTS.md) - UI integration
