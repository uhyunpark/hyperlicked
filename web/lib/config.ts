// Frontend configuration loaded from environment variables

export const config = {
  // API endpoints
  api: {
    baseUrl: process.env.NEXT_PUBLIC_API_URL || 'http://localhost:8080/api/v1',
    wsUrl: process.env.NEXT_PUBLIC_WS_URL || 'ws://localhost:8080/ws'
  },

  // Blockchain network
  network: {
    chainId: parseInt(process.env.NEXT_PUBLIC_CHAIN_ID || '1337'),
    chainName: process.env.NEXT_PUBLIC_CHAIN_NAME || 'HyperLicked Devnet',
    rpcUrl: process.env.NEXT_PUBLIC_RPC_URL || 'http://localhost:8545',
    blockExplorerUrl: process.env.NEXT_PUBLIC_BLOCK_EXPLORER_URL || ''
  },

  // Currency details
  currency: {
    name: process.env.NEXT_PUBLIC_CURRENCY_NAME || 'Ether',
    symbol: process.env.NEXT_PUBLIC_CURRENCY_SYMBOL || 'ETH',
    decimals: parseInt(process.env.NEXT_PUBLIC_CURRENCY_DECIMALS || '18')
  },

  // EIP-712 domain
  eip712: {
    name: process.env.NEXT_PUBLIC_EIP712_NAME || 'HyperLicked',
    version: process.env.NEXT_PUBLIC_EIP712_VERSION || '1',
    chainId: parseInt(process.env.NEXT_PUBLIC_CHAIN_ID || '1337'),
    verifyingContract: process.env.NEXT_PUBLIC_VERIFYING_CONTRACT || '0x0000000000000000000000000000000000000000'
  }
} as const

// Helper to check if we're in development mode
export const isDevelopment = process.env.NODE_ENV === 'development'
export const isProduction = process.env.NODE_ENV === 'production'

// Log configuration on load (development only)
if (typeof window !== 'undefined' && isDevelopment) {
  console.log('[config] Frontend configuration loaded:')
  console.log('[config] Chain ID:', config.network.chainId)
  console.log('[config] RPC URL:', config.network.rpcUrl)
  console.log('[config] API URL:', config.api.baseUrl)
  console.log('[config] WS URL:', config.api.wsUrl)
}
