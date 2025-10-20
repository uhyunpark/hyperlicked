package mempool

import (
	"testing"
)

func TestClassifyRaw_SignedJSON(t *testing.T) {
	tests := []struct {
		name     string
		tx       string
		expected TxType
	}{
		{
			name:     "signed order JSON",
			tx:       `{"type":"order","order":{"symbol":"BTC-USDT"},"signature":"0x1234"}`,
			expected: TxOrderGTC,
		},
		{
			name:     "signed cancel JSON",
			tx:       `{"type":"cancel","cancel":{"orderId":"0x5678"},"signature":"0xabcd"}`,
			expected: TxCancel,
		},
		{
			name:     "invalid JSON defaults to order",
			tx:       `{"invalid": "json"`,
			expected: TxOrderGTC,
		},
		{
			name:     "non-JSON defaults to order (graceful degradation)",
			tx:       "UNKNOWN:foo",
			expected: TxOrderGTC,
		},
		{
			name:     "empty transaction",
			tx:       "",
			expected: TxOrderGTC,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ClassifyRaw([]byte(tt.tx))
			if got != tt.expected {
				t.Errorf("ClassifyRaw() = %v, want %v", got, tt.expected)
			}
		})
	}
}

func TestMempool_Ordering(t *testing.T) {
	m := NewMempool()

	// Push signed transactions in mixed order
	orderTx1 := `{"type":"order","order":{"symbol":"BTC-USDT","side":1},"signature":"0x1111"}`
	orderTx2 := `{"type":"order","order":{"symbol":"BTC-USDT","side":2},"signature":"0x2222"}`
	orderTx3 := `{"type":"order","order":{"symbol":"ETH-USDT","side":1},"signature":"0x3333"}`
	cancelTx1 := `{"type":"cancel","cancel":{"orderId":"0x1111"},"signature":"0x4444"}`
	cancelTx2 := `{"type":"cancel","cancel":{"orderId":"0x2222"},"signature":"0x5555"}`

	// Push in random order
	m.PushRaw([]byte(orderTx1))  // Order 1
	m.PushRaw([]byte(cancelTx1)) // Cancel 1
	m.PushRaw([]byte(orderTx2))  // Order 2
	m.PushRaw([]byte(cancelTx2)) // Cancel 2
	m.PushRaw([]byte(orderTx3))  // Order 3

	// Select all
	txs := m.SelectForProposal(10000)

	// Expected order: cancels (2) → orders (3)
	// (No non-order transactions in this test)
	if len(txs) != 5 {
		t.Fatalf("expected 5 txs, got %d", len(txs))
	}

	// Check ordering: cancels first, then orders (FIFO within each bucket)
	expectOrder := []string{
		cancelTx1, // Cancel 1 (pushed first)
		cancelTx2, // Cancel 2 (pushed second)
		orderTx1,  // Order 1 (pushed first)
		orderTx2,  // Order 2 (pushed second)
		orderTx3,  // Order 3 (pushed third)
	}

	for i, expected := range expectOrder {
		if string(txs[i]) != expected {
			t.Errorf("tx[%d] mismatch\ngot:  %q\nwant: %q", i, string(txs[i]), expected)
		}
	}
}

func TestMempool_MaxBytes(t *testing.T) {
	m := NewMempool()

	// Push 3 small txs
	m.PushRaw([]byte("N:1")) // 3 bytes
	m.PushRaw([]byte("N:2")) // 3 bytes
	m.PushRaw([]byte("N:3")) // 3 bytes

	// Select with limit
	txs := m.SelectForProposal(6) // Only fits 2 txs

	if len(txs) != 2 {
		t.Errorf("expected 2 txs with maxBytes=6, got %d", len(txs))
	}

	// Remaining should still be in mempool
	if m.Len() != 1 {
		t.Errorf("expected 1 tx remaining, got %d", m.Len())
	}
}
