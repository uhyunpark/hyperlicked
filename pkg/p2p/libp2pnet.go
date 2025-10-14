package p2p

import (
	"context"
	"errors"
	"io"
	"sync"
	"time"

	libp2p "github.com/libp2p/go-libp2p"
	pubsub "github.com/libp2p/go-libp2p-pubsub"
	"github.com/libp2p/go-libp2p/core/host"
	"github.com/libp2p/go-libp2p/core/network"
	"github.com/libp2p/go-libp2p/core/peer"
	"github.com/libp2p/go-libp2p/core/protocol"
	ma "github.com/multiformats/go-multiaddr"
	"go.uber.org/zap"

	"github.com/uhyunpark/hyperlicked/pkg/consensus"
)

const (
	topicPropose = "hs2-propose"
	topicPrepare = "hs2-prepare"
	protocolVote = protocol.ID("/hs2/vote/1.0.0")
)

type Libp2pNet struct {
	h    host.Host
	ps   *pubsub.PubSub
	log  *zap.SugaredLogger
	self consensus.NodeID
	q    consensus.Quorum

	tPropose, tPrepare     *pubsub.Topic
	subPropose, subPrepare *pubsub.Subscription

	muVotes sync.Mutex
	votes   map[consensus.View]map[consensus.Hash][]consensus.Vote // Leader collects votes here

	// Channel-based reactive vote collection (eliminates polling)
	// When a vote arrives, we signal voteArrivedCh to wake up CollectVotes immediately
	voteArrivedCh chan struct{}

	muPrep  sync.Mutex
	prepByV map[consensus.View]struct {
		c consensus.Certificate
		b consensus.Block
	}

	muH      sync.RWMutex
	handlers consensus.Handlers
}

type Libp2pConfig struct {
	ListenAddr string
	Bootstrap  []string
	SelfID     consensus.NodeID
	Quorum     consensus.Quorum
	Logger     *zap.SugaredLogger
}

func NewLibp2pNet(ctx context.Context, cfg Libp2pConfig) (*Libp2pNet, error) {
	var opts []libp2p.Option
	if cfg.ListenAddr != "" {
		maddr, err := ma.NewMultiaddr(cfg.ListenAddr)
		if err != nil {
			return nil, err
		}
		opts = append(opts, libp2p.ListenAddrs(maddr))
	}
	h, err := libp2p.New(opts...)
	if err != nil {
		return nil, err
	}
	ps, err := pubsub.NewGossipSub(ctx, h)
	if err != nil {
		return nil, err
	}

	net := &Libp2pNet{
		h: h, ps: ps, log: cfg.Logger,
		self: cfg.SelfID, q: cfg.Quorum,
		votes:         make(map[consensus.View]map[consensus.Hash][]consensus.Vote),
		voteArrivedCh: make(chan struct{}, 100), // Buffered to avoid blocking vote handlers
		prepByV: make(map[consensus.View]struct {
			c consensus.Certificate
			b consensus.Block
		}),
	}

	for _, bs := range cfg.Bootstrap {
		if err := connectMultiaddr(ctx, h, bs); err != nil && cfg.Logger != nil {
			cfg.Logger.Warnw("bootstrap_connect_failed", "addr", bs, "err", err)
		}
	}

	if err := net.joinTopics(ctx); err != nil {
		return nil, err
	}

	// Set up stream handler for receiving votes (unicast)
	h.SetStreamHandler(protocolVote, net.handleVoteStream)

	go net.handlePropose(ctx)
	go net.handlePrepare(ctx)

	if cfg.Logger != nil {
		cfg.Logger.Infow("libp2p_ready", "peer", h.ID().String(), "listen", cfg.ListenAddr)
	}
	return net, nil
}

func connectMultiaddr(ctx context.Context, h host.Host, addr string) error {
	m, err := ma.NewMultiaddr(addr)
	if err != nil {
		return err
	}
	info, err := peer.AddrInfoFromP2pAddr(m)
	if err != nil {
		return err
	}
	return h.Connect(ctx, *info)
}

func (n *Libp2pNet) joinTopics(ctx context.Context) error {
	var err error
	if n.tPropose, err = n.ps.Join(topicPropose); err != nil {
		return err
	}
	if n.tPrepare, err = n.ps.Join(topicPrepare); err != nil {
		return err
	}

	if n.subPropose, err = n.tPropose.Subscribe(); err != nil {
		return err
	}
	if n.subPrepare, err = n.tPrepare.Subscribe(); err != nil {
		return err
	}
	return nil
}

// implement Network

func (n *Libp2pNet) SetHandlers(h consensus.Handlers) { n.muH.Lock(); n.handlers = h; n.muH.Unlock() }

func (n *Libp2pNet) Host() host.Host { return n.h }

func (n *Libp2pNet) BroadcastPropose(ctx context.Context, p consensus.Propose) error {
	bb, _ := gobEncode(p.Block)
	hh, _ := gobEncode(p.HighCert)
	data, err := gobEncode(ProposalWire{Block: bb, HighCert: hh})
	if err != nil {
		return err
	}
	return n.tPropose.Publish(ctx, data)
}

func (n *Libp2pNet) BroadcastPrepare(ctx context.Context, cert consensus.Certificate) error {
	cb, _ := gobEncode(cert)

	n.muPrep.Lock()
	var blk consensus.Block
	if entry, ok := n.prepByV[cert.View]; ok && entry.c.H == cert.H {
		blk = entry.b
	}
	n.muPrep.Unlock()

	bb, _ := gobEncode(blk)
	data, err := gobEncode(PrepareWire{Cert: cb, Block: bb})
	if err != nil {
		return err
	}
	return n.tPrepare.Publish(context.Background(), data)
}

