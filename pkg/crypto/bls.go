package crypto

import (
	bls "github.com/cloudflare/circl/sign/bls"
)

type scheme = bls.KeyG1SigG2

type BLSPubKey = bls.PublicKey[scheme]
type BLSSignature = []byte

type BLSSigner struct {
	sk *bls.PrivateKey[scheme]
	pk *BLSPubKey
}

// for test
func NewBLSSignerFromSeed(seed []byte) *BLSSigner {
	sk, _ := bls.KeyGen[scheme](seed, nil, nil)
	pk := sk.PublicKey()
	return &BLSSigner{sk: sk, pk: pk}
}

func (s *BLSSigner) Pubkey() *BLSPubKey { return s.pk }

func (s *BLSSigner) Sign(msg []byte) []byte {
	return bls.Sign(s.sk, msg)
}

func Verify(pk *BLSPubKey, sigBytes, msg []byte) bool {
	return bls.Verify(pk, msg, bls.Signature(sigBytes))
}

// aggregate multiple signatures for the same message
func Aggregate(sigBytesList [][]byte) []byte {
	sigs := make([]bls.Signature, 0, len(sigBytesList))
	for _, sb := range sigBytesList {
		if len(sb) == 0 {
			continue
		}
		sigs = append(sigs, bls.Signature(sb))
	}
	agg, err := bls.Aggregate(bls.G1{}, sigs)
	if err != nil {
		return nil
	}
	return agg
}

func VerifyAggregateSameMsg(pks []*BLSPubKey, msg []byte, aggSig []byte) bool {
	return bls.VerifyAggregate(pks, [][]byte{msg}, bls.Signature(aggSig))
}
