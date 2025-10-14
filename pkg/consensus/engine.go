package consensus

import (
	"context"
	"fmt"

	"github.com/uhyunpark/hyperlicked/pkg/crypto"

	"go.uber.org/zap"
)

type Engine struct {
	State   *State
	Safety  *Safety
	PM      *Pacemaker
	App     AppHook
	Net     Network
	Elector LeaderElector
	Signer  interface{} // can be *crypto.BLSSigner (proto) or dummy
	ID      NodeID

	EnableBLS bool                         // when true, use crypto.BLS to sign/aggregate
	PubKeys   map[NodeID]*crypto.BLSPubKey // validator pubkeys (same message aggregation)

	Logger         *zap.SugaredLogger
	VerboseLogging bool // if false, only log commits and errors

	// Optional: pluggable storage/WAL (proto: in-memory)
	Store BlockStore
	WAL   WAL
}

func NewEngine(state *State, safety *Safety, pm *Pacemaker, app AppHook, net Network, elec LeaderElector, signer interface{}) *Engine {
	e := &Engine{
		State: state, Safety: safety, PM: pm,
		App: app, Net: net, Elector: elec, Signer: signer,
		ID: state.SelfID,
	}
	net.SetHandlers(Handlers{
		OnPropose: e.onPropose,
		OnPrepare: e.onPrepare,
	})
	return e
}

// Run: Main consensus loop (HotStuff standard)
// Leader: actively proposes when it's their view
// Follower: reactively responds to propose/prepare messages
func (e *Engine) Run(ctx context.Context) error {
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		v := e.State.View + 1
		leader := e.Elector.LeaderOf(v)

		if e.Logger != nil && e.VerboseLogging {
			e.Logger.Infow("enter_view", "view", v, "leader", leader, "is_leader", leader == e.ID)
		}

		if leader == e.ID {
			// I am leader: actively propose
			if err := e.leaderRound(ctx, v); err != nil {
				return err
			}
			// Leader advances view after getting QC
			e.State.View = v
		} else {
			// I am follower: wait for propose/prepare (reactive)
			// View will advance in onPrepare when we receive prepare message
			if err := e.PM.WaitForViewAdvance(ctx, v); err != nil {
				return err
			}
		}
	}
}

// RunN: For testing - run N views (leader-only, for backward compatibility)
func (e *Engine) RunN(ctx context.Context, rounds int) error {
	for i := 0; i < rounds; i++ {
		v := e.State.View + 1
		leader := e.Elector.LeaderOf(v)

		if leader != e.ID {
			// Non-leader: wait for view to advance via onPrepare
			if err := e.PM.WaitForViewAdvance(ctx, v); err != nil {
				return err
			}
			continue
		}

		// Leader: propose
		if err := e.leaderRound(ctx, v); err != nil {
			return err
		}
		e.State.View = v
	}
	return nil
}

// CRITICAL: Followers must EXECUTE block before voting to compute AppHash
// This ensures the vote commits to both transactions (H) and resulting state (AppHash)
func (e *Engine) onPropose(ctx context.Context, p Propose) {
	if e.Store != nil {
		e.Store.SaveBlock(p.Block)
	}
	if !e.Safety.CanVote(p) {
		if e.Logger != nil && e.VerboseLogging {
			e.Logger.Debugw("vote_skip_cannot", "view", p.Block.View)
		}
		return
	}

	// Execute block to compute AppHash BEFORE voting
	// This is the key change: validators must agree on state before voting
	appHash := e.App.OnCommit(p.Block)

	v := Vote{
		View:     p.Block.View,
		H:        HashOfBlock(p.Block),
		AppHash:  appHash, // ← NEW: Include state commitment in vote
		SigShare: nil,
		From:     e.ID,
	}

	if e.EnableBLS {
		if s, ok := e.Signer.(*crypto.BLSSigner); ok {
			v.SigShare = s.Sign(v.H[:])
		}
	} else {
		v.SigShare = []byte("s")
	}
	to := e.Elector.LeaderOf(p.Block.View)
	_ = e.Net.SendVote(ctx, to, v)
	if e.Logger != nil && e.VerboseLogging {
		e.Logger.Debugw("vote_sent", "view", p.Block.View, "to", to, "apphash", fmt.Sprintf("0x%x", appHash[:8]))
	}
}

