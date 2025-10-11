// file: tests/engine_e2e_test.go
package tests

import (
	"sync"

	"github.com/uhyunpark/hyperlicked/pkg/consensus"
)

// mockStore implements consensus.BlockStore for testing
type mockStore struct {
	mu         sync.Mutex
	blocks     map[consensus.Hash]consensus.Block
	certByView map[consensus.View]consensus.Certificate
	committed  *consensus.Hash
}

func (s *mockStore) SaveBlock(b consensus.Block) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.blocks[consensus.HashOfBlock(b)] = b
}

func (s *mockStore) GetBlock(h consensus.Hash) (consensus.Block, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	b, ok := s.blocks[h]
	return b, ok
}

func (s *mockStore) SaveCert(c consensus.Certificate) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.certByView[c.View] = c
}

func (s *mockStore) GetCert(v consensus.View) (consensus.Certificate, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	c, ok := s.certByView[v]
	return c, ok
}

func (s *mockStore) SetCommitted(h consensus.Hash) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.committed = &h
}

func (s *mockStore) GetCommitted() (consensus.Hash, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.committed == nil {
		return consensus.Hash{}, false
	}
	return *s.committed, true
}

// TestEngineTwoViewsCommit: Removed - HotStuff requires minimum 4 validators (N=3f+1, f=1)
// Single-validator mode is not part of HotStuff standard
// See TestFourValidators for proper multi-validator consensus test
