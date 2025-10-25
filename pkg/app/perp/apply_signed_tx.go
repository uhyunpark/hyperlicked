package perp

import (
	"fmt"
	"log"
	"math/big"

	"github.com/ethereum/go-ethereum/common"
	"github.com/uhyunpark/hyperlicked/pkg/app/core"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/transaction"
	"github.com/uhyunpark/hyperlicked/pkg/crypto"
)

// TxVerifier handles signature verification for transactions
type TxVerifier struct {
	verifier *transaction.Verifier
}

// NewTxVerifier creates a new transaction verifier with default domain
func NewTxVerifier() *TxVerifier {
	domain := crypto.DefaultDomain()
	return &TxVerifier{
		verifier: transaction.NewVerifier(domain),
	}
}

// applyTxV2 processes a transaction with signature verification
// Deprecated: Use applyTxV2WithFills for better observability
func (a *App) applyTxV2(txBytes []byte, verifier *TxVerifier) int {
	fills := a.applyTxV2WithFills(txBytes, verifier)
	return len(fills)
}

// applyTxV2WithFills processes a transaction and returns fills with metadata for broadcasting
func (a *App) applyTxV2WithFills(txBytes []byte, verifier *TxVerifier) []fillWithMetadata {
	// Try parsing as signed transaction first
	tx, err := transaction.ParseTransaction(txBytes)
	if err != nil {
		log.Printf("[app] invalid transaction: %v", err)
		return nil
	}

	// Handle legacy transactions (backward compatibility)
	if tx.Type == transaction.TxTypeLegacy {
		// Fall back to old string parsing (no signature verification)
		// Legacy format doesn't support fill tracking
		a.applyTx(string(txBytes))
		return nil
	}

	// Process signed transactions
	switch tx.Type {
	case transaction.TxTypeOrder:
		return a.applySignedOrderWithFills(tx, verifier)

	case transaction.TxTypeCancel:
		a.applySignedCancel(tx, verifier)
		return nil // Cancels don't produce fills

	default:
		log.Printf("[app] unsupported transaction type: %s", tx.Type)
		return nil
	}
}

// applySignedOrder processes a signed order transaction (returns fill count)
// Deprecated: Use applySignedOrderWithFills for better observability
func (a *App) applySignedOrder(tx *transaction.SignedTransaction, verifier *TxVerifier) int {
	fills := a.applySignedOrderWithFills(tx, verifier)
	return len(fills)
}

