package core

import (
	"sort"
	"sync"
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
	mu    sync.Mutex
	bids  map[int64][]*Order // price -> FIFO slice
	asks  map[int64][]*Order
	index map[string]struct{} // id -> exists
}

func NewOrderBook() *OrderBook {
	return &OrderBook{
		bids:  make(map[int64][]*Order),
		asks:  make(map[int64][]*Order),
		index: make(map[string]struct{}),
	}
}

func min(a, b int64) int64 {
	if a < b {
		return a
	}
	return b
}
func (ob *OrderBook) bestBid() (int64, bool) {
	var best int64
	ok := false
	for p, lv := range ob.bids {
		if len(lv) == 0 {
			continue
		}
		if !ok || p > best {
			best, ok = p, true
		}
	}
	return best, ok
}
func (ob *OrderBook) bestAsk() (int64, bool) {
	var best int64
	ok := false
	for p, lv := range ob.asks {
		if len(lv) == 0 {
			continue
		}
		if !ok || p < best {
			best, ok = p, true
		}
	}
	return best, ok
}

func (ob *OrderBook) addBid(p int64, o *Order) {
	ob.bids[p] = append(ob.bids[p], o)
	ob.index[o.ID] = struct{}{}
}
func (ob *OrderBook) addAsk(p int64, o *Order) {
	ob.asks[p] = append(ob.asks[p], o)
	ob.index[o.ID] = struct{}{}
}

func (ob *OrderBook) Cancel(id string) bool {
	ob.mu.Lock()
	defer ob.mu.Unlock()
	if _, ok := ob.index[id]; !ok {
		return false
	}

	// naive linear search
	for p, arr := range ob.bids {
		for i, o := range arr {
			if o.ID == id {
				ob.bids[p] = append(arr[:i], arr[i+1:]...)
				if len(ob.bids[p]) == 0 {
					delete(ob.bids, p)
				}
				delete(ob.index, id)
				return true
			}
		}
	}
	for p, arr := range ob.asks {
		for i, o := range arr {
			if o.ID == id {
				ob.asks[p] = append(arr[:i], arr[i+1:]...)
				if len(ob.asks[p]) == 0 {
					delete(ob.asks, p)
				}
				delete(ob.index, id)
				return true
			}
		}
	}
	return false
}

// Place matches IOC/GTC by price-time. Remaining qty rests only if GTC.
func (ob *OrderBook) Place(o *Order) []Fill {
	ob.mu.Lock()
	defer ob.mu.Unlock()

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
				continue
			}
			maker := level[0]
			match := min(o.Qty, maker.Qty)
			o.Qty -= match
			maker.Qty -= match
			fills = append(fills, Fill{TakerID: o.ID, MakerID: maker.ID, Price: askP, Qty: match})
			if maker.Qty == 0 {
				ob.asks[askP] = level[1:]
				delete(ob.index, maker.ID)
				if len(ob.asks[askP]) == 0 {
					delete(ob.asks, askP)
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
				continue
			}
			maker := level[0]
			match := min(o.Qty, maker.Qty)
			o.Qty -= match
			maker.Qty -= match
			fills = append(fills, Fill{TakerID: o.ID, MakerID: maker.ID, Price: bidP, Qty: match})
			if maker.Qty == 0 {
				ob.bids[bidP] = level[1:]
				delete(ob.index, maker.ID)
				if len(ob.bids[bidP]) == 0 {
					delete(ob.bids, bidP)
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
	return fills
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
