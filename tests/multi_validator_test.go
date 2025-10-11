// file: tests/multi_validator_test.go
package tests

import (
	"context"
	"testing"
	"time"

	"github.com/uhyunpark/hyperlicked/pkg/abci"
	"github.com/uhyunpark/hyperlicked/pkg/consensus"
	"github.com/uhyunpark/hyperlicked/pkg/crypto"
	"github.com/uhyunpark/hyperlicked/pkg/p2p"
	"github.com/uhyunpark/hyperlicked/pkg/storage"
	"github.com/uhyunpark/hyperlicked/pkg/util"
)

// TestFourValidators: In-memory simulation of 4 validators running HotStuff consensus
// This is the minimum viable BFT setup (N=4, f=1, need 2f+1=3 votes)
func TestFourValidators(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	// Validator IDs
	ids := []consensus.NodeID{"val1", "val2", "val3", "val4"}

	// Create 4 validators, each with their own state and app
	engines := make([]*consensus.Engine, 4)
	networks := make([]*p2p.Libp2pNet, 4)

	for i, id := range ids {
		// Each validator has independent app state
		app := abci.NewMockApp()
		if i == 0 {
			// Only val1 (leader) has tx in mempool
			app.PushTx([]byte("tx1"))
		}
		bridge := &abci.Bridge{App: app}

		state := &consensus.State{
			Q:       consensus.Quorum{N: 4, T: 1}, // N=4, need 2*1+1=3 votes
			SelfID:  id,
			Blocks:  make(map[consensus.Hash]consensus.Block),
			Genesis: consensus.GenesisBlock(),
		}
		safety := consensus.NewSafety(state)
		pm := consensus.NewPacemaker(
			consensus.PacemakerTimers{Ppc: 50 * time.Millisecond, Delta: 50 * time.Millisecond},
			util.RealClock{},
			state,
		)

		// Create libp2p network for each validator
		net, err := p2p.NewLibp2pNet(ctx, p2p.Libp2pConfig{
			ListenAddr: "", // Random port
			Bootstrap:  []string{},
			SelfID:     id,
			Quorum:     state.Q,
			Logger:     nil,
		})
		if err != nil {
			t.Fatalf("val%d: libp2p init failed: %v", i+1, err)
		}
		networks[i] = net

		// Leader is val1 (round-robin with single leader)
		elec := consensus.RoundRobinElector{IDs: []consensus.NodeID{"val1"}}
		signer := crypto.DummySigner{}

		engine := consensus.NewEngine(state, safety, pm, bridge, net, elec, signer)
		engine.Store = storage.NewInMemoryBlockStore()
		engines[i] = engine
	}

	// Connect validators to each other (peer discovery)
	// In real libp2p, this happens via bootstrap/DHT
	// For in-memory test, we manually connect peers
	for i := 0; i < 4; i++ {
		for j := i + 1; j < 4; j++ {
			// Add peer addresses
			networks[i].Host().Peerstore().AddAddrs(networks[j].Host().ID(), networks[j].Host().Addrs(), time.Hour)
			networks[j].Host().Peerstore().AddAddrs(networks[i].Host().ID(), networks[i].Host().Addrs(), time.Hour)

			// Connect i <-> j
			if err := networks[i].Host().Connect(ctx, networks[j].Host().Peerstore().PeerInfo(networks[j].Host().ID())); err != nil {
				t.Logf("warn: connecting val%d <-> val%d: %v", i+1, j+1, err)
			}
		}
	}

	// Give peers time to connect
	time.Sleep(200 * time.Millisecond)

	// Run all validators concurrently with new Run() method
	// Leader actively proposes, followers reactively respond
	for i := 0; i < 4; i++ {
		i := i
		go func() {
			if err := engines[i].Run(ctx); err != nil && ctx.Err() == nil {
				t.Logf("val%d: engine error: %v", i+1, err)
			}
		}()
	}

	// Wait for consensus to reach height 1 (with timeout)
	deadline := time.After(5 * time.Second)
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-deadline:
			t.Fatal("timeout waiting for consensus")
		case <-ticker.C:
			// Check if all validators reached height 1
			allReady := true
			for _, e := range engines {
				if e.State.Height < 1 {
					allReady = false
					break
				}
			}
			if allReady {
				cancel() // Stop all validators
				time.Sleep(100 * time.Millisecond) // Let them finish
				goto done
			}
		}
	}
done:

	// Verify all validators reached height >= 1 (committed at least first block)
	for i, engine := range engines {
		if engine.State.Height < 1 {
			t.Errorf("val%d: expected height>=1, got %d", i+1, engine.State.Height)
		}
	}

	// Verify all validators have same committed block hash
	var commitHash consensus.Hash
	for i := 0; i < 4; i++ {
		h, ok := engines[i].Store.GetCommitted()
		if !ok {
			t.Errorf("val%d: no committed block", i+1)
			continue
		}
		if i == 0 {
			commitHash = h
		} else if h != commitHash {
			t.Errorf("val%d: committed hash mismatch: got %x, want %x", i+1, h[:8], commitHash[:8])
		}
	}

	t.Logf("✅ All 4 validators reached consensus on block %x", commitHash[:8])
}
