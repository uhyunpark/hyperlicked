package tests

import (
	"fmt"
	"os"
	"testing"

	"github.com/ethereum/go-ethereum/common"
	"github.com/uhyunpark/hyperlicked/pkg/app/core"
)

// newTestAccountManager creates an account manager with a temporary database
// Each test gets a unique database path to avoid Pebble lock conflicts
func newTestAccountManager(t *testing.T) *core.AccountManager {
	dbPath := fmt.Sprintf("./tmp_test_accounts_%s.db", t.Name())

	// Cleanup old database if exists
	os.RemoveAll(dbPath)

	// Register cleanup to remove database after test
	t.Cleanup(func() {
		os.RemoveAll(dbPath)
	})

	am, err := core.NewAccountManagerWithPath(dbPath)
	if err != nil {
		t.Fatalf("failed to create account manager: %v", err)
	}

	// Register cleanup to close database
	t.Cleanup(func() {
		am.Close()
	})

	return am
}

var (
	alice = common.HexToAddress("0xAA00000000000000000000000000000000000000")
	bob   = common.HexToAddress("0xBB00000000000000000000000000000000000000")
)

// TestAccountCreation tests basic account creation
func TestAccountCreation(t *testing.T) {
	acc := core.NewAccount(alice)

	if acc.Address != alice {
		t.Errorf("wrong address: got %s, want %s", acc.Address.Hex(), alice.Hex())
	}
	if acc.USDCBalance != 0 {
		t.Errorf("expected zero balance, got %d", acc.USDCBalance)
	}
	if acc.LockedCollateral != 0 {
		t.Errorf("expected zero locked collateral, got %d", acc.LockedCollateral)
	}
	if len(acc.Positions) != 0 {
		t.Errorf("expected no positions, got %d", len(acc.Positions))
	}
}

// TestAccountManagerDeposit tests deposit functionality
func TestAccountManagerDeposit(t *testing.T) {
	am := newTestAccountManager(t)

	// Deposit $1000 (100,000 cents)
	err := am.Deposit(alice, 100000)
	if err != nil {
		t.Fatalf("deposit failed: %v", err)
	}

	acc := am.GetAccount(alice)
	if acc.USDCBalance != 100000 {
		t.Errorf("balance = %d, want 100000", acc.USDCBalance)
	}
	if acc.AvailableBalance() != 100000 {
		t.Errorf("available = %d, want 100000", acc.AvailableBalance())
	}

	// Test invalid deposit
	err = am.Deposit(alice, -100)
	if err == nil {
		t.Error("expected error for negative deposit")
	}
}

// TestAccountManagerWithdraw tests withdrawal functionality
func TestAccountManagerWithdraw(t *testing.T) {
	am := newTestAccountManager(t)
	am.Deposit(alice, 100000) // $1000

	// Withdraw $500
	err := am.Withdraw(alice, 50000)
	if err != nil {
		t.Fatalf("withdraw failed: %v", err)
	}

	acc := am.GetAccount(alice)
	if acc.USDCBalance != 50000 {
		t.Errorf("balance = %d, want 50000", acc.USDCBalance)
	}

	// Test insufficient balance
	err = am.Withdraw(alice, 60000)
	if err == nil {
		t.Error("expected error for insufficient balance")
	}

	// Test nonexistent account
	err = am.Withdraw(bob, 100)
	if err == nil {
		t.Error("expected error for nonexistent account")
	}
}

// TestAccountManagerLockCollateral tests margin locking
func TestAccountManagerLockCollateral(t *testing.T) {
	am := newTestAccountManager(t)
	am.Deposit(alice, 100000) // $1000

	// Lock $200 for margin
	err := am.LockCollateral(alice, 20000)
	if err != nil {
		t.Fatalf("lock failed: %v", err)
	}

	acc := am.GetAccount(alice)
	if acc.LockedCollateral != 20000 {
		t.Errorf("locked = %d, want 20000", acc.LockedCollateral)
	}
	if acc.AvailableBalance() != 80000 {
		t.Errorf("available = %d, want 80000", acc.AvailableBalance())
	}

	// Lock more
	err = am.LockCollateral(alice, 30000)
	if err != nil {
		t.Fatalf("second lock failed: %v", err)
	}
	if acc.LockedCollateral != 50000 {
		t.Errorf("locked = %d, want 50000", acc.LockedCollateral)
	}

	// Test insufficient balance
	err = am.LockCollateral(alice, 60000)
	if err == nil {
		t.Error("expected error for insufficient balance")
	}
}

