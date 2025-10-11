package storage

import (
	"bytes"
	"encoding/binary"
	"encoding/gob"

	"github.com/uhyunpark/hyperlicked/pkg/consensus"
)

func encodeGob(v any) ([]byte, error) {
	var buf bytes.Buffer
	if err := gob.NewEncoder(&buf).Encode(v); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}
func decodeGob(b []byte, v any) error {
	return gob.NewDecoder(bytes.NewReader(b)).Decode(v)
}

func viewKey(v consensus.View) []byte {
	var k [8]byte
	binary.BigEndian.PutUint64(k[:], uint64(v))
	return k[:]
}
