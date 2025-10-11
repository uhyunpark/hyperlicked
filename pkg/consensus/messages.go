package consensus

type Propose struct {
	Block      Block
	HighCert   Certificate
	HighDouble *DoubleCert // optional fast path
}
