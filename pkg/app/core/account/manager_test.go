package account

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/ethereum/go-ethereum/common"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestActivePositions(t *testing.T) {
	// Create temporary db directory
	tmpDir := filepath.Join(os.TempDir(), "test-accounts-active-positions")
	defer os.RemoveAll(tmpDir)

	am, err := NewAccountManager(tmpDir)
	require.NoError(t, err)
	defer am.Close()

	addr1 := common.HexToAddress("0x1111111111111111111111111111111111111111")
	addr2 := common.HexToAddress("0x2222222222222222222222222222222222222222")

	// Initially no active accounts
	assert.Equal(t, 0, len(am.ListActiveAccounts()))

	// Create accounts
	acc1 := am.GetAccount(addr1)
	acc2 := am.GetAccount(addr2)
	acc1.USDCBalance = 100000 // $1000
	acc2.USDCBalance = 100000

	// Still no active accounts (no positions yet)
	assert.Equal(t, 0, len(am.ListActiveAccounts()))

	// Add position to account 1
	err = am.UpdatePosition(addr1, "BTC-USDC", 100, 5000000, 1000) // Long 100 @ $50,000
	require.NoError(t, err)

	// Now 1 active account
	activeAccounts := am.ListActiveAccounts()
	assert.Equal(t, 1, len(activeAccounts))
	assert.Equal(t, addr1, activeAccounts[0].Address)

	// Add position to account 2
	err = am.UpdatePosition(addr2, "ETH-USDC", -50, 200000, 500) // Short 50 @ $2,000
	require.NoError(t, err)

	// Now 2 active accounts
	activeAccounts = am.ListActiveAccounts()
	assert.Equal(t, 2, len(activeAccounts))

	// Close account 1's position
	err = am.UpdatePosition(addr1, "BTC-USDC", -100, 5100000, -1000) // Close long @ $51,000
	require.NoError(t, err)

	// Now only 1 active account (addr2)
	activeAccounts = am.ListActiveAccounts()
	assert.Equal(t, 1, len(activeAccounts))
	assert.Equal(t, addr2, activeAccounts[0].Address)

	// Account 1 should have realized PnL
	acc1 = am.GetAccount(addr1)
	expectedPnL := (int64(5100000) - int64(5000000)) * int64(100) // (exit - entry) * size
	assert.Equal(t, expectedPnL, acc1.RealizedPnL)

	// Close account 2's position
	err = am.UpdatePosition(addr2, "ETH-USDC", 50, 190000, -500) // Cover short @ $1,900
	require.NoError(t, err)

	// Now 0 active accounts
	activeAccounts = am.ListActiveAccounts()
	assert.Equal(t, 0, len(activeAccounts))

	// Account 2 should have realized PnL (profit from short)
	acc2 = am.GetAccount(addr2)
	// For short: profit when price drops
	// Entry at 200000, exit at 190000, size=-50
	// Realized PnL calculation in UpdatePosition for opposite direction (line 283-287)
	// closedSize = 50, realizedPnL = (190000 - 200000) * 50 = -500000
	// But flipped because oldSize < 0, so realizedPnL = -(-500000) = 500000
	expectedPnL = int64(500000) // $5,000 profit
	assert.Equal(t, expectedPnL, acc2.RealizedPnL)
}

