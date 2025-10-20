package perp

import (
	"context"
	"log"
	"time"
)

// TxFeederConfig controls transaction generation rate
type TxFeederConfig struct {
	TxPerSecond int           // Target transactions per second
	BatchSize   int           // Number of txs to generate per batch
	Interval    time.Duration // How often to generate batches
	NumAccounts int           // Number of simulated traders
	Symbols     []string      // Markets to trade
}

// DefaultFeederConfig returns reasonable defaults for testing
func DefaultFeederConfig() TxFeederConfig {
	return TxFeederConfig{
		TxPerSecond: 100,         // 100 tx/sec (modest load)
		BatchSize:   10,          // 10 txs per batch
		Interval:    100 * time.Millisecond, // Every 100ms
		NumAccounts: 50,          // 50 simulated traders
		Symbols:     []string{"BTC-USDT"},
	}
}

// HighLoadConfig returns config for stress testing
func HighLoadConfig() TxFeederConfig {
	return TxFeederConfig{
		TxPerSecond: 1000,        // 1000 tx/sec (high load)
		BatchSize:   100,         // 100 txs per batch
		Interval:    100 * time.Millisecond,
		NumAccounts: 200,         // 200 simulated traders
		Symbols:     []string{"BTC-USDT"},
	}
}

// HyperliquidConfig mimics Hyperliquid's load (500-3000 orders/block, ~100ms blocks)
func HyperliquidConfig() TxFeederConfig {
	return TxFeederConfig{
		TxPerSecond: 15000,       // 15k tx/sec (1500 per 100ms block)
		BatchSize:   150,         // 150 txs per batch
		Interval:    10 * time.Millisecond, // Every 10ms
		NumAccounts: 500,         // 500 simulated traders
		Symbols:     []string{"BTC-USDT"},
	}
}

// StartTxFeeder starts a background goroutine that continuously feeds transactions to the app
// Returns a cancel function to stop the feeder
func StartTxFeeder(ctx context.Context, app *App, cfg TxFeederConfig) context.CancelFunc {
	if len(cfg.Symbols) == 0 {
		cfg.Symbols = []string{"PERP-USD"}
	}

	gen := NewTxGenerator(cfg.NumAccounts, cfg.Symbols)

	// Create cancellable context
	feedCtx, cancel := context.WithCancel(ctx)

	go func() {
		ticker := time.NewTicker(cfg.Interval)
		defer ticker.Stop()

		startTime := time.Now()
		totalTxs := 0

		log.Printf("[txfeeder] Started - target %d tx/sec, batch %d every %v",
			cfg.TxPerSecond, cfg.BatchSize, cfg.Interval)

		for {
			select {
			case <-feedCtx.Done():
				elapsed := time.Since(startTime)
				log.Printf("[txfeeder] Stopped - generated %d txs in %v (%.1f tx/sec)",
					totalTxs, elapsed.Round(time.Second), float64(totalTxs)/elapsed.Seconds())
				return

			case <-ticker.C:
				// Generate batch of transactions
				batch := gen.GenerateBatch(cfg.BatchSize)

				// Push to app mempool
				for _, tx := range batch {
					app.PushTx(tx)
				}

				totalTxs += len(batch)

				// Log stats every 10 seconds
				elapsed := time.Since(startTime)
				if int(elapsed.Seconds())%10 == 0 && elapsed.Seconds() > 1 {
					stats := gen.GetStats(elapsed)
					log.Printf("[txfeeder] Stats - total: %d, rate: %.1f tx/sec, accounts: %d, symbols: %d",
						totalTxs, float64(totalTxs)/elapsed.Seconds(),
						cfg.NumAccounts, len(cfg.Symbols))
					_ = stats
				}
			}
		}
	}()

	return cancel
}
