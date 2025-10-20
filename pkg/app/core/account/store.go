package account

import (
	"encoding/json"
	"fmt"

	"github.com/cockroachdb/pebble"
	"github.com/ethereum/go-ethereum/common"
)

// Store provides Pebble-based persistence for accounts, positions, orders, and trades
// Thread-safe: all operations go through AccountManager's mutex
type Store struct {
	db *pebble.DB
}

// NewStore opens a Pebble database at the given path
func NewStore(dbPath string) (*Store, error) {
	opts := &pebble.Options{
		// Performance tuning
		Cache:                       pebble.NewCache(128 << 20), // 128MB cache
		MemTableSize:                64 << 20,                   // 64MB memtable
		MaxConcurrentCompactions:    func() int { return 3 },
		L0CompactionThreshold:       2,
		L0StopWritesThreshold:       12,
		LBaseMaxBytes:               64 << 20, // 64MB
		MaxOpenFiles:                1000,
		BytesPerSync:                512 << 10, // 512KB
		DisableAutomaticCompactions: false,
	}

	db, err := pebble.Open(dbPath, opts)
	if err != nil {
		return nil, fmt.Errorf("failed to open pebble db at %s: %w", dbPath, err)
	}

	return &Store{db: db}, nil
}

// Close closes the database
func (s *Store) Close() error {
	return s.db.Close()
}

