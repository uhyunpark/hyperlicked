package main

import (
	"encoding/json"
	"fmt"
	"math/big"
	"os"

	"github.com/uhyunpark/hyperlicked/pkg/app/core/transaction"
	"github.com/uhyunpark/hyperlicked/pkg/crypto"
)

func main() {
	// Step 1: Generate or load key
	fmt.Println("Generating new keypair...")
	signer, err := crypto.GenerateKey()
	if err != nil {
		fmt.Printf("Error: %v\n", err)
		os.Exit(1)
	}

	fmt.Printf("Address: %s\n", signer.Address().Hex())
	fmt.Printf("Private Key: %s (KEEP SECRET!)\n\n", signer.PrivateKeyHex())

	// Step 2: Create order
	order := &crypto.OrderEIP712{
		Symbol:   "BTC-USDT",
		Side:     1, // Buy
		Type:     1, // GTC
		Price:    big.NewInt(50000),
		Qty:      big.NewInt(100),
		Nonce:    big.NewInt(1),
		Deadline: big.NewInt(0), // No expiry
		Leverage: 10,
		Owner:    signer.Address(),
	}

	fmt.Println("Order Details:")
	fmt.Printf("  Symbol: %s\n", order.Symbol)
	fmt.Printf("  Side: %s\n", crypto.Uint8ToSide(order.Side))
	fmt.Printf("  Type: %s\n", crypto.Uint8ToOrderType(order.Type))
	fmt.Printf("  Price: %s\n", order.Price.String())
	fmt.Printf("  Qty: %s\n", order.Qty.String())
	fmt.Printf("  Leverage: %dx\n", order.Leverage)
	fmt.Printf("  Owner: %s\n\n", order.Owner.Hex())

	// Step 3: Sign order with EIP-712
	eip712Signer := crypto.NewEIP712Signer(crypto.DefaultDomain())
	signature, err := eip712Signer.SignOrder(signer, order)
	if err != nil {
		fmt.Printf("Error signing: %v\n", err)
		os.Exit(1)
	}

	fmt.Printf("Signature: 0x%x\n\n", signature)

	// Step 4: Create signed transaction
	orderPayload := transaction.FromEIP712Order(order)
	signedTx := &transaction.SignedTransaction{
		Type:      transaction.TxTypeOrder,
		Order:     orderPayload,
		Signature: fmt.Sprintf("0x%x", signature),
	}

	// Step 5: Serialize to JSON
	txJSON, err := json.MarshalIndent(signedTx, "", "  ")
	if err != nil {
		fmt.Printf("Error marshaling JSON: %v\n", err)
		os.Exit(1)
	}

	fmt.Println("Signed Transaction (JSON):")
	fmt.Println(string(txJSON))
	fmt.Println()

	// Step 6: Verify signature
	fmt.Println("Verifying signature...")
	verifier := transaction.NewVerifier(crypto.DefaultDomain())
	recoveredOwner, valid, err := verifier.VerifyOrderTransaction(signedTx)
	if err != nil {
		fmt.Printf("Error verifying: %v\n", err)
		os.Exit(1)
	}

	if !valid {
		fmt.Println("✗ Signature INVALID")
		os.Exit(1)
	}

	fmt.Println("✓ Signature VALID")
	fmt.Printf("  Signer: %s\n", recoveredOwner.Hex())
	fmt.Printf("  Matches owner: %v\n\n", recoveredOwner == order.Owner)

	// Step 7: Show how to submit to API
	fmt.Println("To submit this order to HyperLicked:")
	fmt.Println("  POST http://localhost:8080/api/v1/orders")
	fmt.Println("  Content-Type: application/json")
	fmt.Println("  Body:")
	fmt.Println(string(txJSON))
}
