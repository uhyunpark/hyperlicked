package core

type Position struct {
	Symbol     string
	Size       int64 // positive = long, negative = short
	EntryPrice int64 // VWAP entry, integer ticks
}

type Account struct {
	Addr      string
	Balance   int64 // e.g. USDC cents
	Positions map[string]*Position
}

func NewAccount(addr string, bal int64) *Account {
	return &Account{
		Addr: addr, Balance: bal,
		Positions: make(map[string]*Position),
	}
}

// Update on fill
func (a *Account) ApplyFill(sym string, side Side, price, qty int64) {
	pos := a.Positions[sym]
	if pos == nil {
		pos = &Position{Symbol: sym}
		a.Positions[sym] = pos
	}
	// naive VWAP update
	if side == Buy {
		newSize := pos.Size + qty
		if pos.Size >= 0 {
			// same direction
			pos.EntryPrice = (pos.EntryPrice*pos.Size + price*qty) / newSize
		} else {
			// reducing short
			// profit/loss realized here (later extend)
		}
		pos.Size = newSize
	} else {
		newSize := pos.Size - qty
		if pos.Size <= 0 {
			pos.EntryPrice = (pos.EntryPrice*(-pos.Size) + price*qty) / (-newSize)
		} else {
			// reducing long
		}
		pos.Size = newSize
	}
}
