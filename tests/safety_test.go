// file: tests/safety_test.go
package tests

import (
	"testing"
	"time"

	"github.com/uhyunpark/hyperlicked/pkg/consensus"
)

func TestSafetyCanVote(t *testing.T) {
	st := &consensus.State{
		Q:       consensus.Quorum{N: 4, T: 1},
		SelfID:  consensus.NodeID("val1"),
		Blocks:  make(map[consensus.Hash]consensus.Block),
		Genesis: consensus.GenesisBlock(),
	}
	sf := consensus.NewSafety(st)

	blk := consensus.Block{Height: 1, View: 10, Time: time.Now()}
	h := consensus.HashOfBlock(blk)
	c10 := consensus.Certificate{View: 10, H: h}
	sf.UpdateLock(c10, blk)

	if sf.CanVote(consensus.Propose{HighCert: consensus.Certificate{View: 9}}) {
		t.Fatalf("expected CanVote=false for highcert=9 vs locked=10")
	}
	if !sf.CanVote(consensus.Propose{HighCert: consensus.Certificate{View: 10}}) {
		t.Fatalf("expected CanVote=true for highcert=10")
	}
	if !sf.CanVote(consensus.Propose{HighCert: consensus.Certificate{View: 11}}) {
		t.Fatalf("expected CanVote=true for highcert=11")
	}
}
