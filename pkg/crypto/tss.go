package crypto

type SigShare []byte

type ThresholdSigner interface {
	SignShare(msg []byte) (SigShare, error)
	Combine(shares [][]byte) ([]byte, error)
	Verify(sig []byte, msg []byte) bool
}

type DummySigner struct{}

func (DummySigner) SignShare(msg []byte) (SigShare, error) { return append([]byte{}, msg...), nil }
func (DummySigner) Combine(_ [][]byte) ([]byte, error)     { return []byte("agg"), nil }
func (DummySigner) Verify(_ []byte, _ []byte) bool         { return true }
