// file: pkg/consensus/types.go
package consensus

import (
	"crypto/sha256"
	"encoding/binary"
	"fmt"
	"time"
)

type NodeID string
type View uint64
type Height uint64

type Quorum struct{ N, T int } // N=3t+1, T=t

type Hash [32]byte

func (h Hash) String() string { return fmt.Sprintf("%x", h[:]) }

type Block struct {
	Height   Height
	View     View
	Parent   Hash
	AppHash  Hash // Hash of application state after executing this block
	Payload  []byte
	Proposer NodeID
	Time     time.Time
}

type Certificate struct {
	View    View
	H       Hash // Consensus hash (transactions)
	AppHash Hash // Application state hash (state after execution)
	Sig     []byte
}

type DoubleCert struct{ C1, C2 Certificate }

type Vote struct {
	View     View
	H        Hash // Consensus hash (transactions)
	AppHash  Hash // Application state hash (state after execution)
	SigShare []byte
	From     NodeID
}

type Locked struct {
	Block Block
	Cert  Certificate
}

// HashOfBlock computes the consensus hash of a block.
// Following Tendermint/HotStuff architecture, this hash commits to CONSENSUS data only:
//   - Block structure (height, view, parent)
//   - Transaction payload
//   - Proposer and timestamp
//
// IMPORTANT: AppHash is NOT included in this hash. Why?
//  1. Blocks are proposed BEFORE execution (AppHash unknown at proposal time)
//  2. Certificate.H commits to consensus data, not application state
//  3. AppHash is verified separately by validators after execution
//  4. This separation allows pipelined execution in BFT consensus
//
// State verification happens via:
//   - Validators execute block deterministically
//   - Compare resulting AppHash with other validators
//   - Mismatch → halt and investigate (Byzantine fault)
//
// This matches:
//   - Tendermint: BlockHash (header) vs AppHash (state root)
//   - Cosmos SDK: Block commitment vs IAVL state root
//   - HotStuff family: QC over block vs application state
func HashOfBlock(b Block) Hash {
	h := sha256.New()

	// Height (8 bytes)
	var heightBuf [8]byte
	binary.BigEndian.PutUint64(heightBuf[:], uint64(b.Height))
	h.Write(heightBuf[:])

	// View (8 bytes)
	var viewBuf [8]byte
	binary.BigEndian.PutUint64(viewBuf[:], uint64(b.View))
	h.Write(viewBuf[:])

	// Parent hash (32 bytes)
	h.Write(b.Parent[:])

	// NOTE: AppHash is NOT included in consensus hash
	// It's set after execution and verified separately

	// Payload (variable length)
	h.Write(b.Payload)

	// Proposer (variable length string)
	h.Write([]byte(b.Proposer))

	// Time - 결정론적 합의를 위해 포함하지만,
	// 프로덕션에서는 타임스탬프 대신 view/height만 사용 권장
	var timeBuf [8]byte
	binary.BigEndian.PutUint64(timeBuf[:], uint64(b.Time.UnixNano()))
	h.Write(timeBuf[:])

	return sha256.Sum256(h.Sum(nil))
}

// ---- Storage/WAL interfaces (impl in pkg/storage) ----

type BlockStore interface {
	SaveBlock(b Block)
	GetBlock(h Hash) (Block, bool)
	SaveCert(c Certificate)
	GetCert(v View) (Certificate, bool)
	SetCommitted(h Hash)
	GetCommitted() (Hash, bool)
}

type WAL interface {
	Append(line string)
}
