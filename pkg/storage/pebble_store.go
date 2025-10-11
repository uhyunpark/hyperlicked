package storage

import (
	"fmt"

	"github.com/cockroachdb/pebble"

	"github.com/uhyunpark/hyperlicked/pkg/consensus"
)

type PebbleStore struct {
	db *pebble.DB
}

func NewPebbleStore(path string) (*PebbleStore, error) {
	db, err := pebble.Open(path, &pebble.Options{})
	if err != nil {
		return nil, err
	}
	return &PebbleStore{db: db}, nil
}
func (s *PebbleStore) Close() error { return s.db.Close() }

// keys: b:<32-byte-hash>, c:<8-byte-view>, cm:committed
func kBlock(h consensus.Hash) []byte { return append([]byte("b:"), h[:]...) }
func kCert(v consensus.View) []byte  { return append([]byte("c:"), viewKey(v)...) }
func kCommitted() []byte             { return []byte("cm") }

func (s *PebbleStore) SaveBlock(b consensus.Block) {
	key := kBlock(consensus.HashOfBlock(b))
	val, err := encodeGob(b)
	if err != nil {
		panic(fmt.Errorf("encode block: %w", err))
	}
	if err := s.db.Set(key, val, pebble.Sync); err != nil {
		panic(err)
	}
}

func (s *PebbleStore) GetBlock(h consensus.Hash) (consensus.Block, bool) {
	val, closer, err := s.db.Get(kBlock(h))
	if err != nil {
		if err == pebble.ErrNotFound {
			return consensus.Block{}, false
		}
		panic(err)
	}
	defer closer.Close()
	var out consensus.Block
	if err := decodeGob(val, &out); err != nil {
		panic(err)
	}
	return out, true
}

func (s *PebbleStore) SaveCert(c consensus.Certificate) {
	val, err := encodeGob(c)
	if err != nil {
		panic(fmt.Errorf("encode cert: %w", err))
	}
	if err := s.db.Set(kCert(c.View), val, pebble.Sync); err != nil {
		panic(err)
	}
}

func (s *PebbleStore) GetCert(v consensus.View) (consensus.Certificate, bool) {
	val, closer, err := s.db.Get(kCert(v))
	if err != nil {
		if err == pebble.ErrNotFound {
			return consensus.Certificate{}, false
		}
		panic(err)
	}
	defer closer.Close()
	var out consensus.Certificate
	if err := decodeGob(val, &out); err != nil {
		panic(err)
	}
	return out, true
}

func (s *PebbleStore) SetCommitted(h consensus.Hash) {
	if err := s.db.Set(kCommitted(), h[:], pebble.Sync); err != nil {
		panic(err)
	}
}

func (s *PebbleStore) GetCommitted() (consensus.Hash, bool) {
	val, closer, err := s.db.Get(kCommitted())
	if err != nil {
		if err == pebble.ErrNotFound {
			return consensus.Hash{}, false
		}
		panic(err)
	}
	defer closer.Close()
	var out consensus.Hash
	copy(out[:], val)
	return out, true
}

var _ consensus.BlockStore = (*PebbleStore)(nil)
