package perp

import (
	"encoding/json"
	"fmt"
	"math/big"
	"math/rand"
	"time"

	ethCrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/transaction"
	"github.com/uhyunpark/hyperlicked/pkg/crypto"
)

// SignedTxGenerator creates signed transactions for testing
type SignedTxGenerator struct {
	signers  []*crypto.Signer     // Keypairs for simulated traders
	symbols  []string             // List of tradeable markets
	rng      *rand.Rand
	nonces   map[string]uint64    // Track nonces per address
	eip712   *crypto.EIP712Signer
}

// NewSignedTxGenerator creates a new signed transaction generator
func NewSignedTxGenerator(numAccounts int, symbols []string) *SignedTxGenerator {
	signers := make([]*crypto.Signer, numAccounts)
	nonces := make(map[string]uint64)

	for i := 0; i < numAccounts; i++ {
		signer, _ := crypto.GenerateKey()
		signers[i] = signer
		nonces[signer.Address().Hex()] = 0 // Start nonces at 0
	}

	return &SignedTxGenerator{
		signers: signers,
		symbols: symbols,
		rng:     rand.New(rand.NewSource(time.Now().UnixNano())),
		nonces:  nonces,
		eip712:  crypto.NewEIP712Signer(crypto.DefaultDomain()),
	}
}

// GenerateSignedOrder creates a cryptographically signed order transaction
func (g *SignedTxGenerator) GenerateSignedOrder() []byte {
	// Pick random trader
	signer := g.signers[g.rng.Intn(len(g.signers))]
	symbol := g.symbols[g.rng.Intn(len(g.symbols))]

	// Increment nonce for this address
	addrHex := signer.Address().Hex()
	g.nonces[addrHex]++
	nonce := g.nonces[addrHex]

	// Random order type: 70% GTC, 20% IOC, 10% ALO
	var orderType uint8
	r := g.rng.Intn(100)
	if r < 70 {
		orderType = 1 // GTC
	} else if r < 90 {
		orderType = 2 // IOC
	} else {
		orderType = 3 // ALO
	}

	// Random side: 50% BUY, 50% SELL
	side := uint8(1) // Buy
	if g.rng.Intn(2) == 1 {
		side = 2 // Sell
	}

	// Random price around $50,000 BTC (±5%)
	basePrice := 50000
	priceVariation := g.rng.Intn(5000) - 2500
	price := basePrice + priceVariation
	if price < 1000 {
		price = 1000
	}

	// Random quantity: 1 to 100 lots
	qty := g.rng.Intn(100) + 1

	// Random leverage: 1x to 20x
	leverage := uint8(g.rng.Intn(20) + 1)

	// Create EIP-712 order
	order := &crypto.OrderEIP712{
		Symbol:   symbol,
		Side:     side,
		Type:     orderType,
		Price:    big.NewInt(int64(price)),
		Qty:      big.NewInt(int64(qty)),
		Nonce:    big.NewInt(int64(nonce)),
		Deadline: big.NewInt(0), // No expiry
		Leverage: leverage,
		Owner:    signer.Address(),
	}

	// Sign the order
	signature, err := g.eip712.SignOrder(signer, order)
	if err != nil {
		// Fallback to legacy format on error
		return []byte(fmt.Sprintf("O:GTC:%s:BUY:price=%d:qty=%d:id=fallback_%d", symbol, price, qty, nonce))
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
		// Fallback to legacy format on error
		return []byte(fmt.Sprintf("O:GTC:%s:BUY:price=%d:qty=%d:id=fallback_%d", symbol, price, qty, nonce))
	}

	return txJSON
}

// GenerateSignedCancel creates a cryptographically signed cancel transaction
func (g *SignedTxGenerator) GenerateSignedCancel(orderID, symbol string, ownerIndex int) []byte {
	if ownerIndex >= len(g.signers) {
		return []byte(fmt.Sprintf("C:%s:%s", symbol, orderID))
	}

	signer := g.signers[ownerIndex]
	addrHex := signer.Address().Hex()

	// Increment nonce
	g.nonces[addrHex]++
	nonce := g.nonces[addrHex]

	// Create cancel payload
	cancelPayload := &transaction.CancelPayload{
		OrderID: orderID,
		Symbol:  symbol,
		Nonce:   fmt.Sprintf("%d", nonce),
		Owner:   signer.Address().Hex(),
	}

	// Sign cancel (simple hash)
	message := fmt.Sprintf("CANCEL:%s:%s:%s", symbol, orderID, cancelPayload.Nonce)
	hash := ethCrypto.Keccak256([]byte(message))
	signature, err := signer.Sign(hash)
	if err != nil {
		// Fallback to legacy
		return []byte(fmt.Sprintf("C:%s:%s", symbol, orderID))
	}

	// Create signed transaction
	signedTx := &transaction.SignedTransaction{
		Type:      transaction.TxTypeCancel,
		Cancel:    cancelPayload,
		Signature: fmt.Sprintf("0x%x", signature),
	}

	txJSON, err := json.Marshal(signedTx)
	if err != nil {
		return []byte(fmt.Sprintf("C:%s:%s", symbol, orderID))
	}

	return txJSON
}

// GetSigners returns all signers (for testing/debugging)
func (g *SignedTxGenerator) GetSigners() []*crypto.Signer {
	return g.signers
}

// GetNonce returns current nonce for an address
func (g *SignedTxGenerator) GetNonce(addr string) uint64 {
	return g.nonces[addr]
}
