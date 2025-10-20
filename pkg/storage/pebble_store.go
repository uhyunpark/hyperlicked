package storage

import (
	"encoding/json"
	"fmt"

	"github.com/cockroachdb/pebble"
	"github.com/ethereum/go-ethereum/common"

	"github.com/uhyunpark/hyperlicked/pkg/app/core/account"
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

// ============================================================================
// Account Persistence Methods
// ============================================================================

// SaveAccount persists an account to Pebble
func (s *PebbleStore) SaveAccount(acc *account.Account) error {
	data, err := json.Marshal(acc)
	if err != nil {
		return fmt.Errorf("failed to marshal account: %w", err)
	}

	key := accountKey(acc.Address)
	if err := s.db.Set(key, data, pebble.Sync); err != nil {
		return fmt.Errorf("failed to save account: %w", err)
	}

	return nil
}

// LoadAccount loads an account from Pebble
// Returns nil if account doesn't exist
func (s *PebbleStore) LoadAccount(addr common.Address) (*account.Account, error) {
	key := accountKey(addr)
	data, closer, err := s.db.Get(key)
	if err == pebble.ErrNotFound {
		return nil, nil // Account doesn't exist
	}
	if err != nil {
		return nil, fmt.Errorf("failed to get account: %w", err)
	}
	defer closer.Close()

	var acc account.Account
	if err := json.Unmarshal(data, &acc); err != nil {
		return nil, fmt.Errorf("failed to unmarshal account: %w", err)
	}

	// Initialize maps if nil
	if acc.Positions == nil {
		acc.Positions = make(map[string]*account.Position)
	}

	return &acc, nil
}

// SavePosition persists a position to Pebble
func (s *PebbleStore) SavePosition(addr common.Address, pos *account.Position) error {
	data, err := json.Marshal(pos)
	if err != nil {
		return fmt.Errorf("failed to marshal position: %w", err)
	}

	key := positionKey(addr, pos.Symbol)
	if err := s.db.Set(key, data, pebble.Sync); err != nil {
		return fmt.Errorf("failed to save position: %w", err)
	}

	return nil
}

// LoadPosition loads a position from Pebble
func (s *PebbleStore) LoadPosition(addr common.Address, symbol string) (*account.Position, error) {
	key := positionKey(addr, symbol)
	data, closer, err := s.db.Get(key)
	if err == pebble.ErrNotFound {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("failed to get position: %w", err)
	}
	defer closer.Close()

	var pos account.Position
	if err := json.Unmarshal(data, &pos); err != nil {
		return nil, fmt.Errorf("failed to unmarshal position: %w", err)
	}

	return &pos, nil
}

// LoadAllPositions loads all positions for an account
func (s *PebbleStore) LoadAllPositions(addr common.Address) (map[string]*account.Position, error) {
	prefix := positionPrefix(addr)
	iter, _ := s.db.NewIter(&pebble.IterOptions{
		LowerBound: prefix,
		UpperBound: keyUpperBound(prefix),
	})
	defer iter.Close()

	positions := make(map[string]*account.Position)
	for iter.First(); iter.Valid(); iter.Next() {
		var pos account.Position
		if err := json.Unmarshal(iter.Value(), &pos); err != nil {
			continue // Skip invalid entries
		}
		positions[pos.Symbol] = &pos
	}

	return positions, nil
}

// SaveOrder persists an order to Pebble
func (s *PebbleStore) SaveOrder(order *account.Order) error {
	data, err := json.Marshal(order)
	if err != nil {
		return fmt.Errorf("failed to marshal order: %w", err)
	}

	key := orderKey(order.Owner, order.ID)
	if err := s.db.Set(key, data, pebble.Sync); err != nil {
		return fmt.Errorf("failed to save order: %w", err)
	}

	return nil
}

// DeleteOrder removes an order from Pebble
func (s *PebbleStore) DeleteOrder(addr common.Address, orderID string) error {
	key := orderKey(addr, orderID)
	if err := s.db.Delete(key, pebble.Sync); err != nil {
		return fmt.Errorf("failed to delete order: %w", err)
	}
	return nil
}

// LoadOpenOrders loads all open orders for an account
func (s *PebbleStore) LoadOpenOrders(addr common.Address) ([]*account.Order, error) {
	prefix := orderPrefix(addr)
	iter, _ := s.db.NewIter(&pebble.IterOptions{
		LowerBound: prefix,
		UpperBound: keyUpperBound(prefix),
	})
	defer iter.Close()

	var orders []*account.Order
	for iter.First(); iter.Valid(); iter.Next() {
		var order account.Order
		if err := json.Unmarshal(iter.Value(), &order); err != nil {
			continue
		}
		if !order.IsClosed() {
			orders = append(orders, &order)
		}
	}

	return orders, nil
}

// SaveTrade persists a trade to Pebble
func (s *PebbleStore) SaveTrade(trade *account.Trade) error {
	data, err := json.Marshal(trade)
	if err != nil {
		return fmt.Errorf("failed to marshal trade: %w", err)
	}

	key := tradeKey(trade.Symbol, trade.Timestamp, trade.ID)
	if err := s.db.Set(key, data, pebble.NoSync); err != nil {
		return fmt.Errorf("failed to save trade: %w", err)
	}

	return nil
}

// LoadRecentTrades loads the most recent N trades for a symbol
func (s *PebbleStore) LoadRecentTrades(symbol string, limit int) ([]*account.Trade, error) {
	prefix := tradePrefix(symbol)
	iter, _ := s.db.NewIter(&pebble.IterOptions{
		LowerBound: prefix,
		UpperBound: keyUpperBound(prefix),
	})
	defer iter.Close()

	var trades []*account.Trade
	for iter.Last(); iter.Valid() && len(trades) < limit; iter.Prev() {
		var trade account.Trade
		if err := json.Unmarshal(iter.Value(), &trade); err != nil {
			continue
		}
		trades = append(trades, &trade)
	}

	return trades, nil
}
