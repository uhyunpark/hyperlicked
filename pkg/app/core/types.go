package core

type Side int8

const (
	Buy  Side = 1
	Sell Side = -1
)

type Order struct {
	ID       string
	Symbol   string
	Side     Side
	Price    int64  // integer ticks
	Qty      int64  // integer lots
	Type     string // "GTC" or "IOC"
	OwnerHex string // optional owner address (0x...)
}
