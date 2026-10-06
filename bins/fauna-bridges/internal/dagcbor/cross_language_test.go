// Cross-language interop verifier — reads the Rust-produced fixture under
// libs/fauna-cbor/tests/fixtures/cross-language/ and re-verifies it from
// Go using only blake3 + ed25519 + this package's canonical-form validator.
//
// Proves the load-bearing claim of docs/goal/architecture/serialization.md
// § 4: any language with BLAKE3 + Ed25519 + a dag-cbor decoder can verify
// Rust-produced content. The Rust producer is
// libs/fauna-cbor/examples/gen_cross_language_fixtures.rs; if either side
// changes the fixture shape the other must regenerate or the test fails.
//
// Fixture layout (3 files, deterministic from fixed seed [7u8; 32]):
//
//   rust-signed.bin           — canonical dag-cbor bytes for the demo payload.
//   rust-signed.envelope.bin  — 100 bytes: 36-byte CID || 64-byte Ed25519 sig.
//   rust-pubkey.bin           — 32 bytes: the Ed25519 verifying key.
//
// CID layout (IPLD v1 dag-cbor + blake3-256, 36 bytes):
//
//   byte 0:     0x01 (CID version 1)
//   byte 1:     0x71 (multicodec: dag-cbor)
//   byte 2:     0x1e (multihash code: blake3-256)
//   byte 3:     0x20 (multihash length: 32)
//   bytes 4-35: 32-byte BLAKE3 digest of the signed bytes
//
// The signature is over the full 36-byte CID, not just the digest.

package dagcbor

import (
	"bytes"
	"crypto/ed25519"
	"os"
	"path/filepath"
	"testing"

	"lukechampine.com/blake3"
)

// fixtureDir is the path from this package's directory
// (bins/fauna-bridges/internal/dagcbor) to the cross-language fixture
// directory at repo root. Go tests run with the package dir as cwd, so the
// relative path is `../../../../libs/fauna-cbor/tests/fixtures/cross-language`.
const fixtureDir = "../../../../libs/fauna-cbor/tests/fixtures/cross-language"

// TestRustSignedVerifiesInGo runs the three-step verification pipeline on
// the Rust-produced fixture:
//
//  1. dagcbor.ValidateCanonical — the signed bytes are canonical dag-cbor.
//  2. blake3(bytes) == cid.multihash — the encoder is not in the security path.
//  3. ed25519.Verify(pk, cid, sig) — the signature covers the 36-byte CID.
//
// All three must pass for the load-bearing claim to hold.
func TestRustSignedVerifiesInGo(t *testing.T) {
	signedBytes, err := os.ReadFile(filepath.Join(fixtureDir, "rust-signed.bin"))
	if err != nil {
		t.Fatalf("read rust-signed.bin: %v (run `cargo run -p fauna-cbor --example gen_cross_language_fixtures` to regenerate)", err)
	}
	envelopeBytes, err := os.ReadFile(filepath.Join(fixtureDir, "rust-signed.envelope.bin"))
	if err != nil {
		t.Fatalf("read rust-signed.envelope.bin: %v", err)
	}
	pubKeyBytes, err := os.ReadFile(filepath.Join(fixtureDir, "rust-pubkey.bin"))
	if err != nil {
		t.Fatalf("read rust-pubkey.bin: %v", err)
	}

	// Envelope shape: 36-byte CID || 64-byte sig = 100 bytes.
	if got, want := len(envelopeBytes), 100; got != want {
		t.Fatalf("envelope length %d, want %d (36-byte CID + 64-byte sig)", got, want)
	}
	cidBytes := envelopeBytes[0:36]
	sigBytes := envelopeBytes[36:100]

	// Pubkey shape: 32 bytes (Ed25519 verifying key).
	if got, want := len(pubKeyBytes), ed25519.PublicKeySize; got != want {
		t.Fatalf("pubkey length %d, want %d", got, want)
	}

	// CID prefix shape: v1 + dag-cbor + blake3-256 + len 32.
	wantPrefix := []byte{0x01, 0x71, 0x1e, 0x20}
	if !bytes.Equal(cidBytes[:4], wantPrefix) {
		t.Fatalf("CID prefix %x, want %x (v1 dag-cbor + blake3-256)", cidBytes[:4], wantPrefix)
	}

	// Step 1: the signed bytes must be canonical dag-cbor.
	if err := ValidateCanonical(signedBytes); err != nil {
		t.Fatalf("ValidateCanonical(signedBytes): %v", err)
	}

	// Step 2: blake3(bytes) must equal the CID's embedded digest. No
	// CBOR decoder runs in this check — it's pure hash over the wire bytes.
	expectedDigest := blake3.Sum256(signedBytes)
	if !bytes.Equal(cidBytes[4:36], expectedDigest[:]) {
		t.Fatalf("CID digest mismatch: got %x, blake3(bytes) = %x", cidBytes[4:36], expectedDigest[:])
	}

	// Step 3: Ed25519 verifies over the full 36-byte CID (matching the
	// Rust signer's sk.sign(cid.as_bytes()) call).
	if !ed25519.Verify(ed25519.PublicKey(pubKeyBytes), cidBytes, sigBytes) {
		t.Fatalf("ed25519.Verify failed: pubkey=%x cid=%x sig=%x", pubKeyBytes, cidBytes, sigBytes)
	}
}