// applySignedOrderWithFills processes a signed order and returns fills with metadata
func (a *App) applySignedOrderWithFills(tx *transaction.SignedTransaction, verifier *TxVerifier) []fillWithMetadata {
	var owner common.Address
	var valid bool
	var err error

	// Check if agent mode (signed by agent key with delegation)
	if tx.AgentMode && tx.DelegationID != "" {
		// Agent mode: verify delegation + agent signature
		log.Printf("[app] verifying agent-signed order (delegation_id=%s)", tx.DelegationID)

		// Retrieve delegation from storage
		storedDel, ok := a.GetDelegation(tx.DelegationID)
		if !ok {
			log.Printf("[app] delegation not found: %s", tx.DelegationID)
			return nil
		}

		// Verify agent order (checks agent sig + delegation sig + expiration)
		owner, valid, err = verifier.verifier.VerifyAgentOrderTransaction(
			tx,
			storedDel.Delegation,
			storedDel.Signature,
		)
		if err != nil {
			log.Printf("[app] agent signature verification failed: %v", err)
			return nil
		}

		if !valid {
			log.Printf("[app] invalid agent signature")
			return nil
		}

		log.Printf("[app] agent order verified: wallet=%s agent=%s",
			owner.Hex(), storedDel.Delegation.Agent.Hex())
	} else {
		// Regular mode: verify direct wallet signature
		owner, valid, err = verifier.verifier.VerifyOrderTransaction(tx)
		if err != nil {
			log.Printf("[app] signature verification failed: %v", err)
			return nil
		}

		if !valid {
			log.Printf("[app] invalid signature")
			return nil
		}
	}

	// Check nonce (replay protection)
	acc := a.accountManager.GetAccount(owner)
	orderNonce, ok := new(big.Int).SetString(tx.Order.Nonce, 10)
	if !ok {
		log.Printf("[app] invalid nonce: %s", tx.Order.Nonce)
		return nil
	}

	if orderNonce.Uint64() <= acc.Nonce {
		log.Printf("[app] nonce too low (replay attack): order nonce=%s, account nonce=%d",
			orderNonce.String(), acc.Nonce)
		return nil
	}

	// Update nonce (prevent replay)
	acc.Nonce = orderNonce.Uint64()

	// Parse order details
	price, _ := new(big.Int).SetString(tx.Order.Price, 10)
	qty, _ := new(big.Int).SetString(tx.Order.Qty, 10)

	if price.Int64() <= 0 || qty.Int64() <= 0 {
		log.Printf("[app] invalid price or quantity")
		return nil
	}

	// Convert to internal order format
	var side core.Side
	if tx.Order.Side == 1 {
		side = core.Buy
	} else {
		side = core.Sell
	}

	orderType := crypto.Uint8ToOrderType(tx.Order.Type)
	orderID := fmt.Sprintf("%s-ord-%s", owner.Hex(), tx.Order.Nonce)

	order := &core.Order{
		ID:       orderID,
		Symbol:   tx.Order.Symbol,
		Side:     side,
		Price:    price.Int64(),
		Qty:      qty.Int64(),
		Type:     orderType,
		OwnerHex: owner.Hex(),
	}

	// Get market for validation
	market, err := a.registry.GetMarket(tx.Order.Symbol)
	if err != nil {
		log.Printf("[app] market not found for %s: %v", tx.Order.Symbol, err)
		return nil
	}

	// Calculate position delta
	sizeDelta := qty.Int64()
	if side == core.Sell {
		sizeDelta = -qty.Int64()
	}

	// PRE-TRADE MARGIN CHECK
	if err := a.accountManager.CheckMarginRequirement(owner, market, price.Int64(), sizeDelta); err != nil {
		log.Printf("[app] margin check failed: %v", err)
		return nil
	}

	// Lock margin for order
	requiredMargin := market.RequiredInitialMargin(price.Int64(), qty.Int64())
	if err := a.accountManager.LockCollateral(owner, requiredMargin); err != nil {
		log.Printf("[app] failed to lock margin: %v (required=%d)", err, requiredMargin)
		return nil
	}

	// TODO: If order is GTC and not fully filled, keep margin locked
	// For now: unlock immediately after matching
	defer a.accountManager.UnlockCollateral(owner, requiredMargin)

	// Place order with market validation
	fills, err := a.getBook(tx.Order.Symbol).Place(order, market)
	if err != nil {
		log.Printf("[app] order rejected: %v", err)
		return nil
	}

	// Process all fills
	for _, fill := range fills {
		a.processFill(fill, market)
		log.Printf("[fill] %s taker=%s maker=%s px=%d qty=%d", tx.Order.Symbol, fill.TakerID, fill.MakerID, fill.Price, fill.Qty)
	}

	log.Printf("[app] signed order accepted: %s side=%s price=%s qty=%s owner=%s",
		tx.Order.Symbol, crypto.Uint8ToSide(tx.Order.Side), tx.Order.Price, tx.Order.Qty, owner.Hex())

	// Convert fills to metadata format for broadcasting
	var result []fillWithMetadata
	for _, fill := range fills {
		// Taker side determines trade side (buyer or seller initiated)
		tradeSide := "buy"
		if side == core.Sell {
			tradeSide = "sell"
		}

		result = append(result, fillWithMetadata{
			Symbol: tx.Order.Symbol,
			Price:  fill.Price,
			Qty:    fill.Qty,
			Side:   tradeSide,
		})
	}

	return result
}

// applySignedCancel processes a signed cancel transaction
func (a *App) applySignedCancel(tx *transaction.SignedTransaction, verifier *TxVerifier) int {
	// Verify signature
	owner, valid, err := verifier.verifier.VerifyCancelTransaction(tx)
	if err != nil {
		log.Printf("[app] cancel signature verification failed: %v", err)
		return 0
	}

	if !valid {
		log.Printf("[app] invalid cancel signature")
		return 0
	}

	// Check nonce (replay protection)
	acc := a.accountManager.GetAccount(owner)
	cancelNonce, ok := new(big.Int).SetString(tx.Cancel.Nonce, 10)
	if !ok {
		log.Printf("[app] invalid cancel nonce: %s", tx.Cancel.Nonce)
		return 0
	}

	if cancelNonce.Uint64() <= acc.Nonce {
		log.Printf("[app] cancel nonce too low (replay attack)")
		return 0
	}

	// Update nonce
	acc.Nonce = cancelNonce.Uint64()

	// Cancel the order
	if ok := a.getBook(tx.Cancel.Symbol).Cancel(tx.Cancel.OrderID); !ok {
		log.Printf("[app] cancel miss: %s/%s", tx.Cancel.Symbol, tx.Cancel.OrderID)
	} else {
		log.Printf("[app] order cancelled: %s/%s by %s", tx.Cancel.Symbol, tx.Cancel.OrderID, owner.Hex())
	}

	return 0
}
