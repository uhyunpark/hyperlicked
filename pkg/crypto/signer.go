package crypto

import (
	"crypto/ecdsa"
	"crypto/rand"
	"fmt"
	"math/big"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/crypto"
)

// Signer manages ECDSA key pairs for signing transactions
// Uses secp256k1 curve (Ethereum-compatible)
type Signer struct {
	privateKey *ecdsa.PrivateKey
	publicKey  *ecdsa.PublicKey
	address    common.Address
}

// GenerateKey creates a new random secp256k1 key pair
// Returns a Signer with private key, public key, and derived Ethereum address
func GenerateKey() (*Signer, error) {
	privateKey, err := crypto.GenerateKey()
	if err != nil {
		return nil, fmt.Errorf("failed to generate key: %w", err)
	}

	publicKey := privateKey.Public()
	publicKeyECDSA, ok := publicKey.(*ecdsa.PublicKey)
	if !ok {
		return nil, fmt.Errorf("failed to cast public key to ECDSA")
	}

	address := crypto.PubkeyToAddress(*publicKeyECDSA)

	return &Signer{
		privateKey: privateKey,
		publicKey:  publicKeyECDSA,
		address:    address,
	}, nil
}

// FromPrivateKeyHex creates a Signer from a hex-encoded private key
// Format: "0x1234..." or "1234..." (64 hex chars)
func FromPrivateKeyHex(hexKey string) (*Signer, error) {
	privateKey, err := crypto.HexToECDSA(hexKey)
	if err != nil {
		return nil, fmt.Errorf("failed to parse private key: %w", err)
	}

	publicKey := privateKey.Public()
	publicKeyECDSA, ok := publicKey.(*ecdsa.PublicKey)
	if !ok {
		return nil, fmt.Errorf("failed to cast public key to ECDSA")
	}

	address := crypto.PubkeyToAddress(*publicKeyECDSA)

	return &Signer{
		privateKey: privateKey,
		publicKey:  publicKeyECDSA,
		address:    address,
	}, nil
}

// Address returns the Ethereum address derived from the public key
func (s *Signer) Address() common.Address {
	return s.address
}

// PrivateKeyHex returns the private key as hex string (WITHOUT 0x prefix)
// WARNING: Keep this secret! Never expose to users or logs
func (s *Signer) PrivateKeyHex() string {
	return fmt.Sprintf("%x", crypto.FromECDSA(s.privateKey))
}

// PublicKeyHex returns the public key as hex string (uncompressed, 128 chars)
func (s *Signer) PublicKeyHex() string {
	return fmt.Sprintf("%x", crypto.FromECDSAPub(s.publicKey))
}

// Sign signs a message hash using ECDSA and returns the signature
// Returns signature in [R || S || V] format (65 bytes)
// V is recovery ID (0 or 1) + 27 for Ethereum compatibility
func (s *Signer) Sign(hash []byte) ([]byte, error) {
	if len(hash) != 32 {
		return nil, fmt.Errorf("hash must be 32 bytes, got %d", len(hash))
	}

	signature, err := crypto.Sign(hash, s.privateKey)
	if err != nil {
		return nil, fmt.Errorf("failed to sign: %w", err)
	}

	return signature, nil
}

// SignMessage signs a message (not a hash) by first hashing it with Keccak256
// Use this for arbitrary byte messages
func (s *Signer) SignMessage(message []byte) ([]byte, error) {
	hash := crypto.Keccak256Hash(message)
	return s.Sign(hash.Bytes())
}

// VerifySignature verifies that signature was created by address for given hash
// Returns true if signature is valid, false otherwise
func VerifySignature(address common.Address, hash []byte, signature []byte) bool {
	if len(signature) != 65 {
		return false
	}
	if len(hash) != 32 {
		return false
	}

	// Recover public key from signature
	publicKeyBytes, err := crypto.Ecrecover(hash, signature)
	if err != nil {
		return false
	}

	// Convert to ECDSA public key
	publicKey, err := crypto.UnmarshalPubkey(publicKeyBytes)
	if err != nil {
		return false
	}

	// Derive address from recovered public key
	recoveredAddr := crypto.PubkeyToAddress(*publicKey)

	// Compare addresses
	return recoveredAddr == address
}

// RecoverAddress recovers the signer's address from a message hash and signature
// Returns the address that created the signature
func RecoverAddress(hash []byte, signature []byte) (common.Address, error) {
	if len(signature) != 65 {
		return common.Address{}, fmt.Errorf("invalid signature length: %d", len(signature))
	}
	if len(hash) != 32 {
		return common.Address{}, fmt.Errorf("invalid hash length: %d", len(hash))
	}

	// Recover public key
	publicKeyBytes, err := crypto.Ecrecover(hash, signature)
	if err != nil {
		return common.Address{}, fmt.Errorf("failed to recover public key: %w", err)
	}

	// Convert to ECDSA public key
	publicKey, err := crypto.UnmarshalPubkey(publicKeyBytes)
	if err != nil {
		return common.Address{}, fmt.Errorf("failed to unmarshal public key: %w", err)
	}

	// Derive address
	address := crypto.PubkeyToAddress(*publicKey)
	return address, nil
}

// SignatureToRSV splits a 65-byte signature into R, S, V components
// Useful for Solidity verification or debugging
func SignatureToRSV(signature []byte) (r, s *big.Int, v uint8, err error) {
	if len(signature) != 65 {
		return nil, nil, 0, fmt.Errorf("invalid signature length: %d", len(signature))
	}

	r = new(big.Int).SetBytes(signature[:32])
	s = new(big.Int).SetBytes(signature[32:64])
	v = signature[64]

	return r, s, v, nil
}

// RSVToSignature combines R, S, V into a 65-byte signature
func RSVToSignature(r, s *big.Int, v uint8) []byte {
	signature := make([]byte, 65)
	copy(signature[:32], r.Bytes())
	copy(signature[32:64], s.Bytes())
	signature[64] = v
	return signature
}

// GenerateNonce generates a cryptographically secure random nonce
// Used for replay protection (incremental nonces are preferred, but random works for testing)
func GenerateNonce() (uint64, error) {
	var nonce uint64
	nonceBytes := make([]byte, 8)
	if _, err := rand.Read(nonceBytes); err != nil {
		return 0, fmt.Errorf("failed to generate nonce: %w", err)
	}
	nonce = uint64(nonceBytes[0]) | uint64(nonceBytes[1])<<8 | uint64(nonceBytes[2])<<16 |
		uint64(nonceBytes[3])<<24 | uint64(nonceBytes[4])<<32 | uint64(nonceBytes[5])<<40 |
		uint64(nonceBytes[6])<<48 | uint64(nonceBytes[7])<<56
	return nonce, nil
}
