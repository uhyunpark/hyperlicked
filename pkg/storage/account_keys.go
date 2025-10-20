package storage

import (
	"fmt"

	"github.com/ethereum/go-ethereum/common"
)

// Account key schema for Pebble storage
// Uses different prefixes than consensus keys to avoid collisions:
//
// Consensus keys (existing):
//   b:<hash>     → Block
//   c:<view>     → Certificate
//   cm           → Committed hash
//
// Account keys (new):
//   acc:<address>          → Account
//   pos:<address>:<symbol> → Position
//   ord:<address>:<orderID> → Order
//   trade:<symbol>:<timestamp>:<id> → Trade

// Key prefixes
const (
	prefixAccount  = "acc:"
	prefixPosition = "pos:"
	prefixOrder    = "ord:"
	prefixTrade    = "trade:"
)

// accountKey returns the key for an account
// Format: "acc:{address}"
func accountKey(addr common.Address) []byte {
	return []byte(fmt.Sprintf("%s%s", prefixAccount, addr.Hex()))
}

// positionKey returns the key for a position
// Format: "pos:{address}:{symbol}"
func positionKey(addr common.Address, symbol string) []byte {
	return []byte(fmt.Sprintf("%s%s:%s", prefixPosition, addr.Hex(), symbol))
}

// positionPrefix returns the prefix for all positions of an account
// Format: "pos:{address}:"
func positionPrefix(addr common.Address) []byte {
	return []byte(fmt.Sprintf("%s%s:", prefixPosition, addr.Hex()))
}

// orderKey returns the key for an order
// Format: "ord:{address}:{orderID}"
func orderKey(addr common.Address, orderID string) []byte {
	return []byte(fmt.Sprintf("%s%s:%s", prefixOrder, addr.Hex(), orderID))
}

// orderPrefix returns the prefix for all orders of an account
// Format: "ord:{address}:"
func orderPrefix(addr common.Address) []byte {
	return []byte(fmt.Sprintf("%s%s:", prefixOrder, addr.Hex()))
}

// tradeKey returns the key for a trade
// Format: "trade:{symbol}:{timestamp}:{tradeID}"
// Timestamp is zero-padded (20 digits) for lexicographic sorting
func tradeKey(symbol string, timestamp int64, tradeID string) []byte {
	return []byte(fmt.Sprintf("%s%s:%020d:%s", prefixTrade, symbol, timestamp, tradeID))
}

// tradePrefix returns the prefix for all trades of a symbol
// Format: "trade:{symbol}:"
func tradePrefix(symbol string) []byte {
	return []byte(fmt.Sprintf("%s%s:", prefixTrade, symbol))
}

// keyUpperBound returns the exclusive upper bound for a prefix scan
func keyUpperBound(prefix []byte) []byte {
	bound := make([]byte, len(prefix))
	copy(bound, prefix)
	bound[len(bound)-1]++
	return bound
}
