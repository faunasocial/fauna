package atprotoid

import (
	"bytes"
	"crypto/sha256"
	"strings"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/mr-tron/base58"
)

// fixedTestScalar is a deterministic 32-byte K-256 scalar for
// reproducibility-sensitive assertions (any 256-bit value below the curve
// order is valid; a hash output is below it with overwhelming probability —
// and PrivateKeyFromK256Scalar would loudly error otherwise).
func fixedTestScalar() []byte {
	h := sha256.Sum256([]byte("fauna atprotoid fixed test scalar (NOT a real key)"))
	return h[:]
}

// TestPrivateKeyFromK256ScalarRoundTrip proves the self-built private-key
// multibase construction against indigo's own encoding: generate a key via
// atcrypto, extract the raw scalar from its Multibase() (strip the varint
// multicodec prefix 0x1301 = bytes 0x81 0x26), feed the scalar through
// PrivateKeyFromK256Scalar, and require the reconstructed key to be
// byte-identical (same Multibase, same did:key pubkey).
func TestPrivateKeyFromK256ScalarRoundTrip(t *testing.T) {
	orig, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatalf("GeneratePrivateKeyK256: %v", err)
	}
	mb := orig.Multibase()
	if !strings.HasPrefix(mb, "z") {
		t.Fatalf("generated key multibase %q lacks the base58btc 'z' prefix", mb)
	}
	raw, err := base58.Decode(mb[1:])
	if err != nil {
		t.Fatalf("base58 decode: %v", err)
	}
	if !bytes.HasPrefix(raw, k256PrivMulticodecVarint) {
		t.Fatalf("decoded multibase does not start with the secp256k1-priv varint %x: %x", k256PrivMulticodecVarint, raw[:4])
	}
	scalar := raw[len(k256PrivMulticodecVarint):]
	if len(scalar) != 32 {
		t.Fatalf("extracted scalar is %d bytes, want 32", len(scalar))
	}

	rebuilt, err := PrivateKeyFromK256Scalar(scalar)
	if err != nil {
		t.Fatalf("PrivateKeyFromK256Scalar: %v", err)
	}
	if got := rebuilt.Multibase(); got != mb {
		t.Errorf("rebuilt key multibase %q != original %q", got, mb)
	}
	origDID, err := DIDKeyForPrivate(orig)
	if err != nil {
		t.Fatalf("DIDKeyForPrivate(orig): %v", err)
	}
	rebuiltDID, err := DIDKeyForPrivate(rebuilt)
	if err != nil {
		t.Fatalf("DIDKeyForPrivate(rebuilt): %v", err)
	}
	if origDID != rebuiltDID {
		t.Errorf("rebuilt key did:key %q != original %q", rebuiltDID, origDID)
	}
}

// TestScalarSignVerify proves the scalar → key → sign → verify path with a
// fixed scalar: the signature must verify against the public key derived from
// the very same scalar (the shape the mint loop uses: FFI bundle scalar in,
// did:key pubkey out).
func TestScalarSignVerify(t *testing.T) {
	scalar := fixedTestScalar()
	key, err := PrivateKeyFromK256Scalar(scalar)
	if err != nil {
		t.Fatalf("PrivateKeyFromK256Scalar: %v", err)
	}
	didKey, err := DIDKeyForPrivate(key)
	if err != nil {
		t.Fatalf("DIDKeyForPrivate: %v", err)
	}
	if !strings.HasPrefix(didKey, "did:key:z") {
		t.Errorf("did:key %q lacks the did:key:z prefix", didKey)
	}
	// Deterministic: same scalar, same key.
	again, err := PrivateKeyFromK256Scalar(scalar)
	if err != nil {
		t.Fatalf("PrivateKeyFromK256Scalar(again): %v", err)
	}
	againDID, err := DIDKeyForPrivate(again)
	if err != nil {
		t.Fatalf("DIDKeyForPrivate(again): %v", err)
	}
	if againDID != didKey {
		t.Errorf("same scalar produced different did:keys: %q vs %q", didKey, againDID)
	}

	msg := []byte("fauna atprotoid sign/verify probe")
	sig, err := key.HashAndSign(msg)
	if err != nil {
		t.Fatalf("HashAndSign: %v", err)
	}
	if len(sig) != 64 {
		t.Errorf("signature is %d bytes, want fixed-size r||s = 64", len(sig))
	}
	pub, err := ParsePublicDIDKey(didKey)
	if err != nil {
		t.Fatalf("ParsePublicDIDKey: %v", err)
	}
	if err := pub.HashAndVerify(msg, sig); err != nil {
		t.Errorf("signature does not verify against the scalar's own pubkey: %v", err)
	}
	if err := pub.HashAndVerify([]byte("tampered"), sig); err == nil {
		t.Error("signature verified over tampered message; want failure")
	}
}

// TestPrivateKeyFromK256ScalarRejectsBadLength locks the 32-byte contract.
func TestPrivateKeyFromK256ScalarRejectsBadLength(t *testing.T) {
	for _, n := range []int{0, 16, 31, 33, 64} {
		if _, err := PrivateKeyFromK256Scalar(make([]byte, n)); err == nil {
			t.Errorf("PrivateKeyFromK256Scalar accepted a %d-byte scalar; want error", n)
		}
	}
}

// TestMultibaseFromDIDKey pins the did:key ↔ publicKeyMultibase relationship
// the did:web doc builder relies on.
func TestMultibaseFromDIDKey(t *testing.T) {
	key, err := PrivateKeyFromK256Scalar(fixedTestScalar())
	if err != nil {
		t.Fatalf("PrivateKeyFromK256Scalar: %v", err)
	}
	didKey, err := DIDKeyForPrivate(key)
	if err != nil {
		t.Fatalf("DIDKeyForPrivate: %v", err)
	}
	mb, err := MultibaseFromDIDKey(didKey)
	if err != nil {
		t.Fatalf("MultibaseFromDIDKey: %v", err)
	}
	if "did:key:"+mb != didKey {
		t.Errorf("round trip broken: did:key:%s != %s", mb, didKey)
	}
	if _, err := MultibaseFromDIDKey("zNotADidKey"); err == nil {
		t.Error("MultibaseFromDIDKey accepted a non-did:key string")
	}
}
