package perp

import (
	"crypto/sha256"
	"encoding/binary"
	"log"
	"sort"
	"strconv"
	"strings"

	"github.com/uhyunpark/hyperlicked/pkg/abci"
	"github.com/uhyunpark/hyperlicked/pkg/app/core"
)

type App struct {
	mempool *core.Mempool
	books   map[string]*core.OrderBook
}

func NewApp() *App {
	return &App{
		mempool: core.NewMempool(),
		books:   map[string]*core.OrderBook{"PERP-USD": core.NewOrderBook()},
	}
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
	totalFills := 0
	for _, tx := range req.Txs {
		totalFills += a.applyTx(string(tx))
	}

	// Compute state hash after executing all transactions (includes height, timestamp, orderbook state)
	appHash := a.computeStateHash(req.Height, req.Timestamp)

	// Quiet logging: only log non-empty blocks
	if len(req.Txs) > 0 || totalFills > 0 {
		log.Printf("[app] FinalizeBlock: txs=%d fills=%d apphash=0x%x", len(req.Txs), totalFills, appHash[:])
	}

	return abci.ResponseFinalizeBlock{
		Events:  []string{"commit"},
		AppHash: appHash,
	}
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
		fills := a.getBook(sym).Place(o)

		for _, f := range fills {
			log.Printf("[fill] %s taker=%s maker=%s px=%d qty=%d", sym, f.TakerID, f.MakerID, f.Price, f.Qty)
		}

		return len(fills)
	}

	log.Printf("[app] unknown tx: %s", s)
	return 0
}

// computeStateHash computes a deterministic hash of the entire application state.
// Ethereum-style: 0x-prefixed 32-byte hex output.
//
// State components hashed (in order):
//   1. Block height (8 bytes, big-endian) - ensures uniqueness per block
//   2. Block timestamp (8 bytes, big-endian) - additional entropy and ordering
//   3. Orderbook state for each symbol (sorted):
//      - Symbol name
//      - Bid levels (price → qty, sorted high to low)
//      - Ask levels (price → qty, sorted low to high)
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
