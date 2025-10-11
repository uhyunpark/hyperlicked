// file: pkg/crypto/ethaddr.go
package crypto

import (
	"encoding/hex"
	"strings"

	"golang.org/x/crypto/sha3"
)

// AddressFromUncompressedPub expects 65-byte uncompressed secp256k1 pubkey (0x04 || X || Y).
// Returns EIP-55 checksummed hex string like 0xABCD...
func AddressFromUncompressedPub(pub []byte) string {
	if len(pub) != 65 || pub[0] != 0x04 {
		return ""
	}
	// keccak256(pub[1:])
	h := sha3.NewLegacyKeccak256()
	h.Write(pub[1:])
	sum := h.Sum(nil)
	addr := sum[12:] // last 20 bytes
	return EIP55(addr)
}

// EIP55 computes the checksummed hex address string from 20-byte raw address.
func EIP55(addr20 []byte) string {
	hexaddr := hex.EncodeToString(addr20) // lower
	// keccak of lowercase hex
	h := sha3.NewLegacyKeccak256()
	h.Write([]byte(hexaddr))
	hash := h.Sum(nil)
	// apply checksum
	var out = make([]byte, 2+len(hexaddr))
	copy(out, []byte("0x"))
	for i, c := range []byte(hexaddr) {
		if c >= '0' && c <= '9' {
			out[2+i] = c
			continue
		}
		// if high nibble of corresponding hash byte >= 8, uppercase
		// (each hex char maps to 4 bits; i>>1 picks the byte; even/odd decides high/low nibble)
		hb := hash[i>>1]
		nibble := hb
		if i%2 == 0 {
			nibble = (hb >> 4) & 0x0f
		} else {
			nibble = hb & 0x0f
		}
		if nibble >= 8 {
			out[2+i] = byte(strings.ToUpper(string(c))[0])
		} else {
			out[2+i] = c
		}
	}
	return string(out)
}
