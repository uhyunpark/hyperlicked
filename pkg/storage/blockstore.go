package storage

import (
	"sync"

	"github.com/uhyunpark/hyperlicked/pkg/consensus"
)

type InMemoryBlockStore struct {
	mu         sync.Mutex
	blocks     map[consensus.Hash]consensus.Block
	certByView map[consensus.View]consensus.Certificate
	committed  *consensus.Hash
}

func NewInMemoryBlockStore() *InMemoryBlockStore {
	return &InMemoryBlockStore{
		blocks:     make(map[consensus.Hash]consensus.Block),
		certByView: make(map[consensus.View]consensus.Certificate),
	}
}

func (s *InMemoryBlockStore) SaveBlock(b consensus.Block) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.blocks[consensus.HashOfBlock(b)] = b
}

func (s *InMemoryBlockStore) GetBlock(h consensus.Hash) (consensus.Block, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	b, ok := s.blocks[h]
	return b, ok
}

func (s *InMemoryBlockStore) SaveCert(c consensus.Certificate) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.certByView[c.View] = c
}

func (s *InMemoryBlockStore) GetCert(v consensus.View) (consensus.Certificate, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	c, ok := s.certByView[v]
	return c, ok
}

func (s *InMemoryBlockStore) SetCommitted(h consensus.Hash) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.committed = &h
}

func (s *InMemoryBlockStore) GetCommitted() (consensus.Hash, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.committed == nil {
		return consensus.Hash{}, false
	}
	return *s.committed, true
}
