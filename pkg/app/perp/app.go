package perp

import (
	"crypto/sha256"
	"encoding/binary"
	"fmt"
	"log"
	"sort"
	"strconv"
	"strings"
	"sync"

	"github.com/ethereum/go-ethereum/common"
	"github.com/uhyunpark/hyperlicked/pkg/abci"
	"github.com/uhyunpark/hyperlicked/pkg/app/core"
	"github.com/uhyunpark/hyperlicked/pkg/consensus"
	"github.com/uhyunpark/hyperlicked/pkg/crypto"
)

// TradeBroadcaster is called when a trade executes
type TradeBroadcaster func(symbol string, price, size int64, side string, timestamp int64)

// StoredDelegation represents a delegation stored on the backend
type StoredDelegation struct {
	Delegation *crypto.AgentDelegation
	Signature  []byte // EIP-712 signature from wallet
}

type App struct {
	mempool        *core.Mempool
	registry       *core.MarketRegistry
	books          map[string]*core.OrderBook
	accountManager *core.AccountManager
	txVerifier     *TxVerifier // Signature verifier for signed transactions

	// Agent key delegations: delegationID -> delegation
	delegations   map[string]*StoredDelegation
	delegationsMu sync.RWMutex

	// Callbacks for external integrations (WebSocket, etc.)
	OnTrade TradeBroadcaster
}

func NewApp() *App {
	app := &App{
		mempool:        core.NewMempool(),
		registry:       core.NewMarketRegistry(),
		books:          make(map[string]*core.OrderBook),
		accountManager: core.NewAccountManager(),
		txVerifier:     NewTxVerifier(), // Initialize transaction verifier
		delegations:    make(map[string]*StoredDelegation),
	}

	// Register single market: BTC-USDT perpetual
	market, err := core.NewMarketWithDefaults("BTC-USDT", "BTC", "USDT")
	if err != nil {
		log.Fatalf("[app] failed to create BTC-USDT market: %v", err)
	}
	if err := app.registry.RegisterMarket(market); err != nil {
		log.Fatalf("[app] failed to register BTC-USDT market: %v", err)
	}
	app.books["BTC-USDT"] = core.NewOrderBook()

	log.Printf("[app] initialized with market: BTC-USDT")

	return app
}

func (a *App) PushTx(b []byte) { a.mempool.PushRaw(b) }

func (a *App) PrepareProposal(req abci.RequestPrepareProposal) abci.ResponsePrepareProposal {
	txs := a.mempool.SelectForProposal(req.MaxTxBytes)
	return abci.ResponsePrepareProposal{Txs: txs}
}
func (a *App) ProcessProposal(_ abci.RequestProcessProposal) abci.ResponseProcessProposal {
	return abci.ResponseProcessProposal{Accept: true}
}
func (a *App) FinalizeBlock(req abci.RequestFinalizeBlock) abci.ResponseFinalizeBlock {
	// Track fills for broadcasting
	var allFills []fillWithMetadata
	totalFills := 0

	for _, tx := range req.Txs {
		// Use new signature-verified transaction processor
		fills := a.applyTxV2WithFills(tx, a.txVerifier)
		totalFills += len(fills)
		allFills = append(allFills, fills...)
	}

	// Broadcast trades to WebSocket clients (if callback registered)
	if a.OnTrade != nil {
		for _, f := range allFills {
			a.OnTrade(f.Symbol, f.Price, f.Qty, f.Side, req.Timestamp)
		}
	}

	// Compute state hash after executing all transactions (includes height, timestamp, orderbook state)
	appHash := a.computeStateHash(req.Height, req.Timestamp)

	// Log block execution summary
	if len(req.Txs) > 0 || totalFills > 0 {
		log.Printf("[app] FinalizeBlock h=%d txs=%d fills=%d apphash=%s",
			req.Height, len(req.Txs), totalFills, formatHash(appHash))
	}

	return abci.ResponseFinalizeBlock{
		Events:  []string{"commit"},
		AppHash: appHash,
	}
}

