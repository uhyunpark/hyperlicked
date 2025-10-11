package abci

import (
	"log"
	"sync"

	"github.com/uhyunpark/hyperlicked/pkg/app/core"
	"github.com/uhyunpark/hyperlicked/pkg/consensus"
)

type RequestPrepareProposal struct{ Height, MaxTxBytes int64 }
type ResponsePrepareProposal struct{ Txs [][]byte }
type RequestProcessProposal struct {
	Height int64
	Txs    [][]byte
}
type ResponseProcessProposal struct{ Accept bool }
type RequestFinalizeBlock struct {
	Height    int64
	Timestamp int64 // Unix timestamp in seconds
	Txs       [][]byte
}
type ResponseFinalizeBlock struct {
	Events  []string
	AppHash consensus.Hash // Hash of application state after execution
}

type Application interface {
	PrepareProposal(RequestPrepareProposal) ResponsePrepareProposal
	ProcessProposal(RequestProcessProposal) ResponseProcessProposal
	FinalizeBlock(RequestFinalizeBlock) ResponseFinalizeBlock
}

type Bridge struct{ App Application }

func (b *Bridge) PreparePayload(_ consensus.Block, next consensus.Height) []byte {
	resp := b.App.PrepareProposal(RequestPrepareProposal{Height: int64(next), MaxTxBytes: 1 << 24})
	// naive payload: concat with 0x00 delimiter

	var payload []byte

	for _, tx := range resp.Txs {
		payload = append(payload, tx...)
		payload = append(payload, 0x00)
	}
	return payload
}

func (b *Bridge) OnCommit(committed consensus.Block) consensus.Hash {
	txs := splitPayload(committed.Payload)
	resp := b.App.FinalizeBlock(RequestFinalizeBlock{
		Height:    int64(committed.Height),
		Timestamp: committed.Time.Unix(),
		Txs:       txs,
	})
	return resp.AppHash
}

func splitPayload(p []byte) [][]byte {
	var out [][]byte
	cur := make([]byte, 0, len(p))
	for _, b := range p {
		if b == 0x00 {
			if len(cur) > 0 {
				out = append(out, append([]byte(nil), cur...))
				cur = cur[:0]
			}
			continue
		}
		cur = append(cur, b)
	}
	if len(cur) > 0 {
		out = append(out, append([]byte(nil), cur...))
	}
	return out
}

// --- MockApp using HL-like mempool ordering ---
type MockApp struct {
	mu      sync.Mutex
	mempool *core.Mempool
	commits int
}

func NewMockApp() *MockApp { return &MockApp{mempool: core.NewMempool()} }

func (m *MockApp) PushTx(b []byte) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.mempool.PushRaw(b)
}

func (m *MockApp) PrepareProposal(req RequestPrepareProposal) ResponsePrepareProposal {
	m.mu.Lock()
	defer m.mu.Unlock()
	txs := m.mempool.SelectForProposal(req.MaxTxBytes)
	return ResponsePrepareProposal{Txs: txs}
}

func (m *MockApp) ProcessProposal(_ RequestProcessProposal) ResponseProcessProposal {
	return ResponseProcessProposal{Accept: true}
}

func (m *MockApp) FinalizeBlock(req RequestFinalizeBlock) ResponseFinalizeBlock {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.commits++

	// Compute simple hash based on height + tx count (deterministic for tests)
	var hashInput [16]byte
	hashInput[0] = byte(req.Height >> 56)
	hashInput[1] = byte(req.Height >> 48)
	hashInput[2] = byte(req.Height >> 40)
	hashInput[3] = byte(req.Height >> 32)
	hashInput[4] = byte(req.Height >> 24)
	hashInput[5] = byte(req.Height >> 16)
	hashInput[6] = byte(req.Height >> 8)
	hashInput[7] = byte(req.Height)
	hashInput[8] = byte(len(req.Txs))
	appHash := consensus.Hash{}
	copy(appHash[:], hashInput[:])

	// Quiet logging: only log non-empty blocks
	if len(req.Txs) > 0 {
		log.Printf("[app] FinalizeBlock h=%d txs=%d", req.Height, len(req.Txs))
	}
	return ResponseFinalizeBlock{
		Events:  []string{"commit"},
		AppHash: appHash,
	}
}

func (m *MockApp) CommitCount() int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.commits
}
