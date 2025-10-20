package perp

import (
	"fmt"
	"log"
	"math/big"

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
// Supports both legacy string format and new signed JSON format
func (a *App) applyTxV2(txBytes []byte, verifier *TxVerifier) int {
	// Try parsing as signed transaction first
	tx, err := transaction.ParseTransaction(txBytes)
	if err != nil {
		log.Printf("[app] invalid transaction: %v", err)
		return 0
	}

	// Handle legacy transactions (backward compatibility)
	if tx.Type == transaction.TxTypeLegacy {
		// Fall back to old string parsing (no signature verification)
		return a.applyTx(string(txBytes))
	}

	// Process signed transactions
	switch tx.Type {
	case transaction.TxTypeOrder:
		return a.applySignedOrder(tx, verifier)

	case transaction.TxTypeCancel:
		return a.applySignedCancel(tx, verifier)

	default:
		log.Printf("[app] unsupported transaction type: %s", tx.Type)
		return 0
	}
}

// applySignedOrder processes a signed order transaction
func (a *App) applySignedOrder(tx *transaction.SignedTransaction, verifier *TxVerifier) int {
	// Verify signature
	owner, valid, err := verifier.verifier.VerifyOrderTransaction(tx)
	if err != nil {
		log.Printf("[app] signature verification failed: %v", err)
		return 0
	}

	if !valid {
		log.Printf("[app] invalid signature")
		return 0
	}

	// Check nonce (replay protection)
	acc := a.accountManager.GetAccount(owner)
	orderNonce, ok := new(big.Int).SetString(tx.Order.Nonce, 10)
	if !ok {
		log.Printf("[app] invalid nonce: %s", tx.Order.Nonce)
		return 0
	}

	if orderNonce.Uint64() <= acc.Nonce {
		log.Printf("[app] nonce too low (replay attack): order nonce=%s, account nonce=%d",
			orderNonce.String(), acc.Nonce)
		return 0
	}

	// Update nonce (prevent replay)
	acc.Nonce = orderNonce.Uint64()

	// Parse order details
	price, _ := new(big.Int).SetString(tx.Order.Price, 10)
	qty, _ := new(big.Int).SetString(tx.Order.Qty, 10)

	if price.Int64() <= 0 || qty.Int64() <= 0 {
		log.Printf("[app] invalid price or quantity")
		return 0
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
		return 0
	}

	// Calculate position delta
	sizeDelta := qty.Int64()
	if side == core.Sell {
		sizeDelta = -qty.Int64()
	}

	// PRE-TRADE MARGIN CHECK
	if err := a.accountManager.CheckMarginRequirement(owner, market, price.Int64(), sizeDelta); err != nil {
		log.Printf("[app] margin check failed: %v", err)
		return 0
	}

	// Lock margin for order
	requiredMargin := market.RequiredInitialMargin(price.Int64(), qty.Int64())
	if err := a.accountManager.LockCollateral(owner, requiredMargin); err != nil {
		log.Printf("[app] failed to lock margin: %v (required=%d)", err, requiredMargin)
		return 0
	}

	// TODO: If order is GTC and not fully filled, keep margin locked
	// For now: unlock immediately after matching
	defer a.accountManager.UnlockCollateral(owner, requiredMargin)

	// Place order with market validation
	fills, err := a.getBook(tx.Order.Symbol).Place(order, market)
	if err != nil {
		log.Printf("[app] order rejected: %v", err)
		return 0
	}

	// Process all fills
	for _, fill := range fills {
		a.processFill(fill, market)
		log.Printf("[fill] %s taker=%s maker=%s px=%d qty=%d", tx.Order.Symbol, fill.TakerID, fill.MakerID, fill.Price, fill.Qty)
	}

	log.Printf("[app] signed order accepted: %s side=%s price=%s qty=%s owner=%s",
		tx.Order.Symbol, crypto.Uint8ToSide(tx.Order.Side), tx.Order.Price, tx.Order.Qty, owner.Hex())

	return len(fills)
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