// fillWithMetadata wraps a Fill with its symbol and side for broadcasting
type fillWithMetadata struct {
	Symbol string
	Price  int64
	Qty    int64
	Side   string // "buy" or "sell" (taker side)
}

func (a *App) getBook(sym string) *core.OrderBook {
	if ob, ok := a.books[sym]; ok {
		return ob
	}
	ob := core.NewOrderBook()
	a.books[sym] = ob
	return ob
}

func (a *App) applyTx(s string) int {
	if strings.HasPrefix(s, "N:") {
		return 0
	}

	if strings.HasPrefix(s, "C:") {
		rest := strings.TrimPrefix(s, "C:")
		parts := strings.Split(rest, ":")

		var sym, oid string

		if len(parts) == 1 {
			sym, oid = "PERP-USD", parts[0]
		} else {
			sym, oid = parts[0], parts[1]
		}

		if ok := a.getBook(sym).Cancel(oid); !ok {
			log.Printf("[app] cancel miss: %s/%s", sym, oid)
		}

		return 0
	}

	if strings.HasPrefix(s, "O:") {
		parts := strings.Split(s, ":")
		if len(parts) < 7 {
			log.Printf("[app] bad order tx: %s", s)
			return 0
		}
		typ := parts[1]
		sym := parts[2]
		sideStr := parts[3]
		priceStr := strings.TrimPrefix(parts[4], "price=")
		qtyStr := strings.TrimPrefix(parts[5], "qty=")
		idStr := strings.TrimPrefix(parts[6], "id=")

		var owner string

		if len(parts) >= 8 && strings.HasPrefix(parts[7], "owner=") {
			owner = strings.TrimPrefix(parts[7], "owner=")
		}
		price, err1 := strconv.ParseInt(priceStr, 10, 64)
		qty, err2 := strconv.ParseInt(qtyStr, 10, 64)

		if err1 != nil || err2 != nil {
			log.Printf("[app] parse err(order): %s", s)
			return 0
		}

		var side core.Side

		if strings.EqualFold(sideStr, "BUY") {
			side = core.Buy
		} else {
			side = core.Sell
		}
		o := &core.Order{ID: idStr, Symbol: sym, Side: side, Price: price, Qty: qty, Type: typ, OwnerHex: owner}

		// Get market for validation
		market, err := a.registry.GetMarket(sym)
		if err != nil {
			log.Printf("[app] market not found for %s: %v", sym, err)
			return 0
		}

		// Parse owner address (if provided)
		var ownerAddr common.Address
		if owner != "" {
			if !common.IsHexAddress(owner) {
				log.Printf("[app] invalid owner address: %s", owner)
				return 0
			}
			ownerAddr = common.HexToAddress(owner)

			// Calculate position delta from this order
			sizeDelta := qty
			if side == core.Sell {
				sizeDelta = -qty
			}

			// PRE-TRADE MARGIN CHECK: Verify account has sufficient margin and won't exceed leverage
			if err := a.accountManager.CheckMarginRequirement(ownerAddr, market, price, sizeDelta); err != nil {
				log.Printf("[app] margin check failed: %v", err)
				return 0
			}

			// Lock margin for order
			requiredMargin := market.RequiredInitialMargin(price, qty)
			if err := a.accountManager.LockCollateral(ownerAddr, requiredMargin); err != nil {
				log.Printf("[app] failed to lock margin: %v (required=%d)", err, requiredMargin)
				return 0
			}

			// TODO: If order is GTC and not fully filled, keep margin locked
			// For now: unlock immediately after matching (simplified)
			defer a.accountManager.UnlockCollateral(ownerAddr, requiredMargin)
		}

		// Place order with market validation
		fills, err := a.getBook(sym).Place(o, market)
		if err != nil {
			log.Printf("[app] order rejected: %v", err)
			return 0
		}

		// Process all fills (update positions, apply fees)
		for _, f := range fills {
			a.processFill(f, market)
			log.Printf("[fill] %s taker=%s maker=%s px=%d qty=%d", sym, f.TakerID, f.MakerID, f.Price, f.Qty)
		}

		return len(fills)
	}

	log.Printf("[app] unknown tx: %s", s)
	return 0
}

