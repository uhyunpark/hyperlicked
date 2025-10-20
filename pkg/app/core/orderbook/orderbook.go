package orderbook

import (
	"container/heap"
	"sort"
	"sync"

	"github.com/uhyunpark/hyperlicked/pkg/app/core/market"
)

type Fill struct {
	TakerID string
	MakerID string
	Price   int64
	Qty     int64
}

type PriceLevel struct {
	Price int64
	Qty   int64 // total qty at this price level
}

type OrderBook struct {
	mu sync.RWMutex // Changed to RWMutex for concurrent reads

	// Heap-based best price tracking (O(1) peek)
	bidHeap *MaxPriceHeap
	askHeap *MinPriceHeap

	// Price level queues (FIFO matching at each price)
	bids map[int64][]*Order // price -> FIFO slice
	asks map[int64][]*Order

	// Order index for O(1) cancellation
	orderIndex map[string]int64    // order ID -> price
	index      map[string]struct{} // id -> exists (keep for compatibility)

	lastPrice int64 // most recent fill price (for mark price fallback)
}

func NewOrderBook() *OrderBook {
	bidHeap := &MaxPriceHeap{}
	askHeap := &MinPriceHeap{}
	heap.Init(bidHeap)
	heap.Init(askHeap)

	return &OrderBook{
		bidHeap:    bidHeap,
		askHeap:    askHeap,
		bids:       make(map[int64][]*Order),
		asks:       make(map[int64][]*Order),
		orderIndex: make(map[string]int64),
		index:      make(map[string]struct{}),
		lastPrice:  0,
	}
}

func min(a, b int64) int64 {
	if a < b {
		return a
	}
	return b
}
// bestBid returns the highest bid price (O(1) with heap)
func (ob *OrderBook) bestBid() (int64, bool) {
	if ob.bidHeap.Len() == 0 {
		return 0, false
	}
	return ob.bidHeap.Peek(), true
}
// bestAsk returns the lowest ask price (O(1) with heap)
func (ob *OrderBook) bestAsk() (int64, bool) {
	if ob.askHeap.Len() == 0 {
		return 0, false
	}
	return ob.askHeap.Peek(), true
}

func (ob *OrderBook) addBid(p int64, o *Order) {
	// Add to price level queue
	if len(ob.bids[p]) == 0 {
		// New price level - add to heap
		heap.Push(ob.bidHeap, p)
	}
	ob.bids[p] = append(ob.bids[p], o)

	// Index for O(1) cancellation
	ob.orderIndex[o.ID] = p
	ob.index[o.ID] = struct{}{}
}
func (ob *OrderBook) addAsk(p int64, o *Order) {
	// Add to price level queue
	if len(ob.asks[p]) == 0 {
		// New price level - add to heap
		heap.Push(ob.askHeap, p)
	}
	ob.asks[p] = append(ob.asks[p], o)

	// Index for O(1) cancellation
	ob.orderIndex[o.ID] = p
	ob.index[o.ID] = struct{}{}
}

func (ob *OrderBook) Cancel(id string) bool {
	ob.mu.Lock()
	defer ob.mu.Unlock()

	// O(1) lookup via orderIndex
	price, ok := ob.orderIndex[id]
	if !ok {
		return false
	}

	// Try bids first
	if arr, exists := ob.bids[price]; exists {
		for i, o := range arr {
			if o.ID == id {
				// Remove from FIFO queue
				ob.bids[price] = append(arr[:i], arr[i+1:]...)

				// If price level is now empty, remove from heap and map
				if len(ob.bids[price]) == 0 {
					delete(ob.bids, price)
					ob.removeFromBidHeap(price)
				}

				delete(ob.orderIndex, id)
				delete(ob.index, id)
				return true
			}
		}
	}

	// Try asks
	if arr, exists := ob.asks[price]; exists {
		for i, o := range arr {
			if o.ID == id {
				// Remove from FIFO queue
				ob.asks[price] = append(arr[:i], arr[i+1:]...)

				// If price level is now empty, remove from heap and map
				if len(ob.asks[price]) == 0 {
					delete(ob.asks, price)
					ob.removeFromAskHeap(price)
				}

				delete(ob.orderIndex, id)
				delete(ob.index, id)
				return true
			}
		}
	}

	return false
}

// removeFromBidHeap removes a price level from the bid heap (O(N) worst case, but rare)
func (ob *OrderBook) removeFromBidHeap(price int64) {
	for i := 0; i < ob.bidHeap.Len(); i++ {
		if (*ob.bidHeap)[i] == price {
			heap.Remove(ob.bidHeap, i)
			return
		}
	}
}

// removeFromAskHeap removes a price level from the ask heap (O(N) worst case, but rare)
func (ob *OrderBook) removeFromAskHeap(price int64) {
	for i := 0; i < ob.askHeap.Len(); i++ {
		if (*ob.askHeap)[i] == price {
			heap.Remove(ob.askHeap, i)
			return
		}
	}
}

