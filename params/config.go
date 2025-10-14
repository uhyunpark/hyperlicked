package params

import (
	"os"
	"strconv"
	"time"

	"github.com/joho/godotenv"
)

type Consensus struct {
	Validators []string
	Ppc        time.Duration // leader status wait (Case-2)
	Delta      time.Duration // network upper bound
}

type Node struct {
	SingleNode bool
	// MinBlockTime throttles block production to prevent excessive empty blocks
	// in single-node devnet with fast-path enabled.
	//
	// Recommended values:
	//   - Devnet (single node):  200ms (5 blocks/sec, prevents log spam)
	//   - Testnet (multi-node):  100ms (10 blocks/sec, closer to production)
	//   - Production (WAN):      0ms (no artificial throttle; network latency provides natural pacing)
	//
	// Note: In production multi-validator networks, vote collection and gossip
	// naturally pace block production, making artificial throttling unnecessary.
	MinBlockTime time.Duration
}

type Config struct {
	Consensus Consensus
	Node      Node
}

func Default() Config {
	return Config{
		Consensus: Consensus{
			Validators: []string{"val1", "val2", "val3", "val4"},
			Ppc:        150 * time.Millisecond,
			Delta:      50 * time.Millisecond,
		},
		Node: Node{
			SingleNode:   true,
			MinBlockTime: 200 * time.Millisecond, // Devnet default: prevent log spam
		},
	}
}

// LoadFromEnv loads configuration from .env file (if exists) and environment variables
// Priority: ENV > .env file > defaults
func LoadFromEnv(envPath string) Config {
	cfg := Default()

	// Try to load .env file (optional - won't fail if not exists)
	if envPath != "" {
		_ = godotenv.Load(envPath)
	} else {
		_ = godotenv.Load() // loads .env from current directory
	}

	// Override with environment variables
	if ppc := os.Getenv("CONSENSUS_PPC_MS"); ppc != "" {
		if ms, err := strconv.Atoi(ppc); err == nil {
			cfg.Consensus.Ppc = time.Duration(ms) * time.Millisecond
		}
	}

	if delta := os.Getenv("CONSENSUS_DELTA_MS"); delta != "" {
		if ms, err := strconv.Atoi(delta); err == nil {
			cfg.Consensus.Delta = time.Duration(ms) * time.Millisecond
		}
	}

	if minBlock := os.Getenv("NODE_MIN_BLOCK_TIME_MS"); minBlock != "" {
		if ms, err := strconv.Atoi(minBlock); err == nil {
			cfg.Node.MinBlockTime = time.Duration(ms) * time.Millisecond
		}
	}
	if singleNode := os.Getenv("SINGLE_NODE"); singleNode != "" {
		cfg.Node.SingleNode = singleNode == "true"
	}

	// Validators from comma-separated list
	if vals := os.Getenv("CONSENSUS_VALIDATORS"); vals != "" {
		// Example: "val1,val2,val3,val4"
		// You can use strings.Split(vals, ",") if needed
		// cfg.Consensus.Validators = strings.Split(vals, ",")
	}

	return cfg
}

// getEnv returns environment variable value or default
func getEnv(key, defaultValue string) string {
	if value := os.Getenv(key); value != "" {
		return value
	}
	return defaultValue
}
