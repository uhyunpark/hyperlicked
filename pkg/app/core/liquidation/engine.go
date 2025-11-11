package liquidation

import (
	"log"

	"github.com/ethereum/go-ethereum/common"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/account"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/market"
)

// Oracle provides mark prices for liquidation checks
type Oracle interface {
	GetMarkPrices() map[string]int64
}

// Engine checks and executes liquidations for underwater accounts
// Called after each block to ensure all positions remain solvent
type Engine struct {
	accountMgr     *account.AccountManager
	markets        map[string]*market.Market
	oracle         Oracle
	insuranceFund  int64 // Total deficit absorbed by insurance fund
}

// NewEngine creates a liquidation engine
func NewEngine(accountMgr *account.AccountManager, markets map[string]*market.Market, oracle Oracle) *Engine {
	return &Engine{
		accountMgr:    accountMgr,
		markets:       markets,
		oracle:        oracle,
		insuranceFund: 0,
	}
}

// CheckAll checks all accounts for liquidation and executes if necessary
// Called at the end of each block (after fills, before state hash)
// Uses activePositions set for efficiency (only checks accounts with open positions)
func (le *Engine) CheckAll(height int64) {
	accounts := le.accountMgr.ListActiveAccounts() // Only accounts with positions
	markPrices := le.oracle.GetMarkPrices()

	liquidationCount := 0
	totalDeficit := int64(0)

	for _, acc := range accounts {

		// Check if account should be liquidated
		shouldLiquidate, equity, mmr, err := le.accountMgr.CheckLiquidation(
			acc.Address, le.markets, markPrices,
		)
		if err != nil {
			log.Printf("[liquidation] error checking %s: %v", acc.Address.Hex(), err)
			continue
		}

		// Liquidate if underwater
		if shouldLiquidate {
			finalBalance, deficit, err := le.accountMgr.Liquidate(
				acc.Address, le.markets, markPrices,
			)
			if err != nil {
				log.Printf("[liquidation] error liquidating %s: %v", acc.Address.Hex(), err)
				continue
			}

			log.Printf("[liquidation] h=%d addr=%s equity=%d mmr=%d deficit=%d final=%d",
				height, formatAddress(acc.Address), equity, mmr, deficit, finalBalance)

			// Transfer deficit to insurance fund
			if deficit > 0 {
				le.insuranceFund += deficit
				totalDeficit += deficit
			}

			liquidationCount++
		}
	}

	// Log summary if any liquidations occurred
	if liquidationCount > 0 {
		log.Printf("[liquidation] h=%d total_liquidations=%d total_deficit=%d insurance_fund=%d",
			height, liquidationCount, totalDeficit, le.insuranceFund)
	}
}

// GetInsuranceFund returns the total insurance fund balance
func (le *Engine) GetInsuranceFund() int64 {
	return le.insuranceFund
}

// formatAddress shortens address for logging
func formatAddress(addr common.Address) string {
	hex := addr.Hex()
	if len(hex) > 10 {
		return hex[:6] + "..." + hex[len(hex)-4:]
	}
	return hex
}
