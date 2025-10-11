package consensus

import "time"

type State struct {
	Q        Quorum
	SelfID   NodeID
	Height   Height
	View     View
	Locked   *Locked
	HighCert *Certificate
	Blocks   map[Hash]Block
	Genesis  Block
}

func GenesisBlock() Block {
	return Block{
		Height: 0, View: 0, Parent: Hash{},
		Payload: nil, Proposer: NodeID("genesis"), Time: time.Unix(0, 0),
	}
}
