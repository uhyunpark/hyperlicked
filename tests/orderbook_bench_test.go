package tests

import (
	"fmt"
	"math/rand"
	"testing"

	"github.com/uhyunpark/hyperlicked/pkg/app/core/market"
	"github.com/uhyunpark/hyperlicked/pkg/app/core/orderbook"
)

// BenchmarkOrderbookPlace measures order placement performance
// Target: <10μs per operation (100k orders/sec per symbol)
func BenchmarkOrderbookPlace(b *testing.B) {
	ob := orderbook.NewOrderBook()
	mkt, _ := market.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Pre-fill orderbook with 100 price levels (realistic depth)
	for i := 0; i < 100; i++ {
		bidPrice := int64(1000 - i)
		askPrice := int64(1100 + i)

		bidOrder := &orderbook.Order{
			ID:    fmt.Sprintf("bid-%d", i),
			Side:  orderbook.Buy,
			Type:  "GTC",
			Price: bidPrice,
			Qty:   100,
		}
		askOrder := &orderbook.Order{
			ID:    fmt.Sprintf("ask-%d", i),
			Side:  orderbook.Sell,
			Type:  "GTC",
			Price: askPrice,
			Qty:   100,
		}

		ob.Place(bidOrder, mkt)
		ob.Place(askOrder, mkt)
	}

	b.ResetTimer()

	// Benchmark order placement (alternating buy/sell)
	for i := 0; i < b.N; i++ {
		side := orderbook.Buy
		if i%2 == 0 {
			side = orderbook.Sell
		}

		order := &orderbook.Order{
			ID:    fmt.Sprintf("bench-%d", i),
			Side:  side,
			Type:  "IOC",
			Price: 1050, // Mid-price - will cross and fill
			Qty:   10,
		}

		ob.Place(order, mkt)
	}
}

// BenchmarkOrderbookCancel measures order cancellation performance
// Target: <5μs per operation (O(1) lookup + O(M) queue removal)
func BenchmarkOrderbookCancel(b *testing.B) {
	ob := orderbook.NewOrderBook()
	mkt, _ := market.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Pre-fill orderbook with 1000 resting orders
	orderIDs := make([]string, 1000)
	for i := 0; i < 1000; i++ {
		price := int64(1000 + i)
		orderID := fmt.Sprintf("order-%d", i)
		orderIDs[i] = orderID

		order := &orderbook.Order{
			ID:    orderID,
			Side:  orderbook.Buy,
			Type:  "GTC",
			Price: price,
			Qty:   100,
		}

		ob.Place(order, mkt)
	}

	b.ResetTimer()

	// Benchmark cancellation (cancel random orders)
	for i := 0; i < b.N; i++ {
		idx := i % len(orderIDs)
		ob.Cancel(orderIDs[idx])

		// Re-add order for next iteration (keep orderbook stable)
		if i%100 == 99 {
			for j := 0; j < 100; j++ {
				price := int64(1000 + (i-99+j)%1000)
				orderID := fmt.Sprintf("order-%d", (i-99+j)%1000)

				order := &orderbook.Order{
					ID:    orderID,
					Side:  orderbook.Buy,
					Type:  "GTC",
					Price: price,
					Qty:   100,
				}

				ob.Place(order, mkt)
			}
		}
	}
}

// BenchmarkOrderbookBestPrice measures best bid/ask lookup performance
// Target: <100ns per operation (O(1) heap peek)
func BenchmarkOrderbookBestPrice(b *testing.B) {
	ob := orderbook.NewOrderBook()
	mkt, _ := market.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Pre-fill orderbook with 1000 price levels (stress test)
	for i := 0; i < 1000; i++ {
		bidPrice := int64(10000 - i)
		askPrice := int64(11000 + i)

		bidOrder := &orderbook.Order{
			ID:    fmt.Sprintf("bid-%d", i),
			Side:  orderbook.Buy,
			Type:  "GTC",
			Price: bidPrice,
			Qty:   100,
		}
		askOrder := &orderbook.Order{
			ID:    fmt.Sprintf("ask-%d", i),
			Side:  orderbook.Sell,
			Type:  "GTC",
			Price: askPrice,
			Qty:   100,
		}

		ob.Place(bidOrder, mkt)
		ob.Place(askOrder, mkt)
	}

	b.ResetTimer()

	// Benchmark best bid/ask lookup
	for i := 0; i < b.N; i++ {
		_ = ob.GetBestBid()
		_ = ob.GetBestAsk()
	}
}

