package tests

import (
	"encoding/json"
	"fmt"
	"math/big"
	"testing"

	"github.com/uhyunpark/hyperlicked/pkg/app/core/transaction"
	"github.com/uhyunpark/hyperlicked/pkg/app/perp"
	"github.com/uhyunpark/hyperlicked/pkg/crypto"
)

// TestSignedOrderExecution tests end-to-end signed order flow
func TestSignedOrderExecution(t *testing.T) {
	// Generate signer
	signer, _ := crypto.GenerateKey()
	address := signer.Address()

	t.Logf("Generated test address: %s", address.Hex())

	// Note: For this test, we're just testing transaction parsing and signature verification
	// Full execution test would require account funding and more complex setup

	// Create signed order
	order := &crypto.OrderEIP712{
		Symbol:   "BTC-USDT",
		Side:     1, // Buy
		Type:     1, // GTC
		Price:    big.NewInt(50000),
		Qty:      big.NewInt(10), // Small qty to avoid margin issues
		Nonce:    big.NewInt(1),
		Deadline: big.NewInt(0),
		Leverage: 10,
		Owner:    address,
	}

	// Sign order
	eip712Signer := crypto.NewEIP712Signer(crypto.DefaultDomain())
	signature, err := eip712Signer.SignOrder(signer, order)
	if err != nil {
		t.Fatalf("failed to sign order: %v", err)
	}

	// Create signed transaction
	orderPayload := transaction.FromEIP712Order(order)
	signedTx := &transaction.SignedTransaction{
		Type:      transaction.TxTypeOrder,
		Order:     orderPayload,
		Signature: fmt.Sprintf("0x%x", signature),
	}

	// Serialize to JSON
	txJSON, err := json.Marshal(signedTx)
	if err != nil {
		t.Fatalf("failed to marshal tx: %v", err)
	}

	t.Logf("Signed transaction: %s", string(txJSON))

	// TODO: Test execution via FinalizeBlock
	// For now, just verify the transaction parses correctly
	parsed, err := transaction.ParseTransaction(txJSON)
	if err != nil {
		t.Fatalf("failed to parse transaction: %v", err)
	}

	if parsed.Type != transaction.TxTypeOrder {
		t.Errorf("wrong type: got %s, want %s", parsed.Type, transaction.TxTypeOrder)
	}

	// Verify signature
	verifier := transaction.NewVerifier(crypto.DefaultDomain())
	owner, valid, err := verifier.VerifyOrderTransaction(parsed)
	if err != nil {
		t.Fatalf("verification failed: %v", err)
	}

	if !valid {
		t.Error("signature verification failed")
	}

	if owner != address {
		t.Errorf("owner mismatch: got %s, want %s", owner.Hex(), address.Hex())
	}

	t.Logf("✓ Signature verified successfully")
}

// TestSignedTxGenerator tests the signed transaction generator
func TestSignedTxGenerator(t *testing.T) {
	gen := perp.NewSignedTxGenerator(5, []string{"BTC-USDT"})

	// Generate 10 signed orders
	for i := 0; i < 10; i++ {
		txBytes := gen.GenerateSignedOrder()

		// Parse transaction
		tx, err := transaction.ParseTransaction(txBytes)
		if err != nil {
			t.Fatalf("failed to parse generated tx: %v", err)
		}

		// Verify signature
		verifier := transaction.NewVerifier(crypto.DefaultDomain())
		_, valid, err := verifier.VerifyOrderTransaction(tx)
		if err != nil {
			t.Fatalf("verification failed for generated tx: %v", err)
		}

		if !valid {
			t.Errorf("generated transaction %d has invalid signature", i)
		}
	}

	t.Logf("✓ Generated and verified 10 signed transactions")
}

// TestLegacyTransactionBackwardCompat tests that legacy string format still works
func TestLegacyTransactionBackwardCompat(t *testing.T) {
	legacyTx := []byte("O:GTC:BTC-USDT:BUY:price=50000:qty=100:id=test_o1")

	// Parse as transaction
	tx, err := transaction.ParseTransaction(legacyTx)
	if err != nil {
		t.Fatalf("failed to parse legacy tx: %v", err)
	}

	if tx.Type != transaction.TxTypeLegacy {
		t.Errorf("expected legacy type, got %s", tx.Type)
	}

	t.Logf("✓ Legacy transaction format still supported")
}