func TestActivePositionsMultipleSymbols(t *testing.T) {
	tmpDir := filepath.Join(os.TempDir(), "test-accounts-multi-symbol")
	defer os.RemoveAll(tmpDir)

	am, err := NewAccountManager(tmpDir)
	require.NoError(t, err)
	defer am.Close()

	addr := common.HexToAddress("0x3333333333333333333333333333333333333333")
	acc := am.GetAccount(addr)
	acc.USDCBalance = 1000000 // $10,000

	// No active accounts initially
	assert.Equal(t, 0, len(am.ListActiveAccounts()))

	// Open BTC position
	err = am.UpdatePosition(addr, "BTC-USDC", 100, 5000000, 1000)
	require.NoError(t, err)

	// 1 active account
	assert.Equal(t, 1, len(am.ListActiveAccounts()))

	// Open ETH position
	err = am.UpdatePosition(addr, "ETH-USDC", 50, 200000, 500)
	require.NoError(t, err)

	// Still 1 active account (same address)
	assert.Equal(t, 1, len(am.ListActiveAccounts()))

	// Close BTC position
	err = am.UpdatePosition(addr, "BTC-USDC", -100, 5100000, -1000)
	require.NoError(t, err)

	// Still 1 active account (ETH position remains)
	activeAccounts := am.ListActiveAccounts()
	assert.Equal(t, 1, len(activeAccounts))

	// Verify only ETH position is active (BTC position exists in map but Size=0)
	btcPos := activeAccounts[0].GetPosition("BTC-USDC")
	ethPos := activeAccounts[0].GetPosition("ETH-USDC")
	if btcPos != nil {
		assert.Equal(t, int64(0), btcPos.Size)
	}
	assert.NotNil(t, ethPos)
	assert.NotEqual(t, int64(0), ethPos.Size)

	// Close ETH position
	err = am.UpdatePosition(addr, "ETH-USDC", -50, 210000, -500)
	require.NoError(t, err)

	// Now 0 active accounts
	assert.Equal(t, 0, len(am.ListActiveAccounts()))
}

func TestActivePositionsAfterLiquidation(t *testing.T) {
	tmpDir := filepath.Join(os.TempDir(), "test-accounts-liquidation")
	defer os.RemoveAll(tmpDir)

	am, err := NewAccountManager(tmpDir)
	require.NoError(t, err)
	defer am.Close()

	addr := common.HexToAddress("0x4444444444444444444444444444444444444444")
	acc := am.GetAccount(addr)
	acc.USDCBalance = 10000 // $100

	// Open position
	err = am.UpdatePosition(addr, "BTC-USDC", 100, 5000000, 1000)
	require.NoError(t, err)

	// 1 active account
	assert.Equal(t, 1, len(am.ListActiveAccounts()))

	// Simulate liquidation (price drops to $40,000)
	markPrices := map[string]int64{
		"BTC-USDC": 4000000, // $40,000
	}

	finalBalance, deficit, err := am.Liquidate(addr, nil, markPrices)
	require.NoError(t, err)

	// Account should be removed from active positions
	assert.Equal(t, 0, len(am.ListActiveAccounts()))

	// Verify liquidation results
	acc = am.GetAccount(addr)
	for _, pos := range acc.Positions {
		assert.Equal(t, int64(0), pos.Size, "Position should be closed after liquidation")
	}

	t.Logf("Final balance: %d, Deficit: %d", finalBalance, deficit)
}

func TestListAccountsVsListActiveAccounts(t *testing.T) {
	tmpDir := filepath.Join(os.TempDir(), "test-accounts-comparison")
	defer os.RemoveAll(tmpDir)

	am, err := NewAccountManager(tmpDir)
	require.NoError(t, err)
	defer am.Close()

	// Create 10 accounts with different addresses
	addresses := make([]common.Address, 10)
	for i := 0; i < 10; i++ {
		addr := common.Address{}
		addr[0] = byte(i + 1) // Start from 1 to avoid zero address
		addresses[i] = addr

		acc := am.GetAccount(addr)
		acc.USDCBalance = 100000
	}

	// All accounts exist but none have positions
	assert.Equal(t, 10, len(am.ListAccounts()))
	assert.Equal(t, 0, len(am.ListActiveAccounts()))

	// Give 3 accounts positions
	for i := 0; i < 3; i++ {
		err = am.UpdatePosition(addresses[i], "BTC-USDC", 100, 5000000, 1000)
		require.NoError(t, err)
	}

	// Still 10 total accounts, but only 3 active
	assert.Equal(t, 10, len(am.ListAccounts()))
	assert.Equal(t, 3, len(am.ListActiveAccounts()))
}