// BenchmarkOrderbookGetLevels measures price level aggregation performance
// Used for state hashing and API responses
func BenchmarkOrderbookGetLevels(b *testing.B) {
	ob := orderbook.NewOrderBook()
	mkt, _ := market.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Pre-fill orderbook with 500 price levels
	for i := 0; i < 500; i++ {
		bidPrice := int64(10000 - i)
		askPrice := int64(11000 + i)

		// Multiple orders at same price level (test aggregation)
		for j := 0; j < 5; j++ {
			bidOrder := &orderbook.Order{
				ID:    fmt.Sprintf("bid-%d-%d", i, j),
				Side:  orderbook.Buy,
				Type:  "GTC",
				Price: bidPrice,
				Qty:   100,
			}
			askOrder := &orderbook.Order{
				ID:    fmt.Sprintf("ask-%d-%d", i, j),
				Side:  orderbook.Sell,
				Type:  "GTC",
				Price: askPrice,
				Qty:   100,
			}

			ob.Place(bidOrder, mkt)
			ob.Place(askOrder, mkt)
		}
	}

	b.ResetTimer()

	// Benchmark level aggregation (used for state hashing)
	for i := 0; i < b.N; i++ {
		_ = ob.GetBidLevels()
		_ = ob.GetAskLevels()
	}
}

// BenchmarkOrderbookRealisticWorkload simulates realistic trading patterns
// 70% market orders (immediate fill), 20% limit orders (rest), 10% cancels
func BenchmarkOrderbookRealisticWorkload(b *testing.B) {
	ob := orderbook.NewOrderBook()
	mkt, _ := market.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Pre-fill orderbook with 200 levels
	for i := 0; i < 200; i++ {
		bidPrice := int64(10000 - i*10)
		askPrice := int64(11000 + i*10)

		bidOrder := &orderbook.Order{
			ID:    fmt.Sprintf("init-bid-%d", i),
			Side:  orderbook.Buy,
			Type:  "GTC",
			Price: bidPrice,
			Qty:   1000,
		}
		askOrder := &orderbook.Order{
			ID:    fmt.Sprintf("init-ask-%d", i),
			Side:  orderbook.Sell,
			Type:  "GTC",
			Price: askPrice,
			Qty:   1000,
		}

		ob.Place(bidOrder, mkt)
		ob.Place(askOrder, mkt)
	}

	rng := rand.New(rand.NewSource(12345))
	restingOrders := make([]string, 0, 1000)

	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		r := rng.Float64()

		if r < 0.7 {
			// 70% market orders (IOC, crosses spread)
			side := orderbook.Buy
			price := int64(11000) // Above best ask - will fill
			if rng.Float64() < 0.5 {
				side = orderbook.Sell
				price = 10000 // Below best bid - will fill
			}

			order := &orderbook.Order{
				ID:    fmt.Sprintf("market-%d", i),
				Side:  side,
				Type:  "IOC",
				Price: price,
				Qty:   int64(10 + rng.Intn(90)),
			}

			ob.Place(order, mkt)

		} else if r < 0.9 {
			// 20% limit orders (GTC, rest on book)
			side := orderbook.Buy
			price := int64(9900 - rng.Intn(100)) // Below best bid
			if rng.Float64() < 0.5 {
				side = orderbook.Sell
				price = 11100 + int64(rng.Intn(100)) // Above best ask
			}

			orderID := fmt.Sprintf("limit-%d", i)
			order := &orderbook.Order{
				ID:    orderID,
				Side:  side,
				Type:  "GTC",
				Price: price,
				Qty:   int64(10 + rng.Intn(90)),
			}

			ob.Place(order, mkt)
			restingOrders = append(restingOrders, orderID)

		} else {
			// 10% cancellations
			if len(restingOrders) > 0 {
				idx := rng.Intn(len(restingOrders))
				ob.Cancel(restingOrders[idx])

				// Remove from tracking (swap with last)
				restingOrders[idx] = restingOrders[len(restingOrders)-1]
				restingOrders = restingOrders[:len(restingOrders)-1]
			}
		}
	}
}

// BenchmarkOrderbookHeavyDepth measures performance with deep orderbooks
// Simulates Hyperliquid-style depth (1000+ price levels)
func BenchmarkOrderbookHeavyDepth(b *testing.B) {
	ob := orderbook.NewOrderBook()
	mkt, _ := market.NewMarketWithDefaults("HYPL-USDC", "HYPL", "USDC")

	// Pre-fill with 2000 price levels (extreme depth)
	for i := 0; i < 2000; i++ {
		bidPrice := int64(100000 - i)
		askPrice := int64(110000 + i)

		bidOrder := &orderbook.Order{
			ID:    fmt.Sprintf("bid-%d", i),
			Side:  orderbook.Buy,
			Type:  "GTC",
			Price: bidPrice,
			Qty:   100,
		}
		askOrder := &orderbook.Order{
			ID:    fmt.Sprintf("ask-%d", i),
			Side:  orderbook.Sell,
			Type:  "GTC",
			Price: askPrice,
			Qty:   100,
		}

		ob.Place(bidOrder, mkt)
		ob.Place(askOrder, mkt)
	}

	rng := rand.New(rand.NewSource(54321))

	b.ResetTimer()

	// Benchmark market orders against deep book
	for i := 0; i < b.N; i++ {
		side := orderbook.Buy
		price := int64(110000) // Cross spread
		if i%2 == 0 {
			side = orderbook.Sell
			price = 100000
		}

		order := &orderbook.Order{
			ID:    fmt.Sprintf("deep-%d", i),
			Side:  side,
			Type:  "IOC",
			Price: price,
			Qty:   int64(50 + rng.Intn(450)),
		}

		ob.Place(order, mkt)
	}
}
