package p2p

import (
	"bytes"
	"encoding/gob"
)

func init() {
	gob.Register(ProposalWire{})
	gob.Register(PrepareWire{})
	gob.Register(VoteWire{})
}

type ProposalWire struct {
	Block    []byte // gob-encoded consensus.Block
	HighCert []byte // gob-encoded consensus.Certificate
}

type PrepareWire struct {
	Cert  []byte // gob-encoded consensus.Certificate
	Block []byte // gob-encoded consensus.Block (optional)
}

type VoteWire struct {
	Vote []byte // gob-encoded consensus.Vote
}

func gobEncode(v any) ([]byte, error) {
	var buf bytes.Buffer
	if err := gob.NewEncoder(&buf).Encode(v); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}
func gobDecode(b []byte, v any) error {
	return gob.NewDecoder(bytes.NewReader(b)).Decode(v)
}