// Place matches IOC/GTC by price-time. Remaining qty rests only if GTC.
// Validates order against market parameters before matching.
// Returns error if order violates market rules (invalid tick/lot size, min notional, etc.)
func (ob *OrderBook) Place(o *Order, mkt *market.Market) ([]Fill, error) {
	ob.mu.Lock()
	defer ob.mu.Unlock()

	// Validate order against market parameters
	if err := mkt.ValidateOrder(o.Price, o.Qty); err != nil {
		return nil, err
	}

	var fills []Fill

	if o.Side == Buy {
		for o.Qty > 0 {
			askP, ok := ob.bestAsk()
			if !ok || askP > o.Price {
				break
			}
			level := ob.asks[askP]
			if len(level) == 0 {
				delete(ob.asks, askP)
				ob.removeFromAskHeap(askP)
				continue
			}
			maker := level[0]
			match := min(o.Qty, maker.Qty)
			o.Qty -= match
			maker.Qty -= match
			fills = append(fills, Fill{TakerID: o.ID, MakerID: maker.ID, Price: askP, Qty: match})
			ob.lastPrice = askP // Update last traded price
			if maker.Qty == 0 {
				ob.asks[askP] = level[1:]
				delete(ob.index, maker.ID)
				delete(ob.orderIndex, maker.ID)
				if len(ob.asks[askP]) == 0 {
					delete(ob.asks, askP)
					ob.removeFromAskHeap(askP)
				}
			} else {
				ob.asks[askP][0] = maker
			}
		}
		if o.Qty > 0 && o.Type == "GTC" {
			cp := *o
			ob.addBid(o.Price, &cp)
		}
	} else { // Sell
		for o.Qty > 0 {
			bidP, ok := ob.bestBid()
			if !ok || bidP < o.Price {
				break
			}
			level := ob.bids[bidP]
			if len(level) == 0 {
				delete(ob.bids, bidP)
				ob.removeFromBidHeap(bidP)
				continue
			}
			maker := level[0]
			match := min(o.Qty, maker.Qty)
			o.Qty -= match
			maker.Qty -= match
			fills = append(fills, Fill{TakerID: o.ID, MakerID: maker.ID, Price: bidP, Qty: match})
			ob.lastPrice = bidP // Update last traded price
			if maker.Qty == 0 {
				ob.bids[bidP] = level[1:]
				delete(ob.index, maker.ID)
				delete(ob.orderIndex, maker.ID)
				if len(ob.bids[bidP]) == 0 {
					delete(ob.bids, bidP)
					ob.removeFromBidHeap(bidP)
				}
			} else {
				ob.bids[bidP][0] = maker
			}
		}
		if o.Qty > 0 && o.Type == "GTC" {
			cp := *o
			ob.addAsk(o.Price, &cp)
		}
	}
	return fills, nil
}

// GetBidLevels returns all bid price levels sorted high to low (best bid first).
// Used for state hashing - aggregates qty across all orders at each price.
func (ob *OrderBook) GetBidLevels() []PriceLevel {
	ob.mu.Lock()
	defer ob.mu.Unlock()

	var levels []PriceLevel
	for price, orders := range ob.bids {
		if len(orders) == 0 {
			continue
		}
		var totalQty int64
		for _, o := range orders {
			totalQty += o.Qty
		}
		levels = append(levels, PriceLevel{Price: price, Qty: totalQty})
	}

	// Sort high to low (best bid = highest price first)
	sort.Slice(levels, func(i, j int) bool {
		return levels[i].Price > levels[j].Price
	})

	return levels
}

// GetAskLevels returns all ask price levels sorted low to high (best ask first).
// Used for state hashing - aggregates qty across all orders at each price.
func (ob *OrderBook) GetAskLevels() []PriceLevel {
	ob.mu.Lock()
	defer ob.mu.Unlock()

	var levels []PriceLevel
	for price, orders := range ob.asks {
		if len(orders) == 0 {
			continue
		}
		var totalQty int64
		for _, o := range orders {
			totalQty += o.Qty
		}
		levels = append(levels, PriceLevel{Price: price, Qty: totalQty})
	}

	// Sort low to high (best ask = lowest price first)
	sort.Slice(levels, func(i, j int) bool {
		return levels[i].Price < levels[j].Price
	})

	return levels
}

// GetMidPrice returns the mid-market price (average of best bid and best ask)
// Returns 0 if orderbook is empty or one-sided
// Used as fallback mark price when oracle is unavailable
func (ob *OrderBook) GetMidPrice() int64 {
	ob.mu.Lock()
	defer ob.mu.Unlock()

	if len(ob.bids) == 0 || len(ob.asks) == 0 {
		return 0
	}

	// Get best bid (highest price)
	bestBid := int64(0)
	for price := range ob.bids {
		if price > bestBid {
			bestBid = price
		}
	}

	// Get best ask (lowest price)
	bestAsk := int64(0)
	for price := range ob.asks {
		if bestAsk == 0 || price < bestAsk {
			bestAsk = price
		}
	}

	if bestBid == 0 || bestAsk == 0 {
		return 0
	}

	return (bestBid + bestAsk) / 2
}

// GetLastPrice returns the price of the most recent fill
// Returns 0 if no trades have occurred
func (ob *OrderBook) GetLastPrice() int64 {
	ob.mu.Lock()
	defer ob.mu.Unlock()
	return ob.lastPrice
}

// GetBestBid returns the highest bid price
// Returns 0 if no bids
func (ob *OrderBook) GetBestBid() int64 {
	ob.mu.Lock()
	defer ob.mu.Unlock()

	bestBid := int64(0)
	for price := range ob.bids {
		if price > bestBid {
			bestBid = price
		}
	}
	return bestBid
}

// GetBestAsk returns the lowest ask price
// Returns 0 if no asks
func (ob *OrderBook) GetBestAsk() int64 {
	ob.mu.Lock()
	defer ob.mu.Unlock()

	bestAsk := int64(0)
	for price := range ob.asks {
		if bestAsk == 0 || price < bestAsk {
			bestAsk = price
		}
	}
	return bestAsk
}