// TestAccountManagerUnlockCollateral tests margin unlocking
func TestAccountManagerUnlockCollateral(t *testing.T) {
	am := newTestAccountManager(t)
	am.Deposit(alice, 100000)
	am.LockCollateral(alice, 50000)

	// Unlock $200
	err := am.UnlockCollateral(alice, 20000)
	if err != nil {
		t.Fatalf("unlock failed: %v", err)
	}

	acc := am.GetAccount(alice)
	if acc.LockedCollateral != 30000 {
		t.Errorf("locked = %d, want 30000", acc.LockedCollateral)
	}
	if acc.AvailableBalance() != 70000 {
		t.Errorf("available = %d, want 70000", acc.AvailableBalance())
	}

	// Test unlock more than locked
	err = am.UnlockCollateral(alice, 40000)
	if err == nil {
		t.Error("expected error for unlocking more than locked")
	}
}

// TestAccountManagerUpdatePosition tests position updates
func TestAccountManagerUpdatePosition(t *testing.T) {
	am := newTestAccountManager(t)
	am.Deposit(alice, 100000) // $1000

	// Open long position: buy 100 lots (1 HYPL) at $50
	err := am.UpdatePosition(alice, "HYPL-USDC", 100, 50000, 10000) // sizeDelta=100, price=50000, margin=10000
	if err != nil {
		t.Fatalf("update position failed: %v", err)
	}

	acc := am.GetAccount(alice)
	pos := acc.GetPosition("HYPL-USDC")
	if pos == nil {
		t.Fatal("position not created")
	}
	if pos.Size != 100 {
		t.Errorf("size = %d, want 100", pos.Size)
	}
	if pos.EntryPrice != 50000 {
		t.Errorf("entry price = %d, want 50000", pos.EntryPrice)
	}
	if pos.Margin != 10000 {
		t.Errorf("margin = %d, want 10000", pos.Margin)
	}

	// Add to position: buy another 100 lots at $55
	// VWAP entry = (50000*100 + 55000*100) / 200 = 52500
	err = am.UpdatePosition(alice, "HYPL-USDC", 100, 55000, 10000)
	if err != nil {
		t.Fatalf("second update failed: %v", err)
	}
	if pos.Size != 200 {
		t.Errorf("size = %d, want 200", pos.Size)
	}
	if pos.EntryPrice != 52500 {
		t.Errorf("entry price = %d, want 52500 (VWAP)", pos.EntryPrice)
	}
	if pos.Margin != 20000 {
		t.Errorf("margin = %d, want 20000", pos.Margin)
	}
}

// TestAccountManagerPositionClose tests closing positions
func TestAccountManagerPositionClose(t *testing.T) {
	am := newTestAccountManager(t)
	am.Deposit(alice, 100000)

	// Open long: buy 100 lots at $50
	am.UpdatePosition(alice, "HYPL-USDC", 100, 50000, 10000)

	// Close position: sell 100 lots at $60
	// Realized PnL = (60000 - 50000) * 100 = 1,000,000 (in tick*lots)
	err := am.UpdatePosition(alice, "HYPL-USDC", -100, 60000, -10000)
	if err != nil {
		t.Fatalf("close position failed: %v", err)
	}

	acc := am.GetAccount(alice)
	pos := acc.GetPosition("HYPL-USDC")
	if pos.Size != 0 {
		t.Errorf("size = %d, want 0 (closed)", pos.Size)
	}
	if pos.Margin != 0 {
		t.Errorf("margin = %d, want 0 (closed)", pos.Margin)
	}
	if acc.RealizedPnL != 1000000 {
		t.Errorf("realized PnL = %d, want 1000000", acc.RealizedPnL)
	}
}

// TestAccountManagerShortPosition tests short positions
func TestAccountManagerShortPosition(t *testing.T) {
	am := newTestAccountManager(t)
	am.Deposit(alice, 100000)

	// Open short: sell 100 lots at $50
	err := am.UpdatePosition(alice, "HYPL-USDC", -100, 50000, 10000)
	if err != nil {
		t.Fatalf("open short failed: %v", err)
	}

	acc := am.GetAccount(alice)
	pos := acc.GetPosition("HYPL-USDC")
	if pos.Size != -100 {
		t.Errorf("size = %d, want -100", pos.Size)
	}
	if !pos.IsShort() {
		t.Error("expected IsShort() = true")
	}

	// Close short: buy 100 lots at $40 (profit)
	// Realized PnL = -(40000 - 50000) * 100 = 1,000,000 (price dropped, short profits)
	am.UpdatePosition(alice, "HYPL-USDC", 100, 40000, -10000)
	if acc.RealizedPnL != 1000000 {
		t.Errorf("realized PnL = %d, want 1000000 (short profit)", acc.RealizedPnL)
	}
}