// processFill updates positions and applies fees for a trade fill
func (a *App) processFill(fill core.Fill, market *core.Market) {
	// TODO: Support fills without owner addresses (for backward compat with test txs)
	// For now, skip fills without owner info

	// Note: In production, we need to track which order ID belongs to which address
	// For prototype: we'll extract addresses from order IDs if they start with "0x"

	// Extract taker and maker addresses from order IDs
	// Format: order IDs should be "0xADDRESS-orderId" or just use raw address
	takerAddr, takerOk := a.parseOwnerFromOrderID(fill.TakerID)
	makerAddr, makerOk := a.parseOwnerFromOrderID(fill.MakerID)

	if !takerOk || !makerOk {
		// Skip position/fee processing if addresses not available
		return
	}

	// Calculate notional value
	notional := fill.Price * fill.Qty

	// 1. Apply fees
	// Taker pays fee: notional × TakerFeeBps / 10000
	takerFee := (notional * market.TakerFeeBps) / 10000
	if takerFee != 0 {
		if err := a.accountManager.ApplyFees(takerAddr, -takerFee); err != nil {
			log.Printf("[app] failed to apply taker fee: %v", err)
		}
	}

	// Maker earns rebate: notional × (-MakerFeeBps) / 10000
	makerRebate := (notional * -market.MakerFeeBps) / 10000
	if makerRebate != 0 {
		if err := a.accountManager.ApplyFees(makerAddr, makerRebate); err != nil {
			log.Printf("[app] failed to apply maker rebate: %v", err)
		}
	}

	// 2. Update positions
	// Taker increases position (buy = +ve, sell = -ve)
	// Determine taker's position delta from fill
	// Note: We need to know taker's side - for now infer from orderbook context
	// TODO: Add Side field to Fill struct

	// For prototype: assume taker is always the one taking liquidity
	// We'll need better tracking in production

	// 3. Record trade statistics
	if err := a.accountManager.RecordTrade(takerAddr, notional); err != nil {
		log.Printf("[app] failed to record taker trade: %v", err)
	}
	if err := a.accountManager.RecordTrade(makerAddr, notional); err != nil {
		log.Printf("[app] failed to record maker trade: %v", err)
	}
}

// parseOwnerFromOrderID extracts address from order ID
// Supports formats: "0xADDRESS", "0xADDRESS-suffix", or plain orderID (returns false)
func (a *App) parseOwnerFromOrderID(orderID string) (common.Address, bool) {
	// Check if it starts with 0x (hex address)
	if !strings.HasPrefix(orderID, "0x") {
		return common.Address{}, false
	}

	// Extract just the address part (before any '-')
	parts := strings.Split(orderID, "-")
	addrStr := parts[0]

	if !common.IsHexAddress(addrStr) {
		return common.Address{}, false
	}

	return common.HexToAddress(addrStr), true
}

// formatHash returns a short hex representation of hash for logging
func formatHash(h consensus.Hash) string {
	// Show first 8 bytes for readability (0xabcd1234...)
	return fmt.Sprintf("0x%x", h[:8])
}

