package crypto

import (
	"encoding/json"
	"fmt"
	"math/big"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/math"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/signer/core/apitypes"
)

// EIP712Domain represents the domain separator for EIP-712 typed data
// This prevents replay attacks across different chains/contracts
type EIP712Domain struct {
	Name              string         // Protocol name (e.g., "HyperLicked")
	Version           string         // Protocol version (e.g., "1")
	ChainID           *big.Int       // Chain ID (1337 for local, 1 for mainnet)
	VerifyingContract common.Address // Contract address (or zero for off-chain)
}

// OrderEIP712 represents an order for EIP-712 signing
// This is the typed data structure users sign in their wallets
type OrderEIP712 struct {
	Symbol   string         // Market symbol (e.g., "BTC-USDT")
	Side     uint8          // 1 = Buy, 2 = Sell (uint8 for EIP-712 compatibility)
	Type     uint8          // 1 = GTC, 2 = IOC, 3 = ALO
	Price    *big.Int       // Limit price in ticks
	Qty      *big.Int       // Quantity in lots
	Nonce    *big.Int       // Nonce for replay protection
	Deadline *big.Int       // Expiration timestamp (Unix seconds), 0 = no expiry
	Leverage uint8          // Leverage multiplier (1-50)
	Owner    common.Address // Order owner address
}

// CancelEIP712 represents a cancel order request for EIP-712 signing
type CancelEIP712 struct {
	OrderID string         // Order ID to cancel
	Symbol  string         // Market symbol (e.g., "BTC-USDT")
	Nonce   *big.Int       // Nonce for replay protection
	Owner   common.Address // Order owner address
}

// EIP712Signer handles EIP-712 typed data signing for orders
type EIP712Signer struct {
	domain EIP712Domain
}

// NewEIP712Signer creates a new EIP-712 signer with given domain
func NewEIP712Signer(domain EIP712Domain) *EIP712Signer {
	return &EIP712Signer{domain: domain}
}

// DefaultDomain returns the default EIP-712 domain for HyperLicked
func DefaultDomain() EIP712Domain {
	return EIP712Domain{
		Name:              "HyperLicked",
		Version:           "1",
		ChainID:           big.NewInt(1337), // Local dev chain
		VerifyingContract: common.Address{}, // Zero address for off-chain signing
	}
}

// HashOrder hashes an order according to EIP-712 spec
// Returns the digest that should be signed
func (e *EIP712Signer) HashOrder(order *OrderEIP712) ([]byte, error) {
	// Build EIP-712 typed data
	typedData := apitypes.TypedData{
		Types: apitypes.Types{
			"EIP712Domain": []apitypes.Type{
				{Name: "name", Type: "string"},
				{Name: "version", Type: "string"},
				{Name: "chainId", Type: "uint256"},
				{Name: "verifyingContract", Type: "address"},
			},
			"Order": []apitypes.Type{
				{Name: "symbol", Type: "string"},
				{Name: "side", Type: "uint8"},
				{Name: "type", Type: "uint8"},
				{Name: "price", Type: "uint256"},
				{Name: "qty", Type: "uint256"},
				{Name: "nonce", Type: "uint256"},
				{Name: "deadline", Type: "uint256"},
				{Name: "leverage", Type: "uint8"},
				{Name: "owner", Type: "address"},
			},
		},
		PrimaryType: "Order",
		Domain: apitypes.TypedDataDomain{
			Name:              e.domain.Name,
			Version:           e.domain.Version,
			ChainId:           (*math.HexOrDecimal256)(e.domain.ChainID),
			VerifyingContract: e.domain.VerifyingContract.Hex(),
		},
		Message: apitypes.TypedDataMessage{
			"symbol":   order.Symbol,
			"side":     fmt.Sprintf("%d", order.Side),
			"type":     fmt.Sprintf("%d", order.Type),
			"price":    order.Price.String(),
			"qty":      order.Qty.String(),
			"nonce":    order.Nonce.String(),
			"deadline": order.Deadline.String(),
			"leverage": fmt.Sprintf("%d", order.Leverage),
			"owner":    order.Owner.Hex(),
		},
	}

	// Compute EIP-712 hash
	domainSeparator, err := typedData.HashStruct("EIP712Domain", typedData.Domain.Map())
	if err != nil {
		return nil, fmt.Errorf("failed to hash domain: %w", err)
	}

	typedDataHash, err := typedData.HashStruct(typedData.PrimaryType, typedData.Message)
	if err != nil {
		return nil, fmt.Errorf("failed to hash message: %w", err)
	}

	// Final digest: keccak256("\x19\x01" || domainSeparator || typedDataHash)
	rawData := []byte(fmt.Sprintf("\x19\x01%s%s", string(domainSeparator), string(typedDataHash)))
	digest := crypto.Keccak256Hash(rawData)

	return digest.Bytes(), nil
}