// TestAccountManagerFees tests fee accounting
func TestAccountManagerFees(t *testing.T) {
	am := newTestAccountManager(t)
	am.Deposit(alice, 100000)

	// Pay taker fee: $5 (500 cents)
	err := am.ApplyFees(alice, -500)
	if err != nil {
		t.Fatalf("apply taker fee failed: %v", err)
	}

	acc := am.GetAccount(alice)
	if acc.USDCBalance != 99500 {
		t.Errorf("balance = %d, want 99500", acc.USDCBalance)
	}
	if acc.TotalFeesPaid != 500 {
		t.Errorf("fees paid = %d, want 500", acc.TotalFeesPaid)
	}

	// Earn maker rebate: $2 (200 cents)
	err = am.ApplyFees(alice, 200)
	if err != nil {
		t.Fatalf("apply maker rebate failed: %v", err)
	}
	if acc.USDCBalance != 99700 {
		t.Errorf("balance = %d, want 99700", acc.USDCBalance)
	}
	if acc.TotalFeesEarned != 200 {
		t.Errorf("fees earned = %d, want 200", acc.TotalFeesEarned)
	}
}

// TestAccountValidation tests account invariants
func TestAccountValidation(t *testing.T) {
	am := newTestAccountManager(t)

	// Valid account
	am.Deposit(alice, 100000)
	am.LockCollateral(alice, 20000)
	err := am.ValidateAccount(alice)
	if err != nil {
		t.Errorf("valid account failed validation: %v", err)
	}

	// Manually corrupt account (direct access for testing)
	acc := am.GetAccount(alice)
	acc.LockedCollateral = 150000 // More than balance
	err = am.ValidateAccount(alice)
	if err == nil {
		t.Error("expected validation error for locked > balance")
	}
}

// TestPositionHelpers tests position calculation methods
func TestPositionHelpers(t *testing.T) {
	pos := &core.Position{
		Symbol:     "HYPL-USDC",
		Size:       100,    // 1 HYPL long
		EntryPrice: 50000,  // $50 entry
		Margin:     10000,  // $100 margin
	}

	// Test unrealized PnL
	markPrice := int64(60000) // $60 mark price
	pnl := pos.UnrealizedPnL(markPrice)
	if pnl != 1000000 { // (60000 - 50000) * 100
		t.Errorf("unrealized PnL = %d, want 1000000", pnl)
	}

	// Test notional
	notional := pos.Notional(markPrice)
	if notional != 6000000 { // 60000 * 100
		t.Errorf("notional = %d, want 6000000", notional)
	}

	// Test leverage
	leverage := pos.Leverage(markPrice)
	if leverage != 600 { // 6000000 / 10000
		t.Errorf("leverage = %d, want 600", leverage)
	}

	// Test margin ratio
	marginRatio := pos.MarginRatio(markPrice)
	expectedRatio := int64(10000 * 10000 / 6000000) // ~16 bps
	if marginRatio != expectedRatio {
		t.Errorf("margin ratio = %d, want %d", marginRatio, expectedRatio)
	}

	// Test IsLong/IsShort
	if !pos.IsLong() {
		t.Error("expected IsLong() = true")
	}
	if pos.IsShort() {
		t.Error("expected IsShort() = false")
	}
}

// TestAccountTotalEquity tests equity calculation with unrealized PnL
func TestAccountTotalEquity(t *testing.T) {
	acc := core.NewAccount(alice)
	acc.USDCBalance = 100000 // $1000

	// Open long position
	pos := &core.Position{
		Symbol:     "HYPL-USDC",
		Size:       100,
		EntryPrice: 50000,
		Margin:     10000,
	}
	acc.Positions["HYPL-USDC"] = pos

	// Mark price = $60 (profit of $10 per lot)
	markPrices := map[string]int64{
		"HYPL-USDC": 60000,
	}

	equity := acc.TotalEquity(markPrices)
	// Equity = Balance + UnrealizedPnL
	// = 100000 + (60000 - 50000) * 100
	// = 100000 + 1000000 = 1100000
	if equity != 1100000 {
		t.Errorf("total equity = %d, want 1100000", equity)
	}

	// Test with mark price at loss
	markPrices["HYPL-USDC"] = 40000 // $40 (loss of $10 per lot)
	equity = acc.TotalEquity(markPrices)
	// = 100000 + (40000 - 50000) * 100
	// = 100000 - 1000000 = -900000
	if equity != -900000 {
		t.Errorf("total equity = %d, want -900000", equity)
	}
}