func (n *Libp2pNet) SendVote(ctx context.Context, to consensus.NodeID, v consensus.Vote) error {
	// HotStuff: votes are sent directly to leader (unicast), not broadcast

	// Case 1: Sending to self (single-node or leader voting for own propose)
	if to == n.self {
		n.muVotes.Lock()
		if n.votes[v.View] == nil {
			n.votes[v.View] = make(map[consensus.Hash][]consensus.Vote)
		}
		n.votes[v.View][v.H] = append(n.votes[v.View][v.H], v)
		n.muVotes.Unlock()

		// Signal vote arrival (wake up CollectVotes immediately)
		select {
		case n.voteArrivedCh <- struct{}{}:
		default:
			// Channel full, skip signal (collector will check periodically anyway)
		}
		return nil
	}

	// Case 2: Multi-node - send to leader via libp2p stream
	// Find leader's peer ID (for now, assume NodeID maps to first connected peer)
	// TODO: Proper peer ID mapping from NodeID
	peers := n.h.Network().Peers()
	if len(peers) == 0 {
		return errors.New("no peers connected")
	}

	// Send to first peer (simplified - in production, map NodeID -> PeerID)
	var targetPeer peer.ID
	for _, p := range peers {
		targetPeer = p
		break
	}

	stream, err := n.h.NewStream(ctx, targetPeer, protocolVote)
	if err != nil {
		return err
	}
	defer stream.Close()

	// Encode and send vote
	data, err := gobEncode(v)
	if err != nil {
		return err
	}

	_, err = stream.Write(data)
	return err
}

// CollectVotes: Channel-based reactive collection (eliminates polling)
// Performance improvement: instant wake-up when threshold reached (was 0-50ms random delay)
func (n *Libp2pNet) CollectVotes(ctx context.Context, view consensus.View, h consensus.Hash, need int) ([]consensus.Vote, error) {
	deadline := time.NewTimer(3 * time.Second)
	defer deadline.Stop()

	// Check immediately if we already have enough votes (fast path)
	n.muVotes.Lock()
	got := n.votes[view][h]
	if len(got) >= need {
		result := make([]consensus.Vote, need)
		copy(result, got[:need])
		n.muVotes.Unlock()
		return result, nil
	}
	n.muVotes.Unlock()

	// Reactive wait: wake up on vote arrival or timeout
	for {
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-deadline.C:
			// Timeout: return what we have if threshold met, else error
			n.muVotes.Lock()
			out := append([]consensus.Vote(nil), n.votes[view][h]...)
			n.muVotes.Unlock()
			if len(out) >= need {
				return out[:need], nil
			}
			return nil, errors.New("timeout collecting votes")
		case <-n.voteArrivedCh:
			// Vote arrived: check if we have enough now
			n.muVotes.Lock()
			got := n.votes[view][h]
			if len(got) >= need {
				result := make([]consensus.Vote, need)
				copy(result, got[:need])
				n.muVotes.Unlock()
				return result, nil
			}
			n.muVotes.Unlock()
			// Not enough yet, continue waiting
		}
	}
}

// inbound

func (n *Libp2pNet) handlePropose(ctx context.Context) {
	for {
		msg, err := n.subPropose.Next(ctx)
		if err != nil {
			return
		}
		var w ProposalWire
		if err := gobDecode(msg.Data, &w); err != nil {
			continue
		}
		var blk consensus.Block
		var hc consensus.Certificate
		if err := gobDecode(w.Block, &blk); err != nil {
			continue
		}
		if err := gobDecode(w.HighCert, &hc); err != nil {
			continue
		}

		n.muH.RLock()
		h := n.handlers
		n.muH.RUnlock()
		if h.OnPropose != nil {
			h.OnPropose(ctx, consensus.Propose{Block: blk, HighCert: hc})
		}
	}
}

func (n *Libp2pNet) handlePrepare(ctx context.Context) {
	for {
		msg, err := n.subPrepare.Next(ctx)
		if err != nil {
			return
		}
		var w PrepareWire
		if err := gobDecode(msg.Data, &w); err != nil {
			continue
		}
		var cert consensus.Certificate
		_ = gobDecode(w.Cert, &cert)
		var blk consensus.Block
		if len(w.Block) > 0 {
			_ = gobDecode(w.Block, &blk)
		}

		// leader 수집용 보관(선택적)
		n.muPrep.Lock()
		n.prepByV[cert.View] = struct {
			c consensus.Certificate
			b consensus.Block
		}{c: cert, b: blk}
		n.muPrep.Unlock()

		n.muH.RLock()
		h := n.handlers
		n.muH.RUnlock()
		if h.OnPrepare != nil {
			h.OnPrepare(ctx, cert, blk)
		}
	}
}

// handleVoteStream: Receive votes via libp2p stream (unicast from followers to leader)
func (n *Libp2pNet) handleVoteStream(s network.Stream) {
	defer s.Close()

	// Read vote data from stream
	data, err := io.ReadAll(s)
	if err != nil {
		return
	}

	var v consensus.Vote
	if err := gobDecode(data, &v); err != nil {
		return
	}

	// Store vote (leader collects votes)
	n.muVotes.Lock()
	if n.votes[v.View] == nil {
		n.votes[v.View] = make(map[consensus.Hash][]consensus.Vote)
	}
	n.votes[v.View][v.H] = append(n.votes[v.View][v.H], v)
	n.muVotes.Unlock()

	// Signal vote arrival (wake up CollectVotes immediately)
	select {
	case n.voteArrivedCh <- struct{}{}:
	default:
		// Channel full, skip signal (collector will eventually timeout and check)
	}
}