// SignOrder signs an order and returns the signature
func (e *EIP712Signer) SignOrder(signer *Signer, order *OrderEIP712) ([]byte, error) {
	hash, err := e.HashOrder(order)
	if err != nil {
		return nil, fmt.Errorf("failed to hash order: %w", err)
	}

	signature, err := signer.Sign(hash)
	if err != nil {
		return nil, fmt.Errorf("failed to sign order: %w", err)
	}

	return signature, nil
}

// VerifyOrderSignature verifies that an order signature is valid
// Returns true if signature matches the order and claimed owner
func (e *EIP712Signer) VerifyOrderSignature(order *OrderEIP712, signature []byte) (bool, error) {
	hash, err := e.HashOrder(order)
	if err != nil {
		return false, fmt.Errorf("failed to hash order: %w", err)
	}

	// Recover signer address from signature
	recoveredAddr, err := RecoverAddress(hash, signature)
	if err != nil {
		return false, fmt.Errorf("failed to recover address: %w", err)
	}

	// Check if recovered address matches order owner
	return recoveredAddr == order.Owner, nil
}

// RecoverOrderSigner recovers the address that signed an order
// Useful for extracting owner from signature without prior knowledge
func (e *EIP712Signer) RecoverOrderSigner(order *OrderEIP712, signature []byte) (common.Address, error) {
	hash, err := e.HashOrder(order)
	if err != nil {
		return common.Address{}, fmt.Errorf("failed to hash order: %w", err)
	}

	return RecoverAddress(hash, signature)
}

// OrderToJSON converts an order to JSON for frontend/wallet signing
// MetaMask and other wallets use this format for eth_signTypedData_v4
func (e *EIP712Signer) OrderToJSON(order *OrderEIP712) (string, error) {
	typedData := map[string]interface{}{
		"types": map[string]interface{}{
			"EIP712Domain": []map[string]string{
				{"name": "name", "type": "string"},
				{"name": "version", "type": "string"},
				{"name": "chainId", "type": "uint256"},
				{"name": "verifyingContract", "type": "address"},
			},
			"Order": []map[string]string{
				{"name": "symbol", "type": "string"},
				{"name": "side", "type": "uint8"},
				{"name": "type", "type": "uint8"},
				{"name": "price", "type": "uint256"},
				{"name": "qty", "type": "uint256"},
				{"name": "nonce", "type": "uint256"},
				{"name": "deadline", "type": "uint256"},
				{"name": "leverage", "type": "uint8"},
				{"name": "owner", "type": "address"},
			},
		},
		"primaryType": "Order",
		"domain": map[string]interface{}{
			"name":              e.domain.Name,
			"version":           e.domain.Version,
			"chainId":           e.domain.ChainID.String(),
			"verifyingContract": e.domain.VerifyingContract.Hex(),
		},
		"message": map[string]interface{}{
			"symbol":   order.Symbol,
			"side":     order.Side,
			"type":     order.Type,
			"price":    order.Price.String(),
			"qty":      order.Qty.String(),
			"nonce":    order.Nonce.String(),
			"deadline": order.Deadline.String(),
			"leverage": order.Leverage,
			"owner":    order.Owner.Hex(),
		},
	}

	jsonBytes, err := json.MarshalIndent(typedData, "", "  ")
	if err != nil {
		return "", fmt.Errorf("failed to marshal JSON: %w", err)
	}

	return string(jsonBytes), nil
}

