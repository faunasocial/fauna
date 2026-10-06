package tls

import (
	"bytes"
	"testing"

	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// Verifies the seal → unseal round-trip across the regenerated UniFFI
// surface — SealTlsCertBlob (a recently added export)
// followed by the existing UnsealTlsCertBlob. Mirrors the admin-uploaded
// TLS path documented in docs/goal/behavior/mail-bridge-lifecycle.md
// § TLS provisioning (two paths): admin client serializes a TlsCertBundle
// to canonical DAG-CBOR → seals → ships bytes to nest → bridge fetches +
// unseals. This test exercises the seal-then-unseal half end-to-end in
// the same process, so a future ciphertext-shape drift between the seal
// and unseal halves of the FFI fails here loudly.
func TestSealUnsealTlsCertBlob_RoundTrip(t *testing.T) {
	type bundle struct {
		CertChain []byte `cbor:"cert_chain"`
		PrivKey   []byte `cbor:"priv_key"`
		ExpiresAt uint64 `cbor:"expires_at"`
		IssuedAt  uint64 `cbor:"issued_at"`
	}
	b := bundle{
		CertChain: []byte("-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----"),
		PrivKey:   bytes.Repeat([]byte{0xCD}, 32),
		ExpiresAt: 1_700_000_000 + 90*86_400,
		IssuedAt:  1_700_000_000,
	}
	bundleBytes, err := dagcbor.Marshal(b)
	if err != nil {
		t.Fatalf("encode bundle: %v", err)
	}

	kp := fauna_ffi.GenerateX25519Keypair()

	blob, err := fauna_ffi.SealTlsCertBlob(
		bundleBytes,
		"mta",
		"bridge-1",
		"example.com",
		kp.Pubkey,
	)
	if err != nil {
		t.Fatalf("seal: %v", err)
	}

	got, err := fauna_ffi.UnsealTlsCertBlob(blob, kp.Secret)
	if err != nil {
		t.Fatalf("unseal: %v", err)
	}

	if !bytes.Equal(got.CertChain, b.CertChain) {
		t.Errorf("cert_chain mismatch: got %q want %q", got.CertChain, b.CertChain)
	}
	if !bytes.Equal(got.PrivKey, b.PrivKey) {
		t.Errorf("priv_key mismatch")
	}
	if got.ExpiresAt != b.ExpiresAt {
		t.Errorf("expires_at: got %d want %d", got.ExpiresAt, b.ExpiresAt)
	}
	if got.IssuedAt != b.IssuedAt {
		t.Errorf("issued_at: got %d want %d", got.IssuedAt, b.IssuedAt)
	}
}

// Wrong-recipient case: sealing to one X25519 pubkey then attempting to
// unseal with a different secret must surface an HPKE error. The inner
// Rust test for this lives at libs/fauna-mls/src/wrapped_blob/mod.rs (the
// `tls_wrong_recipient_fails` test); this Go twin verifies the error
// surfaces through the FFI-layer FfiError::General flattening that
// UnsealTlsCertBlob returns.
func TestSealUnsealTlsCertBlob_WrongRecipientFails(t *testing.T) {
	type bundle struct {
		CertChain []byte `cbor:"cert_chain"`
		PrivKey   []byte `cbor:"priv_key"`
		ExpiresAt uint64 `cbor:"expires_at"`
		IssuedAt  uint64 `cbor:"issued_at"`
	}
	b := bundle{
		CertChain: []byte("x"),
		PrivKey:   bytes.Repeat([]byte{0}, 32),
		ExpiresAt: 2,
		IssuedAt:  1,
	}
	bundleBytes, err := dagcbor.Marshal(b)
	if err != nil {
		t.Fatalf("encode bundle: %v", err)
	}

	kp := fauna_ffi.GenerateX25519Keypair()
	wrongKp := fauna_ffi.GenerateX25519Keypair()

	blob, err := fauna_ffi.SealTlsCertBlob(
		bundleBytes,
		"mta",
		"bridge-1",
		"example.com",
		kp.Pubkey,
	)
	if err != nil {
		t.Fatalf("seal: %v", err)
	}

	if _, err := fauna_ffi.UnsealTlsCertBlob(blob, wrongKp.Secret); err == nil {
		t.Fatalf("unseal with wrong recipient secret unexpectedly succeeded")
	}
}
