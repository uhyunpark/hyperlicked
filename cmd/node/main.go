package main

import (
	"context"
	"log"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/uhyunpark/hyperlicked/params"
	"github.com/uhyunpark/hyperlicked/pkg/abci"
	"github.com/uhyunpark/hyperlicked/pkg/api"
	"github.com/uhyunpark/hyperlicked/pkg/app/perp"
	"github.com/uhyunpark/hyperlicked/pkg/consensus"
	"github.com/uhyunpark/hyperlicked/pkg/crypto"
	"github.com/uhyunpark/hyperlicked/pkg/p2p"
	"github.com/uhyunpark/hyperlicked/pkg/storage"
	"github.com/uhyunpark/hyperlicked/pkg/util"
)

func main() {
	// Load config from .env file and environment variables
	cfg := params.LoadFromEnv("") // "" means load from .env in current directory

	// Setup logging (write to both console and file)
	logFile := os.Getenv("LOG_FILE")
	if logFile == "" {
		logFile = "data/node.log"
	}

	logger, err := util.NewLoggerWithFile(logFile)
	if err != nil {
		log.Fatalf("logger: %v", err)
	}
	defer logger.Sync()
	sugar := logger.Sugar()
	sugar.Infow("logger_initialized", "log_file", logFile)

	// ---- App: Perp DEX (production) ----
	app := perp.NewApp()
	// Market initialized in NewApp(): BTC-USDT only
	//
	// NOTE: Sample transactions removed - all orders must be signed (EIP-712).
	// Use frontend wallet or TxFeeder (ENABLE_TXGEN=true) to generate orders.

	bridge := &abci.Bridge{App: app}

	// ---- Consensus ----
	selfID := consensus.NodeID(cfg.Consensus.Validators[0])

	// Build validator set from config
	var ids []consensus.NodeID
	for _, s := range cfg.Consensus.Validators {
		ids = append(ids, consensus.NodeID(s))
	}

	// For single-node development: only use this validator
	// For multi-node: use all validators
	// TODO: Proper peer discovery & dynamic validator set
	singleNodeMode := cfg.Node.SingleNode
	if singleNodeMode {
		ids = []consensus.NodeID{selfID}
	}

	// Quorum: N validators, need 2f+1 = 2*t+1 where N=3t+1
	// For N=1: t=0, need 1 vote (single-node dev mode)
	// For N=4: t=1, need 3 votes
	// For N=7: t=2, need 5 votes
	n := len(ids)
	t := (n - 1) / 3

	state := &consensus.State{
		Q:       consensus.Quorum{N: n, T: t},
		SelfID:  selfID,
		Blocks:  make(map[consensus.Hash]consensus.Block),
		Genesis: consensus.GenesisBlock(),
	}
	safety := consensus.NewSafety(state)
	pm := consensus.NewPacemaker(
		consensus.PacemakerTimers{Ppc: cfg.Consensus.Ppc, Delta: cfg.Consensus.Delta},
		util.RealClock{},
		state,
	)

	// Network: always use libp2p (works for any number of validators)
	elec := consensus.RoundRobinElector{IDs: ids}
	var signer interface{} = crypto.DummySigner{}

	lpn, err := p2p.NewLibp2pNet(context.Background(), p2p.Libp2pConfig{
		ListenAddr: os.Getenv("LISTEN"),
		Bootstrap:  []string{},
		SelfID:     state.SelfID,
		Quorum:     state.Q,
		Logger:     sugar,
	})
	if err != nil {
		sugar.Fatalw("libp2p_init_failed", "err", err)
	}
	net := lpn

	engine := consensus.NewEngine(state, safety, pm, bridge, net, elec, signer)
	engine.Logger = sugar
	engine.Store = storage.NewInMemoryBlockStore()
	engine.MinBlockTime = cfg.Node.MinBlockTime // Apply block time throttle from config

	// Control logging verbosity via env var (default: quiet)
	if os.Getenv("VERBOSE") == "true" {
		engine.VerboseLogging = true
		sugar.Info("verbose logging enabled")
	}

	sugar.Infow("block_time_config", "min_block_time_ms", cfg.Node.MinBlockTime.Milliseconds())

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	// ---- Transaction Feeder (optional) ----
	// Enable with: ENABLE_TXGEN=true TXGEN_MODE=default|high|hyperliquid
	if os.Getenv("ENABLE_TXGEN") == "true" {
		var txCfg perp.TxFeederConfig
		mode := os.Getenv("TXGEN_MODE")
		switch mode {
		case "high":
			txCfg = perp.HighLoadConfig()
			sugar.Infow("txgen_enabled", "mode", "high_load", "target_tps", txCfg.TxPerSecond)
		case "hyperliquid":
			txCfg = perp.HyperliquidConfig()
			sugar.Infow("txgen_enabled", "mode", "hyperliquid", "target_tps", txCfg.TxPerSecond)
		default:
			txCfg = perp.DefaultFeederConfig()
			sugar.Infow("txgen_enabled", "mode", "default", "target_tps", txCfg.TxPerSecond)
		}

		cancelFeeder := perp.StartTxFeeder(ctx, app, txCfg)
		defer cancelFeeder()
	} else {
		sugar.Info("txgen_disabled - using sample transactions only")
	}

	// Logging control: log every N blocks to reduce noise
	logInterval := consensus.Height(100)
	lastLoggedHeight := consensus.Height(0)

	sugar.Infow("node_starting",
		"config_validators", len(cfg.Consensus.Validators),
		"active_validators", len(ids),
		"single_node_mode", singleNodeMode,
		"quorum_need", 2*t+1)

	// ---- API Server ----
	// Start HTTP/WebSocket server for frontend
	apiServer := api.NewServer(app)
	apiAddr := os.Getenv("API_ADDR")
	if apiAddr == "" {
		apiAddr = ":8080"
	}

	go func() {
		sugar.Infow("api_server_starting", "addr", apiAddr)
		if err := apiServer.Start(apiAddr); err != nil {
			sugar.Fatalw("api_server_failed", "err", err)
		}
	}()

	// Hook API server to consensus and app: broadcast updates on every block commit
	engine.OnBlockCommit = func(height consensus.Height) {
		apiServer.BroadcastOrderbook("BTC-USDT", int64(height))
	}

	// Hook app to API server: broadcast trades when they execute
	app.OnTrade = func(symbol string, price, size int64, side string, timestamp int64) {
		apiServer.BroadcastTrade(symbol, price, size, side, timestamp)
	}

	// Start consensus engine (HotStuff Run loop)
	// Leader actively proposes, followers reactively respond
	go func() {
		if err := engine.Run(ctx); err != nil && ctx.Err() == nil {
			sugar.Fatalw("engine_failed", "err", err)
		}
	}()

	// Progress logging loop
	ticker := time.NewTicker(1 * time.Second)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			// Log progress every logInterval blocks
			if state.Height-lastLoggedHeight >= logInterval || state.Height <= 5 {
				sugar.Infow("consensus_progress",
					"height", state.Height,
					"view", state.View,
					"blocks_since_last_log", state.Height-lastLoggedHeight)
				lastLoggedHeight = state.Height
			}
		}
	}
}
