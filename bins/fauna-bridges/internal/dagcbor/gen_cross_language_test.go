// Cross-language interop fixture producer — Go signs, Rust verifies.
//
// This is the second half of Track C: the Rust-signed direction lives in
// cross_language_test.go (TestRustSignedVerifiesInGo). This file produces
// the Go-signed fixture under testdata/ that
// libs/fauna-cbor/tests/cross_language_interop.rs reads + verifies.
//
// Idiomatic Go convention is a -update flag: without it the test is a
// no-op; with `go test -update` it regenerates the three vendored files.
// Verification runs on the Rust side (cargo test -p fauna-cbor --test
// cross_language_interop) so there is no read-back assertion here.
//
// Byte-parity sanity check: the Demo payload is identical to the Rust
// producer (libs/fauna-cbor/examples/gen_cross_language_fixtures.rs), so
// dagcbor.Marshal MUST produce the same wire bytes as Rust's
// encode_canonical. The test compares against rust-signed.bin and fails
// loudly if they diverge — the load-bearing claim of
// docs/goal/architecture/serialization.md § 4 is "the same canonical
// encoder in two languages produces byte-identical output".
//
// Fixture layout (3 files, deterministic from fixed seed [7u8; 32]):
//
//   testdata/go-signed.bin           — canonical dag-cbor bytes for the demo payload.
//   testdata/go-signed.envelope.bin  — 100 bytes: 36-byte CID || 64-byte Ed25519 sig.
//   testdata/go-pubkey.bin           — 32 bytes: the Ed25519 verifying key.
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
	"flag"
	"os"
	"path/filepath"
	"testing"

	"lukechampine.com/blake3"
)

// update toggles fixture regeneration. The flag is registered at package
// load so `go test -update` works without any extra wiring.
var update = flag.Bool("update", false, "regenerate cross-language interop fixture files under testdata/")

// demo mirrors the Rust producer's `Demo` struct
// (libs/fauna-cbor/examples/gen_cross_language_fixtures.rs). Field names
// and order match exactly so the canonical-form length-first-then-bytewise
// key sort produces the same wire bytes on both sides.
//
// Sorted key order on the wire: v (len 1) < data (len 4, 'd'<'n') < name (len 4).
type demo struct {
	V    uint32 `cbor:"v"`
	Name string `cbor:"name"`
	Data []byte `cbor:"data"`
}

// rustSignedFixturePath is the relative path from this package's directory
// (tests run with the package dir as cwd) to the Rust producer's vendored
// signed-bytes fixture. Used only for the byte-parity sanity check; if the
// file is missing, the parity check is skipped (with a t.Logf) and
// regeneration still proceeds — the Rust runner is the authoritative
// verifier.
const rustSignedFixturePath = "../../../../libs/fauna-cbor/tests/fixtures/cross-language/rust-signed.bin"

// TestGenCrossLanguage regenerates the Go-signed cross-language fixture.
//
// Without `-update` the test is a no-op (t.Skip). With `-update`:
//
//  1. Marshal a demo payload via dagcbor.Marshal (canonical encMode).
//  2. Compare bytes to rust-signed.bin — must be byte-identical because
//     both producers run the same canonical-form encoder against the same
//     struct values. A mismatch means one side's canonical encoder is
//     broken; we fail the regeneration rather than silently writing a
//     fixture the Rust verifier would reject.
//  3. Compute blake3(bytes), build the 36-byte CID.
//  4. Sign the CID with Ed25519 using the fixed seed [7u8; 32].
//  5. Write the 3 files under testdata/.
//
// Determinism: the seed is fixed, the payload is fixed, so the output is
// reproducible byte-for-byte across runs and machines.
func TestGenCrossLanguage(t *testing.T) {
	if !*update {
		t.Skip("run with -update to regenerate testdata/go-signed.* (verification lives in the Rust runner)")
	}

	// Fixed seed matches the Rust producer so the verifying key, signature,
	// and (incidentally) the testdata bytes are stable across re-runs.
	seed := make([]byte, ed25519.SeedSize)
	for i := range seed {
		seed[i] = 7
	}
	sk := ed25519.NewKeyFromSeed(seed)
	pk := sk.Public().(ed25519.PublicKey)

	payload := demo{
		V:    42,
		Name: "fauna",
		Data: []byte{0x01, 0x02, 0x03, 0x04, 0x05},
	}

	signedBytes, err := Marshal(payload)
	if err != nil {
		t.Fatalf("dagcbor.Marshal(payload): %v", err)
	}

	// Byte-parity sanity check against the Rust producer's output. This is
	// the load-bearing assertion: two independent canonical-form encoders
	// (serde_ipld_dagcbor in Rust, fxamacker/cbor with SortLengthFirst in
	// Go) MUST emit the same wire bytes for the same Demo values. A
	// mismatch is a real bug — escalate, do not write a divergent fixture.
	if rustBytes, err := os.ReadFile(rustSignedFixturePath); err == nil {
		if !bytes.Equal(signedBytes, rustBytes) {
			t.Logf("Rust: %x", rustBytes)
			t.Logf("Go:   %x", signedBytes)
			t.Fatal("byte parity broken: Go canonical encoder output != Rust canonical encoder output for the same Demo value (the load-bearing claim of docs/goal/architecture/serialization.md § 4)")
		}
		t.Logf("byte-parity sanity check passed: %d bytes match rust-signed.bin", len(signedBytes))
	} else {
		t.Logf("skipping byte-parity sanity check: %v (rust fixture not available; regen still proceeds)", err)
	}

	// Build the 36-byte CID: v1 dag-cbor + blake3-256 + 32-byte digest.
	digest := blake3.Sum256(signedBytes)
	cidBytes := make([]byte, 0, 36)
	cidBytes = append(cidBytes, 0x01, 0x71, 0x1e, 0x20)
	cidBytes = append(cidBytes, digest[:]...)

	// Sign the full 36-byte CID (NOT just the digest, NOT the signed
	// bytes — matches Rust's sk.sign(cid.as_bytes())).
	sig := ed25519.Sign(sk, cidBytes)
	if len(sig) != ed25519.SignatureSize {
		t.Fatalf("ed25519.Sign returned %d-byte sig, want %d", len(sig), ed25519.SignatureSize)
	}

	// Envelope wire layout: 36-byte CID || 64-byte sig = 100 bytes total.
	envelope := make([]byte, 0, 100)
	envelope = append(envelope, cidBytes...)
	envelope = append(envelope, sig...)
	if len(envelope) != 100 {
		t.Fatalf("envelope length %d, want 100 (36 + 64)", len(envelope))
	}

	// Write all three files under testdata/. Tests run with the package
	// directory as cwd, so a relative testdata/ path resolves correctly.
	const testdataDir = "testdata"
	if err := os.MkdirAll(testdataDir, 0o755); err != nil {
		t.Fatalf("create testdata dir: %v", err)
	}

	files := []struct {
		name string
		data []byte
	}{
		{filepath.Join(testdataDir, "go-signed.bin"), signedBytes},
		{filepath.Join(testdataDir, "go-signed.envelope.bin"), envelope},
		{filepath.Join(testdataDir, "go-pubkey.bin"), []byte(pk)},
	}
	for _, f := range files {
		if err := os.WriteFile(f.name, f.data, 0o644); err != nil {
			t.Fatalf("write %s: %v", f.name, err)
		}
		t.Logf("wrote %s (%d bytes)", f.name, len(f.data))
	}
}
