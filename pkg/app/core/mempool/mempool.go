package mempool

import (
	"encoding/json"
	"sync"
)

// TxType classifies transactions into HL-like buckets.
type TxType int

const (
	TxNonOrder TxType = iota
	TxCancel
	TxOrderGTC
	TxOrderIOC
)

type Tx struct {
	Type  TxType
	Bytes []byte
}

// ClassifyRaw classifies a raw transaction by parsing JSON envelope.
//
// All transactions must be signed JSON format (EIP-712):
//   {"type": "order", ...}   -> TxOrderGTC (default for orders)
//   {"type": "cancel", ...}  -> TxCancel
//
// Invalid or malformed transactions default to TxOrderGTC for backward compatibility.
func ClassifyRaw(b []byte) TxType {
	// All transactions must be JSON
	if len(b) == 0 || b[0] != '{' {
		// Invalid format - should never happen in production
		// Default to order for graceful degradation
		return TxOrderGTC
	}

	var txEnvelope struct {
		Type string `json:"type"`
	}

	if err := json.Unmarshal(b, &txEnvelope); err != nil {
		// Malformed JSON - default to order
		return TxOrderGTC
	}

	switch txEnvelope.Type {
	case "cancel":
		return TxCancel
	case "order":
		// TODO: Check order.type field for IOC vs GTC distinction
		// For now, all signed orders are classified as GTC
		return TxOrderGTC
	default:
		// Unknown transaction type - default to order
		return TxOrderGTC
	}
}

// Mempool maintains three queues per HL ordering rule:
// (1) Non-order, (2) Cancel, (3) Orders (GTC/IOC)
// Within each bucket, FIFO by proposer admission order.
type Mempool struct {
	mu       sync.Mutex
	nonOrder [][]byte
	cancel   [][]byte
	orders   [][]byte // both GTC/IOC kept together; parser may tag inside the bytes if needed
}

func NewMempool() *Mempool {
	return &Mempool{}
}

// PushRaw classifies and enqueues a tx.
func (m *Mempool) PushRaw(b []byte) {
	cp := append([]byte(nil), b...)
	m.mu.Lock()
	defer m.mu.Unlock()
	switch ClassifyRaw(b) {
	case TxNonOrder:
		m.nonOrder = append(m.nonOrder, cp)
	case TxCancel:
		m.cancel = append(m.cancel, cp)
	default:
		m.orders = append(m.orders, cp)
	}
}

// SelectForProposal returns up to maxBytes worth of txs in HL order,
// removing selected txs from the mempool (prototype semantics).
func (m *Mempool) SelectForProposal(maxBytes int64) [][]byte {
	m.mu.Lock()
	defer m.mu.Unlock()

	var out [][]byte
	var used int64

	pull := func(q *[][]byte) {
		for len(*q) > 0 {
			tx := (*q)[0]
			n := int64(len(tx))
			if maxBytes > 0 && used+n > maxBytes {
				return
			}
			out = append(out, tx)
			used += n
			*q = (*q)[1:]
		}
	}

	// Order: non-order -> cancel -> orders
	pull(&m.nonOrder)
	pull(&m.cancel)
	pull(&m.orders)

	return out
}

// Len returns total pending txs (for tests/metrics if needed).
func (m *Mempool) Len() int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return len(m.nonOrder) + len(m.cancel) + len(m.orders)
}
