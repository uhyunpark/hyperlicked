package transaction

import (
	"encoding/hex"
	"fmt"
	"strings"

	"github.com/ethereum/go-ethereum/common"
	ethCrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/uhyunpark/hyperlicked/pkg/crypto"
)

// Verifier handles transaction signature verification
type Verifier struct {
	eip712Signer *crypto.EIP712Signer
	agentSigner  *crypto.AgentSigner
}

// NewVerifier creates a new transaction verifier
func NewVerifier(domain crypto.EIP712Domain) *Verifier {
	return &Verifier{
		eip712Signer: crypto.NewEIP712Signer(domain),
		agentSigner:  crypto.NewAgentSigner(domain),
	}
}

// VerifyOrderTransaction verifies a signed order transaction
// Returns (owner address, valid, error)
func (v *Verifier) VerifyOrderTransaction(tx *SignedTransaction) (common.Address, bool, error) {
	if tx.Type != TxTypeOrder {
		return common.Address{}, false, fmt.Errorf("not an order transaction")
	}

	if tx.Order == nil {
		return common.Address{}, false, fmt.Errorf("missing order payload")
	}

	// Convert to EIP-712 format
	order, err := tx.Order.ToEIP712Order()
	if err != nil {
		return common.Address{}, false, fmt.Errorf("invalid order format: %w", err)
	}

	// Decode signature
	sigBytes, err := decodeSignature(tx.Signature)
	if err != nil {
		return common.Address{}, false, fmt.Errorf("invalid signature: %w", err)
	}

	// Verify signature
	valid, err := v.eip712Signer.VerifyOrderSignature(order, sigBytes)
	if err != nil {
		return common.Address{}, false, fmt.Errorf("signature verification failed: %w", err)
	}

	if !valid {
		return common.Address{}, false, fmt.Errorf("signature invalid")
	}

	return order.Owner, true, nil
}

// VerifyAgentOrderTransaction verifies an order signed by an agent key
// Requires delegation to be provided
func (v *Verifier) VerifyAgentOrderTransaction(
	tx *SignedTransaction,
	delegation *crypto.AgentDelegation,
	delegationSignature []byte,
) (common.Address, bool, error) {
	if tx.Type != TxTypeOrder {
		return common.Address{}, false, fmt.Errorf("not an order transaction")
	}

	if !tx.AgentMode {
		return common.Address{}, false, fmt.Errorf("not in agent mode")
	}

	// Convert to EIP-712 format
	order, err := tx.Order.ToEIP712Order()
	if err != nil {
		return common.Address{}, false, fmt.Errorf("invalid order format: %w", err)
	}

	// Decode signature
	agentSigBytes, err := decodeSignature(tx.Signature)
	if err != nil {
		return common.Address{}, false, fmt.Errorf("invalid agent signature: %w", err)
	}

	// Verify agent order (checks both agent sig + delegation)
	valid, err := crypto.VerifyAgentOrder(
		order,
		agentSigBytes,
		delegation,
		delegationSignature,
		v.eip712Signer,
		v.agentSigner,
	)
	if err != nil {
		return common.Address{}, false, fmt.Errorf("agent verification failed: %w", err)
	}

	if !valid {
		return common.Address{}, false, fmt.Errorf("agent order invalid")
	}

	// Return wallet address (not agent address)
	return delegation.Wallet, true, nil
}

// VerifyCancelTransaction verifies a signed cancel transaction
func (v *Verifier) VerifyCancelTransaction(tx *SignedTransaction) (common.Address, bool, error) {
	if tx.Type != TxTypeCancel {
		return common.Address{}, false, fmt.Errorf("not a cancel transaction")
	}

	if tx.Cancel == nil {
		return common.Address{}, false, fmt.Errorf("missing cancel payload")
	}

	// For cancel, we hash the cancel payload
	// (Similar to EIP-712 but simpler - just sign the cancel request)
	owner := common.HexToAddress(tx.Cancel.Owner)

	// Decode signature
	sigBytes, err := decodeSignature(tx.Signature)
	if err != nil {
		return common.Address{}, false, fmt.Errorf("invalid signature: %w", err)
	}

	// Simple hash of cancel data
	message := fmt.Sprintf("CANCEL:%s:%s:%s", tx.Cancel.Symbol, tx.Cancel.OrderID, tx.Cancel.Nonce)

	// Use ethereum crypto package for hashing
	hashBytes := ethCrypto.Keccak256([]byte(message))
	if len(hashBytes) != 32 {
		return common.Address{}, false, fmt.Errorf("unexpected hash length: %d", len(hashBytes))
	}
	hash := hashBytes

	// Verify signature
	valid := crypto.VerifySignature(owner, hash, sigBytes)
	if !valid {
		return common.Address{}, false, fmt.Errorf("invalid cancel signature")
	}

	return owner, true, nil
}

// decodeSignature decodes hex-encoded signature (with or without 0x prefix)
func decodeSignature(sig string) ([]byte, error) {
	sig = strings.TrimPrefix(sig, "0x")

	sigBytes, err := hex.DecodeString(sig)
	if err != nil {
		return nil, fmt.Errorf("invalid hex signature: %w", err)
	}

	if len(sigBytes) != 65 {
		return nil, fmt.Errorf("signature must be 65 bytes, got %d", len(sigBytes))
	}

	return sigBytes, nil
}

// RecoverSigner recovers the address that signed a transaction
// Useful for debugging or extracting owner without prior knowledge
func (v *Verifier) RecoverSigner(tx *SignedTransaction) (common.Address, error) {
	switch tx.Type {
	case TxTypeOrder:
		owner, valid, err := v.VerifyOrderTransaction(tx)
		if err != nil {
			return common.Address{}, err
		}
		if !valid {
			return common.Address{}, fmt.Errorf("invalid signature")
		}
		return owner, nil

	case TxTypeCancel:
		owner, valid, err := v.VerifyCancelTransaction(tx)
		if err != nil {
			return common.Address{}, err
		}
		if !valid {
			return common.Address{}, fmt.Errorf("invalid signature")
		}
		return owner, nil

	default:
		return common.Address{}, fmt.Errorf("unsupported transaction type: %s", tx.Type)
	}
}
