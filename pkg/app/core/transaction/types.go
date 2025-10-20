package transaction

import (
	"encoding/json"
	"fmt"
	"math/big"

	"github.com/ethereum/go-ethereum/common"
	"github.com/uhyunpark/hyperlicked/pkg/crypto"
)

// TxType represents the type of transaction
type TxType string

const (
	TxTypeOrder      TxType = "order"       // Place order (signed)
	TxTypeCancel     TxType = "cancel"      // Cancel order (signed)
	TxTypeLegacy     TxType = "legacy"      // Old string format (backward compat)
	TxTypeDelegation TxType = "delegation"  // Agent key delegation
)

// SignedTransaction represents a cryptographically signed transaction
// This is the new format that replaces string-based "O:GTC:BTC-USDT:..."
type SignedTransaction struct {
	Type      TxType          `json:"type"`               // Transaction type
	Order     *OrderPayload   `json:"order,omitempty"`    // Order data (if type=order)
	Cancel    *CancelPayload  `json:"cancel,omitempty"`   // Cancel data (if type=cancel)
	Signature string          `json:"signature"`          // Hex-encoded signature (0x...)

	// For agent key orders
	AgentMode      bool   `json:"agent_mode,omitempty"`      // True if signed by agent
	DelegationID   string `json:"delegation_id,omitempty"`   // Delegation reference
}

// OrderPayload contains order data for EIP-712 signing
type OrderPayload struct {
	Symbol   string `json:"symbol"`    // "BTC-USDT"
	Side     uint8  `json:"side"`      // 1=Buy, 2=Sell
	Type     uint8  `json:"type"`      // 1=GTC, 2=IOC, 3=ALO
	Price    string `json:"price"`     // BigInt as string
	Qty      string `json:"qty"`       // BigInt as string
	Nonce    string `json:"nonce"`     // BigInt as string
	Deadline string `json:"deadline"`  // Unix timestamp (0 = no expiry)
	Leverage uint8  `json:"leverage"`  // 1-50x
	Owner    string `json:"owner"`     // Ethereum address (0x...)
}

// CancelPayload contains order cancellation data
type CancelPayload struct {
	OrderID string `json:"order_id"` // ID of order to cancel
	Symbol  string `json:"symbol"`   // Market symbol
	Nonce   string `json:"nonce"`    // BigInt as string (replay protection)
	Owner   string `json:"owner"`    // Ethereum address
}

// ToEIP712Order converts OrderPayload to crypto.OrderEIP712 for signing/verification
func (o *OrderPayload) ToEIP712Order() (*crypto.OrderEIP712, error) {
	price, ok := new(big.Int).SetString(o.Price, 10)
	if !ok {
		return nil, fmt.Errorf("invalid price: %s", o.Price)
	}

	qty, ok := new(big.Int).SetString(o.Qty, 10)
	if !ok {
		return nil, fmt.Errorf("invalid qty: %s", o.Qty)
	}

	nonce, ok := new(big.Int).SetString(o.Nonce, 10)
	if !ok {
		return nil, fmt.Errorf("invalid nonce: %s", o.Nonce)
	}

	deadline, ok := new(big.Int).SetString(o.Deadline, 10)
	if !ok {
		return nil, fmt.Errorf("invalid deadline: %s", o.Deadline)
	}

	return &crypto.OrderEIP712{
		Symbol:   o.Symbol,
		Side:     o.Side,
		Type:     o.Type,
		Price:    price,
		Qty:      qty,
		Nonce:    nonce,
		Deadline: deadline,
		Leverage: o.Leverage,
		Owner:    common.HexToAddress(o.Owner),
	}, nil
}

// FromEIP712Order converts crypto.OrderEIP712 to OrderPayload
func FromEIP712Order(order *crypto.OrderEIP712) *OrderPayload {
	return &OrderPayload{
		Symbol:   order.Symbol,
		Side:     order.Side,
		Type:     order.Type,
		Price:    order.Price.String(),
		Qty:      order.Qty.String(),
		Nonce:    order.Nonce.String(),
		Deadline: order.Deadline.String(),
		Leverage: order.Leverage,
		Owner:    order.Owner.Hex(),
	}
}

// Serialize converts SignedTransaction to JSON bytes
func (tx *SignedTransaction) Serialize() ([]byte, error) {
	return json.Marshal(tx)
}

// Deserialize parses JSON bytes into SignedTransaction
func Deserialize(data []byte) (*SignedTransaction, error) {
	var tx SignedTransaction
	if err := json.Unmarshal(data, &tx); err != nil {
		return nil, fmt.Errorf("failed to unmarshal transaction: %w", err)
	}
	return &tx, nil
}

// Validate performs basic validation on transaction structure
func (tx *SignedTransaction) Validate() error {
	if tx.Type == "" {
		return fmt.Errorf("missing transaction type")
	}

	if tx.Signature == "" {
		return fmt.Errorf("missing signature")
	}

	switch tx.Type {
	case TxTypeOrder:
		if tx.Order == nil {
			return fmt.Errorf("order type requires order payload")
		}
		if tx.Order.Symbol == "" {
			return fmt.Errorf("missing order symbol")
		}
		if tx.Order.Side == 0 {
			return fmt.Errorf("invalid order side")
		}
		if tx.Order.Owner == "" {
			return fmt.Errorf("missing order owner")
		}

	case TxTypeCancel:
		if tx.Cancel == nil {
			return fmt.Errorf("cancel type requires cancel payload")
		}
		if tx.Cancel.OrderID == "" {
			return fmt.Errorf("missing cancel order ID")
		}
		if tx.Cancel.Owner == "" {
			return fmt.Errorf("missing cancel owner")
		}

	default:
		return fmt.Errorf("unknown transaction type: %s", tx.Type)
	}

	return nil
}

// IsLegacy checks if transaction is in old string format
func IsLegacy(data []byte) bool {
	// Legacy format starts with "O:" or "C:" or "N:"
	if len(data) < 2 {
		return false
	}
	return (data[0] == 'O' || data[0] == 'C' || data[0] == 'N') && data[1] == ':'
}

// ParseTransaction parses either legacy string format or new JSON format
func ParseTransaction(data []byte) (*SignedTransaction, error) {
	// Check if legacy format
	if IsLegacy(data) {
		return &SignedTransaction{
			Type: TxTypeLegacy,
			// Legacy transactions not verified (backward compat only)
		}, nil
	}

	// Parse as JSON
	tx, err := Deserialize(data)
	if err != nil {
		return nil, fmt.Errorf("failed to parse transaction: %w", err)
	}

	// Validate structure
	if err := tx.Validate(); err != nil {
		return nil, fmt.Errorf("invalid transaction: %w", err)
	}

	return tx, nil
}

// Example formats for reference:

// Old format (legacy):
//   "O:GTC:BTC-USDT:BUY:price=50000:qty=100:id=alice_o1:owner=0x742d..."

// New format (signed):
//   {
//     "type": "order",
//     "order": {
//       "symbol": "BTC-USDT",
//       "side": 1,
//       "type": 1,
//       "price": "50000",
//       "qty": "100",
//       "nonce": "42",
//       "deadline": "0",
//       "leverage": 10,
//       "owner": "0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0"
//     },
//     "signature": "0x1234567890abcdef..."
//   }

// Agent mode (pre-authorized):
//   {
//     "type": "order",
//     "order": { ... },
//     "signature": "0x...",  // Signed by agent key
//     "agent_mode": true,
//     "delegation_id": "del_12345"
//   }
