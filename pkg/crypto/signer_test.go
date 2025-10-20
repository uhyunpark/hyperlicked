package crypto

import (
	"testing"

	"github.com/ethereum/go-ethereum/common"
	eth_crypto "github.com/ethereum/go-ethereum/crypto"
)

func TestGenerateKey(t *testing.T) {
	signer, err := GenerateKey()
	if err != nil {
		t.Fatalf("failed to generate key: %v", err)
	}

	// Check address is valid
	if signer.Address() == (common.Address{}) {
		t.Error("generated zero address")
	}

	// Check private key hex is 64 chars (32 bytes)
	privHex := signer.PrivateKeyHex()
	if len(privHex) != 64 {
		t.Errorf("private key hex length = %d, want 64", len(privHex))
	}

	// Check public key hex is 130 chars (04 prefix + 64 bytes uncompressed)
	pubHex := signer.PublicKeyHex()
	if len(pubHex) != 130 {
		t.Errorf("public key hex length = %d, want 130", len(pubHex))
	}
}

func TestFromPrivateKeyHex(t *testing.T) {
	// Generate a key and use it for round-trip testing
	signer1, _ := GenerateKey()
	privHex := signer1.PrivateKeyHex()
	expectedAddr := signer1.Address()

	// Load from hex (no prefix)
	signer2, err := FromPrivateKeyHex(privHex)
	if err != nil {
		t.Fatalf("failed to load key: %v", err)
	}

	if signer2.Address() != expectedAddr {
		t.Errorf("address = %s, want %s", signer2.Address().Hex(), expectedAddr.Hex())
	}

	if signer2.PrivateKeyHex() != privHex {
		t.Errorf("private key mismatch after reload")
	}
}

func TestSignAndVerify(t *testing.T) {
	signer, _ := GenerateKey()

	// Sign a message hash (SignMessage internally hashes with Keccak256)
	message := []byte("Hello, HyperLicked!")
	signature, err := signer.SignMessage(message)
	if err != nil {
		t.Fatalf("failed to sign: %v", err)
	}

	// Signature should be 65 bytes [R || S || V]
	if len(signature) != 65 {
		t.Errorf("signature length = %d, want 65", len(signature))
	}

	// Verify signature (must use same hash as signing)
	hash := eth_crypto.Keccak256Hash(message).Bytes()
	valid := VerifySignature(signer.Address(), hash, signature)
	if !valid {
		t.Error("signature verification failed")
	}

	// Verify with wrong address
	wrongAddr := common.HexToAddress("0x0000000000000000000000000000000000000001")
	valid = VerifySignature(wrongAddr, hash, signature)
	if valid {
		t.Error("signature should not verify with wrong address")
	}
}

func TestRecoverAddress(t *testing.T) {
	signer, _ := GenerateKey()
	message := []byte("Test message")

	signature, err := signer.SignMessage(message)
	if err != nil {
		t.Fatalf("failed to sign: %v", err)
	}

	// Must use same hash as signing (Keccak256)
	hash := eth_crypto.Keccak256Hash(message).Bytes()
	recoveredAddr, err := RecoverAddress(hash, signature)
	if err != nil {
		t.Fatalf("failed to recover address: %v", err)
	}

	if recoveredAddr != signer.Address() {
		t.Errorf("recovered address = %s, want %s", recoveredAddr.Hex(), signer.Address().Hex())
	}
}

func TestSignatureToRSV(t *testing.T) {
	signer, _ := GenerateKey()
	message := []byte("RSV test")

	signature, _ := signer.SignMessage(message)

	r, s, v, err := SignatureToRSV(signature)
	if err != nil {
		t.Fatalf("failed to split signature: %v", err)
	}

	// Reconstruct signature
	reconstructed := RSVToSignature(r, s, v)

	// Should match original
	if len(reconstructed) != len(signature) {
		t.Errorf("reconstructed length = %d, want %d", len(reconstructed), len(signature))
	}

	for i := range signature {
		if reconstructed[i] != signature[i] {
			t.Errorf("byte %d mismatch: got %d, want %d", i, reconstructed[i], signature[i])
		}
	}
}

func TestGenerateNonce(t *testing.T) {
	nonce1, err := GenerateNonce()
	if err != nil {
		t.Fatalf("failed to generate nonce: %v", err)
	}

	nonce2, err := GenerateNonce()
	if err != nil {
		t.Fatalf("failed to generate second nonce: %v", err)
	}

	// Nonces should be different (statistically)
	if nonce1 == nonce2 {
		t.Error("generated identical nonces (unlikely but possible - retry test)")
	}
}

func TestInvalidSignature(t *testing.T) {
	signer, _ := GenerateKey()
	hash := common.BytesToHash([]byte("test")).Bytes()

	// Test invalid signature length
	invalidSig := []byte{1, 2, 3}
	valid := VerifySignature(signer.Address(), hash, invalidSig)
	if valid {
		t.Error("invalid signature should not verify")
	}

	// Test invalid hash length
	validSig := make([]byte, 65)
	valid = VerifySignature(signer.Address(), []byte("short"), validSig)
	if valid {
		t.Error("invalid hash should not verify")
	}
}
