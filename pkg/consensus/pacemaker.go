package consensus

import (
	"context"
	"time"

	"github.com/uhyunpark/hyperlicked/pkg/util"
)

type PacemakerTimers struct {
	Ppc   time.Duration
	Delta time.Duration
}

type Pacemaker struct {
	Timers PacemakerTimers
	Clock  util.Clock
	State  *State

	// Channels for reactive view advancement (follower mode)
	viewAdvanceCh chan View
}

func NewPacemaker(timers PacemakerTimers, clock util.Clock, state *State) *Pacemaker {
	return &Pacemaker{
		Timers:        timers,
		Clock:         clock,
		State:         state,
		viewAdvanceCh: make(chan View, 10), // Buffered for multiple prepare messages
	}
}

// WaitForViewAdvance: Follower waits for prepare message to advance view
// Returns when prepare for this view is received, or timeout
func (p *Pacemaker) WaitForViewAdvance(ctx context.Context, targetView View) error {
	timeout := p.Timers.Ppc + p.Timers.Delta
	deadline := p.Clock.After(timeout)

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline:
			// Timeout: view-change (not implemented yet, for now just advance)
			p.State.View = targetView
			return nil
		case v := <-p.viewAdvanceCh:
			if v >= targetView {
				p.State.View = v
				return nil
			}
		}
	}
}

// SignalViewAdvance: Called by onPrepare to signal view advancement
func (p *Pacemaker) SignalViewAdvance(v View) {
	select {
	case p.viewAdvanceCh <- v:
	default:
		// Channel full, drop (follower will timeout)
	}
}

type Handlers struct {
	OnPropose func(ctx context.Context, p Propose)
	OnPrepare func(ctx context.Context, cert Certificate, blk Block)
}

type Network interface {
	// outbound
	BroadcastPropose(ctx context.Context, p Propose) error
	BroadcastPrepare(ctx context.Context, cert Certificate) error
	SendVote(ctx context.Context, to NodeID, v Vote) error

	// leader-side collections
	CollectVotes(ctx context.Context, view View, h Hash, need int) ([]Vote, error)

	// inbound handler registration
	SetHandlers(h Handlers)
}

type AppHook interface {
	PreparePayload(parent Block, next Height) []byte
	OnCommit(committed Block) Hash // Returns AppHash after executing block
}
