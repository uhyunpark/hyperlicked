package crypto

import (
	"fmt"
	"math/big"

	"github.com/ethereum/go-ethereum/common"
)

// ExampleSignOrder demonstrates how to sign an order with EIP-712
func ExampleSignOrder() {
	// Step 1: Generate a key pair (or load from private key)
	signer, err := GenerateKey()
	if err != nil {
		panic(err)
	}

	fmt.Printf("Generated address: %s\n", signer.Address().Hex())
	fmt.Printf("Private key: %s (KEEP SECRET!)\n\n", signer.PrivateKeyHex())

	// Step 2: Create EIP-712 signer with domain
	domain := DefaultDomain()
	eip712Signer := NewEIP712Signer(domain)

	// Step 3: Create an order
	order := &OrderEIP712{
		Symbol:   "BTC-USDT",
		Side:     1, // Buy
		Type:     1, // GTC
		Price:    big.NewInt(50000), // $50,000 in ticks
		Qty:      big.NewInt(100),   // 100 lots
		Nonce:    big.NewInt(1),
		Deadline: big.NewInt(0), // No expiry
		Leverage: 10,            // 10x leverage
		Owner:    signer.Address(),
	}

	// Step 4: Sign the order
	signature, err := eip712Signer.SignOrder(signer, order)
	if err != nil {
		panic(err)
	}

	fmt.Printf("Order signed!\n")
	fmt.Printf("Signature: 0x%x\n\n", signature)

	// Step 5: Verify the signature
	valid, err := eip712Signer.VerifyOrderSignature(order, signature)
	if err != nil {
		panic(err)
	}

	fmt.Printf("Signature valid: %v\n", valid)

	// Step 6: Recover signer address from signature
	recoveredAddr, err := eip712Signer.RecoverOrderSigner(order, signature)
	if err != nil {
		panic(err)
	}

	fmt.Printf("Recovered address: %s\n", recoveredAddr.Hex())
	fmt.Printf("Matches original: %v\n\n", recoveredAddr == signer.Address())

	// Step 7: Get JSON for MetaMask signing
	json, err := eip712Signer.OrderToJSON(order)
	if err != nil {
		panic(err)
	}

	fmt.Printf("EIP-712 JSON for MetaMask:\n%s\n", json)
}

// ExampleVerifyTransaction demonstrates how to verify a signed transaction
func ExampleVerifyTransaction() {
	// Scenario: User submits a signed order to the API
	// API needs to verify the signature before accepting

	// User's order data
	userAddress := common.HexToAddress("0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0")
	order := &OrderEIP712{
		Symbol:   "ETH-USDT",
		Side:     2, // Sell
		Type:     1, // GTC
		Price:    big.NewInt(3000),
		Qty:      big.NewInt(50),
		Nonce:    big.NewInt(42),
		Deadline: big.NewInt(1735689600), // Some future timestamp
		Leverage: 5,
		Owner:    userAddress,
	}

	// Simulate signing (in real scenario, user signs in wallet)
	signer, _ := GenerateKey()
	eip712Signer := NewEIP712Signer(DefaultDomain())
	signature, _ := eip712Signer.SignOrder(signer, order)

	// API receives: order + signature
	// API verifies:
	fmt.Println("API: Verifying order signature...")

	// Method 1: Verify against claimed owner
	valid, err := eip712Signer.VerifyOrderSignature(order, signature)
	if err != nil {
		fmt.Printf("Verification error: %v\n", err)
		return
	}

	if !valid {
		fmt.Println("REJECTED: Signature does not match claimed owner")
		return
	}

	// Method 2: Recover signer and check
	recoveredAddr, err := eip712Signer.RecoverOrderSigner(order, signature)
	if err != nil {
		fmt.Printf("Recovery error: %v\n", err)
		return
	}

	if recoveredAddr != order.Owner {
		fmt.Printf("REJECTED: Recovered signer %s != claimed owner %s\n",
			recoveredAddr.Hex(), order.Owner.Hex())
		return
	}

	fmt.Println("✓ Signature valid! Order accepted.")
	fmt.Printf("  Signer: %s\n", recoveredAddr.Hex())
	fmt.Printf("  Symbol: %s\n", order.Symbol)
	fmt.Printf("  Side: %s\n", Uint8ToSide(order.Side))
	fmt.Printf("  Price: %s\n", order.Price.String())
	fmt.Printf("  Qty: %s\n", order.Qty.String())
}

// ExampleReplayProtection demonstrates nonce-based replay protection
func ExampleReplayProtection() {
	signer, _ := GenerateKey()
	eip712Signer := NewEIP712Signer(DefaultDomain())

	// User submits order with nonce 1
	order1 := &OrderEIP712{
		Symbol:   "BTC-USDT",
		Side:     1,
		Type:     1,
		Price:    big.NewInt(50000),
		Qty:      big.NewInt(100),
		Nonce:    big.NewInt(1), // First nonce
		Deadline: big.NewInt(0),
		Leverage: 10,
		Owner:    signer.Address(),
	}

	sig1, _ := eip712Signer.SignOrder(signer, order1)

	// Attacker tries to replay the same signed order
	// (in real scenario, would need to check if nonce was already used)

	// Application tracks used nonces per address
	usedNonces := make(map[common.Address]map[uint64]bool)
	usedNonces[signer.Address()] = make(map[uint64]bool)

	// Process order 1
	fmt.Println("Processing order with nonce 1...")
	if usedNonces[signer.Address()][order1.Nonce.Uint64()] {
		fmt.Println("REJECTED: Nonce already used (replay attack)")
	} else {
		valid, _ := eip712Signer.VerifyOrderSignature(order1, sig1)
		if valid {
			fmt.Println("✓ Order accepted")
			usedNonces[signer.Address()][order1.Nonce.Uint64()] = true
		}
	}

	// Attacker tries to replay
	fmt.Println("\nAttacker replays same order...")
	if usedNonces[signer.Address()][order1.Nonce.Uint64()] {
		fmt.Println("✓ REJECTED: Nonce already used (replay attack prevented!)")
	}

	// User submits new order with nonce 2
	order2 := &OrderEIP712{
		Symbol:   "BTC-USDT",
		Side:     2,
		Type:     1,
		Price:    big.NewInt(51000),
		Qty:      big.NewInt(50),
		Nonce:    big.NewInt(2), // New nonce
		Deadline: big.NewInt(0),
		Leverage: 10,
		Owner:    signer.Address(),
	}

	sig2, _ := eip712Signer.SignOrder(signer, order2)

	fmt.Println("\nProcessing new order with nonce 2...")
	if usedNonces[signer.Address()][order2.Nonce.Uint64()] {
		fmt.Println("REJECTED: Nonce already used")
	} else {
		valid, _ := eip712Signer.VerifyOrderSignature(order2, sig2)
		if valid {
			fmt.Println("✓ Order accepted")
			usedNonces[signer.Address()][order2.Nonce.Uint64()] = true
		}
	}
}
