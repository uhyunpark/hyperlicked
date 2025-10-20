package account

import (
	"fmt"

	"github.com/ethereum/go-ethereum/common"
)

// Pebble key schema for efficient queries
// Design principles:
// 1. Prefix-based for range scans (get all orders for account)
// 2. Lexicographic ordering for time-based queries
// 3. Account address as primary key for ownership

// Key prefixes
const (
	prefixAccount  = "acc:"  // Account state
	prefixPosition = "pos:"  // Position state
	prefixOrder    = "ord:"  // Order state
	prefixTrade    = "trade:" // Trade history
	prefixNonce    = "nonce:" // Account nonce (separate for fast lookup)
)

// accountKey returns the key for an account
// Format: "acc:{address}"
// Example: "acc:0x742d35cc6634c0532925a3b844bc9e7595f0beb"
func accountKey(addr common.Address) []byte {
	return []byte(fmt.Sprintf("%s%s", prefixAccount, addr.Hex()))
}

// nonceKey returns the key for account nonce (separate for atomic increment)
// Format: "nonce:{address}"
func nonceKey(addr common.Address) []byte {
	return []byte(fmt.Sprintf("%s%s", prefixNonce, addr.Hex()))
}

// positionKey returns the key for a position
// Format: "pos:{address}:{symbol}"
// Example: "pos:0x742d35cc...:HYPL-USDC"
func positionKey(addr common.Address, symbol string) []byte {
	return []byte(fmt.Sprintf("%s%s:%s", prefixPosition, addr.Hex(), symbol))
}

// positionPrefix returns the prefix for all positions of an account
// Used for range queries: get all positions for account
// Format: "pos:{address}:"
func positionPrefix(addr common.Address) []byte {
	return []byte(fmt.Sprintf("%s%s:", prefixPosition, addr.Hex()))
}

// orderKey returns the key for an order
// Format: "ord:{address}:{orderID}"
// Example: "ord:0x742d35cc...:0x1234-ord-1234567890"
func orderKey(addr common.Address, orderID string) []byte {
	return []byte(fmt.Sprintf("%s%s:%s", prefixOrder, addr.Hex(), orderID))
}

// orderPrefix returns the prefix for all orders of an account
// Used for range queries: get all orders for account
// Format: "ord:{address}:"
func orderPrefix(addr common.Address) []byte {
	return []byte(fmt.Sprintf("%s%s:", prefixOrder, addr.Hex()))
}

// tradeKey returns the key for a trade
// Format: "trade:{symbol}:{timestamp}:{tradeID}"
// Example: "trade:HYPL-USDC:0000001730000000000:trade-123"
// Note: Timestamp is zero-padded (20 digits) for lexicographic sorting
func tradeKey(symbol string, timestamp int64, tradeID string) []byte {
	return []byte(fmt.Sprintf("%s%s:%020d:%s", prefixTrade, symbol, timestamp, tradeID))
}

// tradePrefix returns the prefix for all trades of a symbol
// Used for range queries: get recent trades for symbol
// Format: "trade:{symbol}:"
func tradePrefix(symbol string) []byte {
	return []byte(fmt.Sprintf("%s%s:", prefixTrade, symbol))
}

// tradePrefixAll returns the prefix for ALL trades (across all symbols)
// Used for range queries: get global trade history
// Format: "trade:"
func tradePrefixAll() []byte {
	return []byte(prefixTrade)
}

// keyUpperBound returns the exclusive upper bound for a prefix scan
// Example: prefix "acc:0x123:" -> upper bound "acc:0x123;" (next byte after ':')
// This ensures the iterator stops at the right boundary
func keyUpperBound(prefix []byte) []byte {
	// Create a copy and increment the last byte
	bound := make([]byte, len(prefix))
	copy(bound, prefix)
	bound[len(bound)-1]++
	return bound
}

// accountKeyFromBytes extracts the address from an account key
// Inverse of accountKey() - used for parsing iterator keys
func accountKeyFromBytes(key []byte) (common.Address, error) {
	// Remove prefix "acc:"
	if len(key) < len(prefixAccount)+42 { // 42 = "0x" + 40 hex chars
		return common.Address{}, fmt.Errorf("invalid account key length: %d", len(key))
	}
	addrHex := string(key[len(prefixAccount):])
	if !common.IsHexAddress(addrHex) {
		return common.Address{}, fmt.Errorf("invalid address in key: %s", addrHex)
	}
	return common.HexToAddress(addrHex), nil
}
