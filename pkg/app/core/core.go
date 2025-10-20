// Package core provides backward compatibility wrappers for reorganized subpackages
package core

import (
	"fmt"

	"github.com/ethereum/go-ethereum/common"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/account"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/market"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/mempool"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/orderbook"
)

// Re-export types from subpackages for backward compatibility

// From orderbook package
type (
	Side       = orderbook.Side
	Order      = orderbook.Order
	Fill       = orderbook.Fill
	PriceLevel = orderbook.PriceLevel
	OrderBook  = orderbook.OrderBook
)

const (
	Buy  = orderbook.Buy
	Sell = orderbook.Sell
)

func NewOrderBook() *OrderBook {
	return orderbook.NewOrderBook()
}

// From account package
type (
	Account        = account.Account
	Position       = account.Position
	AccountManager = account.AccountManager
)

func NewAccount(addr common.Address) *Account {
	return account.NewAccount(addr)
}

func NewAccountManager() *AccountManager {
	// Use default database path for backward compatibility
	// Production code should use NewAccountManagerWithPath() with explicit path
	am, err := account.NewAccountManager("./data/accounts.db")
	if err != nil {
		panic(fmt.Sprintf("failed to create account manager: %v", err))
	}
	return am
}

// NewAccountManagerWithPath creates an account manager with custom database path
func NewAccountManagerWithPath(dbPath string) (*AccountManager, error) {
	return account.NewAccountManager(dbPath)
}

// From market package
type (
	Market         = market.Market
	MarketParams   = market.MarketParams
	MarketRegistry = market.MarketRegistry
)

func NewMarket(symbol, baseAsset, quoteAsset string, params market.MarketParams) (*Market, error) {
	return market.NewMarket(symbol, baseAsset, quoteAsset, params)
}

func NewMarketWithDefaults(symbol, baseAsset, quoteAsset string) (*Market, error) {
	return market.NewMarketWithDefaults(symbol, baseAsset, quoteAsset)
}

func NewMarketRegistry() *MarketRegistry {
	return market.NewMarketRegistry()
}

// DefaultMarketParams returns default market parameters
var DefaultMarketParams = market.DefaultHYPLUSDC

// From mempool package
type Mempool = mempool.Mempool

func NewMempool() *Mempool {
	return mempool.NewMempool()
}

// From transaction package (new)
// Import not needed yet - will add when transaction package is used