// computeStateHash computes a deterministic hash of the entire application state.
// Ethereum-style: 0x-prefixed 32-byte hex output.
//
// State components hashed (in order):
//  1. Block height (8 bytes, big-endian) - ensures uniqueness per block
//  2. Block timestamp (8 bytes, big-endian) - additional entropy and ordering
//  3. Orderbook state for each symbol (sorted):
//     - Symbol name
//     - Bid levels (price → qty, sorted high to low)
//     - Ask levels (price → qty, sorted low to high)
//
// Extension points (update this hash when adding features):
//   - [ ] Account balances (address → balance map, sorted by address)
//   - [ ] Open positions (address → position map, sorted by address)
//   - [ ] Funding rate state (last update time, accumulated funding)
//   - [ ] Oracle prices (symbol → price map, sorted by symbol)
//   - [ ] Insurance fund balance
//   - [ ] Validator set and stake amounts
//
// TODO Phase 2: Replace with IAVL or Jellyfish Merkle tree for:
//   - Incremental updates (don't rehash everything)
//   - State proofs (Merkle inclusion/exclusion proofs)
//   - Fast sync (snapshot + proof validation)
func (a *App) computeStateHash(height, timestamp int64) [32]byte {
	h := sha256.New()

	// 1. Include height (ensures hash changes every block even if state unchanged)
	var buf [8]byte
	binary.BigEndian.PutUint64(buf[:], uint64(height))
	h.Write(buf[:])

	// 2. Include timestamp (additional entropy and ordering)
	binary.BigEndian.PutUint64(buf[:], uint64(timestamp))
	h.Write(buf[:])

	// 3. Hash orderbook state
	// Get sorted symbol names for deterministic ordering
	symbols := make([]string, 0, len(a.books))
	for sym := range a.books {
		symbols = append(symbols, sym)
	}
	sort.Strings(symbols)

	// Hash each orderbook in sorted order
	for _, sym := range symbols {
		book := a.books[sym]

		// Write symbol name
		h.Write([]byte(sym))

		// Get sorted price levels from the book
		bidLevels := book.GetBidLevels()
		askLevels := book.GetAskLevels()

		// Hash bid levels (sorted high to low)
		for _, level := range bidLevels {
			binary.BigEndian.PutUint64(buf[:], uint64(level.Price))
			h.Write(buf[:])
			binary.BigEndian.PutUint64(buf[:], uint64(level.Qty))
			h.Write(buf[:])
		}

		// Hash ask levels (sorted low to high)
		for _, level := range askLevels {
			binary.BigEndian.PutUint64(buf[:], uint64(level.Price))
			h.Write(buf[:])
			binary.BigEndian.PutUint64(buf[:], uint64(level.Qty))
			h.Write(buf[:])
		}
	}

	return sha256.Sum256(h.Sum(nil))
}

// ==============================
// Public API Accessors
// ==============================

// GetOrderbook returns the orderbook for a symbol (thread-safe read)
func (a *App) GetOrderbook(symbol string) *core.OrderBook {
	return a.getBook(symbol)
}

// GetMarket returns market details for a symbol
func (a *App) GetMarket(symbol string) (*core.Market, error) {
	return a.registry.GetMarket(symbol)
}

// ListMarkets returns all registered markets
func (a *App) ListMarkets() []*core.Market {
	return a.registry.ListMarkets()
}

// GetAccount returns account details for an address (creates if not exists)
func (a *App) GetAccount(addr common.Address) *core.Account {
	return a.accountManager.GetAccount(addr)
}

// GetMempoolSize returns current mempool transaction count
func (a *App) GetMempoolSize() int {
	return a.mempool.Len()
}

// ==============================
// Agent Delegation Management
// ==============================

// StoreDelegation stores an agent key delegation
func (a *App) StoreDelegation(delegationID string, delegation *crypto.AgentDelegation, signature []byte) {
	a.delegationsMu.Lock()
	defer a.delegationsMu.Unlock()

	a.delegations[delegationID] = &StoredDelegation{
		Delegation: delegation,
		Signature:  signature,
	}

	log.Printf("[app] delegation stored: id=%s wallet=%s agent=%s",
		delegationID, delegation.Wallet.Hex(), delegation.Agent.Hex())
}

// GetDelegation retrieves a delegation by ID
func (a *App) GetDelegation(delegationID string) (*StoredDelegation, bool) {
	a.delegationsMu.RLock()
	defer a.delegationsMu.RUnlock()

	delegation, ok := a.delegations[delegationID]
	return delegation, ok
}