// follower/leader 공통: Prepare 수신 → HighestQC 갱신 + (더블‑체인 충족 시) 커밋
func (e *Engine) onPrepare(ctx context.Context, cert Certificate, blk Block) {
	if e.Store != nil {
		e.Store.SaveCert(cert)
		if blk.Height > 0 || blk.Proposer != "" {
			e.Store.SaveBlock(blk)
		}
	}
	e.Safety.OnPrepare(cert, blk)

	// Signal view advancement to waiting followers (reactive mode)
	e.PM.SignalViewAdvance(cert.View)

	// 더블‑체인 커밋: C_{v-1}, C_v 있고, B_v.Parent == C_{v-1}.H 이면 B_{v-1} 커밋
	if cert.View == 0 {
		return
	}
	if e.Store == nil {
		return
	} // 저장소 없으면 스킵(프로토)
	prevCert, ok := e.Store.GetCert(cert.View - 1)
	if !ok {
		return
	}

	childBlk := blk
	if childBlk.Height == 0 && childBlk.Proposer == "" {
		if b, ok2 := e.Store.GetBlock(cert.H); ok2 {
			childBlk = b
		}
	}
	if childBlk.Proposer == "" {
		return
	} // 현재 블록 모르면 커밋 판단 보류

	if childBlk.Parent != prevCert.H {
		return
	}

	prevBlk, ok := e.Store.GetBlock(prevCert.H)
	if !ok {
		return
	}

	// Use AppHash from previous certificate (already agreed upon by 2f+1 validators)
	// Block was executed during voting, AppHash is now part of the certificate
	appHash := prevCert.AppHash

	// Update block with AppHash if not already set
	if prevBlk.AppHash == (Hash{}) {
		prevBlk.AppHash = appHash
	}

	e.Safety.UpdateLock(prevCert, prevBlk)
	e.State.Height++

	if e.Store != nil {
		e.Store.SaveBlock(prevBlk) // Re-save block with AppHash
		e.Store.SetCommitted(HashOfBlock(prevBlk))
	}
	if e.WAL != nil {
		e.WAL.Append(fmt.Sprintf("commit height=%d view=%d apphash=0x%x", e.State.Height, prevBlk.View, appHash[:]))
	}

	if e.Logger != nil {
		e.Logger.Infow("commit", "height", e.State.Height, "committed_view", prevBlk.View, "apphash", fmt.Sprintf("0x%x", appHash[:]))
	}
}

func (e *Engine) leaderRound(ctx context.Context, v View) error {
	ldr := &Leader{ID: e.ID, Net: e.Net, Safety: e.Safety, App: e.App}
	block, prop, err := ldr.Propose(ctx, v, e.State.Height)
	if err != nil {
		return fmt.Errorf("propose: %w", err)
	}
	if e.Logger != nil && e.VerboseLogging {
		e.Logger.Infow("propose_broadcasted", "height", block.Height, "view", v, "parent", prop.HighCert.H.String())
	}
	if e.Store != nil {
		e.Store.SaveBlock(block)
	}
	if e.WAL != nil {
		e.WAL.Append(fmt.Sprintf("propose v=%d h=%d", v, block.Height))
	}

	// Leader receives its own propose and votes (handled by onPropose via broadcast)
	// No execution here - leader executes in onPropose like all other validators

	need := 2*e.State.Q.T + 1
	votes, err := e.Net.CollectVotes(ctx, v, HashOfBlock(block), need)
	if err != nil {
		return fmt.Errorf("collect votes: %w", err)
	}

	// CRITICAL: Verify all votes have the SAME AppHash
	// This detects Byzantine validators with divergent state
	if len(votes) == 0 {
		return fmt.Errorf("no votes collected")
	}

	agreedAppHash := votes[0].AppHash
	for i, vote := range votes {
		if vote.AppHash != agreedAppHash {
			return fmt.Errorf("AppHash mismatch: vote[0] from %s has 0x%x, but vote[%d] from %s has 0x%x - Byzantine fault detected",
				votes[0].From, agreedAppHash[:8], i, vote.From, vote.AppHash[:8])
		}
	}

	if e.Logger != nil && e.VerboseLogging {
		e.Logger.Debugw("apphash_verified", "view", v, "apphash", fmt.Sprintf("0x%x", agreedAppHash[:8]), "votes", len(votes))
	}

	var sigAgg []byte

	if e.EnableBLS {
		// aggregate shares (same message = block hash)
		var shares [][]byte
		for _, vt := range votes {
			if len(vt.SigShare) > 0 {
				shares = append(shares, vt.SigShare)
			}
		}
		sigAgg = crypto.Aggregate(shares)
	} else {
		sigAgg = []byte("agg")
	}

	// Certificate now commits to BOTH consensus hash AND application state
	cert := Certificate{
		View:    v,
		H:       HashOfBlock(block),
		AppHash: agreedAppHash, // ← NEW: Include agreed state in certificate
		Sig:     sigAgg,
	}
	if e.Store != nil {
		e.Store.SaveCert(cert)
	}
	if err := e.Net.BroadcastPrepare(ctx, cert); err != nil {
		return fmt.Errorf("broadcast prepare: %w", err)
	}
	e.Safety.OnPrepare(cert, block) // 로컬도 관찰 처리

	return nil
}