// SaveAccount persists an account to Pebble
func (s *Store) SaveAccount(acc *Account) error {
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
func (s *Store) LoadAccount(addr common.Address) (*Account, error) {
	key := accountKey(addr)
	data, closer, err := s.db.Get(key)
	if err == pebble.ErrNotFound {
		return nil, nil // Account doesn't exist
	}
	if err != nil {
		return nil, fmt.Errorf("failed to get account: %w", err)
	}
	defer closer.Close()

	var acc Account
	if err := json.Unmarshal(data, &acc); err != nil {
		return nil, fmt.Errorf("failed to unmarshal account: %w", err)
	}

	// Initialize maps if nil (JSON unmarshal may leave them nil)
	if acc.Positions == nil {
		acc.Positions = make(map[string]*Position)
	}

	return &acc, nil
}

// SavePosition persists a position to Pebble
func (s *Store) SavePosition(addr common.Address, pos *Position) error {
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
// Returns nil if position doesn't exist
func (s *Store) LoadPosition(addr common.Address, symbol string) (*Position, error) {
	key := positionKey(addr, symbol)
	data, closer, err := s.db.Get(key)
	if err == pebble.ErrNotFound {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("failed to get position: %w", err)
	}
	defer closer.Close()

	var pos Position
	if err := json.Unmarshal(data, &pos); err != nil {
		return nil, fmt.Errorf("failed to unmarshal position: %w", err)
	}

	return &pos, nil
}

// LoadAllPositions loads all positions for an account
func (s *Store) LoadAllPositions(addr common.Address) (map[string]*Position, error) {
	prefix := positionPrefix(addr)
	iter, _ := s.db.NewIter(&pebble.IterOptions{
		LowerBound: prefix,
		UpperBound: keyUpperBound(prefix),
	})
	defer iter.Close()

	positions := make(map[string]*Position)
	for iter.First(); iter.Valid(); iter.Next() {
		var pos Position
		if err := json.Unmarshal(iter.Value(), &pos); err != nil {
			continue // Skip invalid entries
		}
		positions[pos.Symbol] = &pos
	}

	return positions, nil
}

// SaveOrder persists an order to Pebble
func (s *Store) SaveOrder(order *Order) error {
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
func (s *Store) DeleteOrder(addr common.Address, orderID string) error {
	key := orderKey(addr, orderID)
	if err := s.db.Delete(key, pebble.Sync); err != nil {
		return fmt.Errorf("failed to delete order: %w", err)
	}
	return nil
}

// LoadOrder loads an order from Pebble
// Returns nil if order doesn't exist
func (s *Store) LoadOrder(addr common.Address, orderID string) (*Order, error) {
	key := orderKey(addr, orderID)
	data, closer, err := s.db.Get(key)
	if err == pebble.ErrNotFound {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("failed to get order: %w", err)
	}
	defer closer.Close()

	var order Order
	if err := json.Unmarshal(data, &order); err != nil {
		return nil, fmt.Errorf("failed to unmarshal order: %w", err)
	}

	return &order, nil
}

// LoadOpenOrders loads all open orders for an account
func (s *Store) LoadOpenOrders(addr common.Address) ([]*Order, error) {
	prefix := orderPrefix(addr)
	iter, _ := s.db.NewIter(&pebble.IterOptions{
		LowerBound: prefix,
		UpperBound: keyUpperBound(prefix),
	})
	defer iter.Close()

	var orders []*Order
	for iter.First(); iter.Valid(); iter.Next() {
		var order Order
		if err := json.Unmarshal(iter.Value(), &order); err != nil {
			continue // Skip invalid entries
		}
		if !order.IsClosed() {
			orders = append(orders, &order)
		}
	}

	return orders, nil
}

// SaveTrade persists a trade to Pebble
func (s *Store) SaveTrade(trade *Trade) error {
	data, err := json.Marshal(trade)
	if err != nil {
		return fmt.Errorf("failed to marshal trade: %w", err)
	}

	key := tradeKey(trade.Symbol, trade.Timestamp, trade.ID)
	if err := s.db.Set(key, data, pebble.NoSync); err != nil { // NoSync for trades (batched writes)
		return fmt.Errorf("failed to save trade: %w", err)
	}

	return nil
}

// LoadRecentTrades loads the most recent N trades for a symbol
// Trades are returned in reverse chronological order (newest first)
func (s *Store) LoadRecentTrades(symbol string, limit int) ([]*Trade, error) {
	prefix := tradePrefix(symbol)

	// Use reverse iterator to get newest trades first
	iter, _ := s.db.NewIter(&pebble.IterOptions{
		LowerBound: prefix,
		UpperBound: keyUpperBound(prefix),
	})
	defer iter.Close()

	var trades []*Trade
	for iter.Last(); iter.Valid() && len(trades) < limit; iter.Prev() {
		var trade Trade
		if err := json.Unmarshal(iter.Value(), &trade); err != nil {
			continue // Skip invalid entries
		}
		trades = append(trades, &trade)
	}

	return trades, nil
}

// BatchWrite provides atomic batch writes for multiple operations
type BatchWrite struct {
	batch *pebble.Batch
	store *Store
}

// NewBatch creates a new batch writer
func (s *Store) NewBatch() *BatchWrite {
	return &BatchWrite{
		batch: s.db.NewBatch(),
		store: s,
	}
}

// SaveAccount adds account save to batch
func (bw *BatchWrite) SaveAccount(acc *Account) error {
	data, err := json.Marshal(acc)
	if err != nil {
		return err
	}
	return bw.batch.Set(accountKey(acc.Address), data, nil)
}

// SavePosition adds position save to batch
func (bw *BatchWrite) SavePosition(addr common.Address, pos *Position) error {
	data, err := json.Marshal(pos)
	if err != nil {
		return err
	}
	return bw.batch.Set(positionKey(addr, pos.Symbol), data, nil)
}

// SaveOrder adds order save to batch
func (bw *BatchWrite) SaveOrder(order *Order) error {
	data, err := json.Marshal(order)
	if err != nil {
		return err
	}
	return bw.batch.Set(orderKey(order.Owner, order.ID), data, nil)
}

// SaveTrade adds trade save to batch
func (bw *BatchWrite) SaveTrade(trade *Trade) error {
	data, err := json.Marshal(trade)
	if err != nil {
		return err
	}
	return bw.batch.Set(tradeKey(trade.Symbol, trade.Timestamp, trade.ID), data, nil)
}

// Commit writes the batch to Pebble atomically
func (bw *BatchWrite) Commit() error {
	return bw.batch.Commit(pebble.Sync)
}

// Close closes the batch without committing
func (bw *BatchWrite) Close() error {
	return bw.batch.Close()
}
