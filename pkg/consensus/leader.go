package consensus

import (
	"context"
	"time"
)

type LeaderElector interface{ LeaderOf(v View) NodeID }

type RoundRobinElector struct{ IDs []NodeID }

func (r RoundRobinElector) LeaderOf(v View) NodeID {
	if len(r.IDs) == 0 {
		return NodeID("unknown")
	}
	idx := int(v)
	if idx <= 0 {
		idx = 1
	}
	return r.IDs[(idx-1)%len(r.IDs)]
}

type Leader struct {
	ID     NodeID
	Net    Network
	Safety *Safety
	App    AppHook
}

func (l *Leader) Propose(ctx context.Context, view View, height Height) (Block, Propose, error) {
	high := l.Safety.HighestCert()
	parent, ok := l.Safety.BlockByHash(high.H)
	if !ok {
		parent = l.Safety.state.Genesis
	}
	payload := l.App.PreparePayload(parent, height+1)
	b := Block{
		Height: height + 1, View: view, Parent: high.H,
		Payload: payload, Proposer: l.ID, Time: time.Now(),
	}
	prop := Propose{Block: b, HighCert: high, HighDouble: l.Safety.HighestDouble()}
	return b, prop, l.Net.BroadcastPropose(ctx, prop)
}