// Helper: Convert core.Side to EIP712 uint8
func SideToUint8(side string) uint8 {
	switch side {
	case "buy", "BUY":
		return 1
	case "sell", "SELL":
		return 2
	default:
		return 0
	}
}

// Helper: Convert EIP712 uint8 to core.Side string
func Uint8ToSide(side uint8) string {
	switch side {
	case 1:
		return "buy"
	case 2:
		return "sell"
	default:
		return "unknown"
	}
}

// Helper: Convert order type string to uint8
func OrderTypeToUint8(orderType string) uint8 {
	switch orderType {
	case "GTC", "gtc":
		return 1
	case "IOC", "ioc":
		return 2
	case "ALO", "alo":
		return 3
	default:
		return 0
	}
}

// Helper: Convert uint8 to order type string
func Uint8ToOrderType(orderType uint8) string {
	switch orderType {
	case 1:
		return "GTC"
	case 2:
		return "IOC"
	case 3:
		return "ALO"
	default:
		return "unknown"
	}
}

// HashCancel hashes a cancel order according to EIP-712 spec
// Returns the digest that should be signed
func (e *EIP712Signer) HashCancel(cancel *CancelEIP712) ([]byte, error) {
	// Build EIP-712 typed data
	typedData := apitypes.TypedData{
		Types: apitypes.Types{
			"EIP712Domain": []apitypes.Type{
				{Name: "name", Type: "string"},
				{Name: "version", Type: "string"},
				{Name: "chainId", Type: "uint256"},
				{Name: "verifyingContract", Type: "address"},
			},
			"CancelOrder": []apitypes.Type{
				{Name: "orderId", Type: "string"},
				{Name: "symbol", Type: "string"},
				{Name: "nonce", Type: "uint256"},
				{Name: "owner", Type: "address"},
			},
		},
		PrimaryType: "CancelOrder",
		Domain: apitypes.TypedDataDomain{
			Name:              e.domain.Name,
			Version:           e.domain.Version,
			ChainId:           (*math.HexOrDecimal256)(e.domain.ChainID),
			VerifyingContract: e.domain.VerifyingContract.Hex(),
		},
		Message: apitypes.TypedDataMessage{
			"orderId": cancel.OrderID,
			"symbol":  cancel.Symbol,
			"nonce":   cancel.Nonce.String(),
			"owner":   cancel.Owner.Hex(),
		},
	}

	// Compute EIP-712 hash
	domainSeparator, err := typedData.HashStruct("EIP712Domain", typedData.Domain.Map())
	if err != nil {
		return nil, fmt.Errorf("failed to hash domain: %w", err)
	}

	typedDataHash, err := typedData.HashStruct(typedData.PrimaryType, typedData.Message)
	if err != nil {
		return nil, fmt.Errorf("failed to hash message: %w", err)
	}

	// Final digest: keccak256("\x19\x01" || domainSeparator || typedDataHash)
	rawData := []byte(fmt.Sprintf("\x19\x01%s%s", string(domainSeparator), string(typedDataHash)))
	digest := crypto.Keccak256Hash(rawData)

	return digest.Bytes(), nil
}

// VerifyCancelSignature verifies that a cancel order signature is valid
// Returns true if signature matches the cancel request and claimed owner
func (e *EIP712Signer) VerifyCancelSignature(cancel *CancelEIP712, signature []byte) (bool, error) {
	hash, err := e.HashCancel(cancel)
	if err != nil {
		return false, fmt.Errorf("failed to hash cancel: %w", err)
	}

	// Recover signer address from signature
	recoveredAddr, err := RecoverAddress(hash, signature)
	if err != nil {
		return false, fmt.Errorf("failed to recover address: %w", err)
	}

	// Check if recovered address matches cancel owner
	return recoveredAddr == cancel.Owner, nil
}
