package consensus

import "sync"

type Safety struct {
	state  *State
	blocks map[Hash]Block
	mu     sync.RWMutex // Protects blocks map from concurrent access
}

func NewSafety(s *State) *Safety {
	st := &Safety{state: s, blocks: make(map[Hash]Block)}
	gen := s.Genesis
	st.blocks[HashOfBlock(gen)] = gen
	return st
}

func (s *Safety) HighestCert() Certificate {
	if s.state.HighCert != nil {
		return *s.state.HighCert
	}
	// Genesis certificate with zero AppHash (no state before genesis)
	return Certificate{View: 0, H: HashOfBlock(s.state.Genesis), AppHash: Hash{}, Sig: nil}
}

func (s *Safety) HighestDouble() *DoubleCert { return nil }

// OnPrepare: 새 Prepare QC 관찰 시 HighestQC 갱신, 블록(있으면) 보관
func (s *Safety) OnPrepare(cert Certificate, b Block) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if b.Height > 0 || b.Proposer != "" {
		s.blocks[HashOfBlock(b)] = b
	}
	if s.state.HighCert == nil || cert.View > s.state.HighCert.View {
		c := cert
		s.state.HighCert = &c
	}
}

func (s *Safety) UpdateLock(cert Certificate, b Block) {
	s.mu.Lock()
	defer s.mu.Unlock()

	s.state.HighCert = &cert
	s.blocks[HashOfBlock(b)] = b
	s.state.Locked = &Locked{Block: b, Cert: cert}
}

func (s *Safety) CanVote(p Propose) bool {
	if s.state.Locked == nil {
		return true
	}
	return p.HighCert.View >= s.state.Locked.Cert.View
}

func (s *Safety) BlockByHash(h Hash) (Block, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	b, ok := s.blocks[h]
	return b, ok
}

// FastPath: 직전 뷰 QC를 봤다면 다음 뷰에서 대기 생략 가능
func (s *Safety) FastPathReady(v View) bool {
	if s.state.HighCert == nil {
		return false
	}
	return s.state.HighCert.View+1 >= v
}
