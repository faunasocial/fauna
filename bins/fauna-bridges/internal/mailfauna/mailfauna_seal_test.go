package mailfauna

import (
	"bytes"
	"crypto/ecdh"
	"crypto/rand"
	"testing"
)

// EncryptToRecipient is the Go-side wrapper over libs/fauna-ffi's
// `seal_to_recipient` (HPKE-Seal). Cryptographic correctness (round-trip
// against unseal_mail_record, AAD/info domain separation, KEM
// per-recipient binding) is pinned by the Rust unit tests in
// libs/fauna-mls/src/wrapped_blob/mod.rs::seal_tests; these tests pin
// the Go-side wire contract (signature, length validation, freshness
// per call).

// freshRecipientPubkey returns a real X25519 pubkey by generating an
// ephemeral keypair via crypto/ecdh. An all-zero buffer would fail
// hpke's small-order-point check; a real pubkey exercises the same
// code path production hits.
func freshRecipientPubkey(t *testing.T) []byte {
	t.Helper()
	sk, err := ecdh.X25519().GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("ecdh GenerateKey: %v", err)
	}
	return sk.PublicKey().Bytes()
}

func TestEncryptToRecipientProducesNonEmptyCiphertext(t *testing.T) {
	plaintext := []byte("raw rfc 5322 bytes")
	ct, err := EncryptToRecipient(plaintext, freshRecipientPubkey(t))
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}
	if len(ct) == 0 {
		t.Fatal("ciphertext is empty")
	}
	// The ciphertext bytes are the canonical CBOR encoding of a
	// MailRecordEnvelope ({v, kind, hpke{ks,enc,ct}}). Plaintext
	// should not appear literally — that would mean the seal
	// somehow short-circuited and stored the raw bytes.
	if bytes.Contains(ct, plaintext) {
		t.Fatal("ciphertext contains the literal plaintext — seal short-circuited?")
	}
}

func TestEncryptToRecipientFreshnessPerCall(t *testing.T) {
	// HPKE generates a fresh ephemeral X25519 keypair per call, so
	// identical inputs (same plaintext + same recipient) MUST produce
	// distinct ciphertexts. Pin the pubkey to isolate the freshness
	// property from the per-recipient ciphertext difference.
	pk := freshRecipientPubkey(t)
	a, err := EncryptToRecipient([]byte("x"), pk)
	if err != nil {
		t.Fatalf("first seal: %v", err)
	}
	b, err := EncryptToRecipient([]byte("x"), pk)
	if err != nil {
		t.Fatalf("second seal: %v", err)
	}
	if bytes.Equal(a, b) {
		t.Fatal("two seals with identical inputs produced identical ciphertext — ephemeral key reused?")
	}
}

func TestEncryptToRecipientRejectsWrongLengthPubkey(t *testing.T) {
	// 31 bytes should be rejected as a length-mismatch by the FFI's
	// array_32 helper; the error message includes "32 bytes".
	short := make([]byte, 31)
	_, err := EncryptToRecipient([]byte("x"), short)
	if err == nil {
		t.Fatal("expected error for 31-byte pubkey, got nil")
	}
}

// EncryptToRecipientHybrid is the single seal-suite decision for every MTA/MDA
// seal — body AND index hint (PQ-6): X-Wing iff it is handed a 1184-byte
// ML-KEM ek, else classical (no ek pairs with a dedicated index key). The X-Wing crypto round-trip is pinned
// in Rust (libs/fauna-mls/.../mod.rs::mail_record_xwing_round_trip); these Go
// tests pin the Go-side selection + degrade contract.

func TestEncryptToRecipientHybridNilEkIsClassical(t *testing.T) {
	// No ek passed (the index hint sealed to a dedicated index key, which no
	// ek pairs with) seals as classical — no error,
	// non-empty envelope, plaintext not stored literally.
	plaintext := []byte("canonical index-hint tokens for a dedicated index key")
	ct, err := EncryptToRecipientHybrid(plaintext, freshRecipientPubkey(t), nil)
	if err != nil {
		t.Fatalf("EncryptToRecipientHybrid (nil ek): %v", err)
	}
	if len(ct) == 0 {
		t.Fatal("ciphertext is empty")
	}
	if bytes.Contains(ct, plaintext) {
		t.Fatal("ciphertext contains the literal plaintext — seal short-circuited?")
	}
}

func TestEncryptToRecipientHybridDegradesOnInvalidEk(t *testing.T) {
	// PQ-4(b): a right-length (1184 B) but FIPS-203-invalid ek makes the
	// X-Wing seal error (ek validity is deferred to encaps). EncryptToRecipientHybrid
	// MUST degrade to the classical seal rather than fail closed — failing closed
	// would self-DoS the recipient's inbound mail. An all-0xFF buffer is a
	// guaranteed-invalid ML-KEM-768 ek (every 12-bit coefficient decodes to
	// 4095 >= q=3329), so this deterministically exercises the fallback.
	plaintext := []byte("body that must still get sealed despite a bad ek")
	invalidEk := bytes.Repeat([]byte{0xFF}, 1184)
	ct, err := EncryptToRecipientHybrid(plaintext, freshRecipientPubkey(t), invalidEk)
	if err != nil {
		t.Fatalf("EncryptToRecipientHybrid must degrade to classical on an invalid ek, got error: %v", err)
	}
	if len(ct) == 0 {
		t.Fatal("degraded classical ciphertext is empty")
	}
	if bytes.Contains(ct, plaintext) {
		t.Fatal("ciphertext contains the literal plaintext — seal short-circuited?")
	}
}

// TestIndexHintMlkemEkGate pins the PQ-6 Phase-E gate that decides whether the
// index-hint seal goes hybrid: the body's ek is used for the hint ONLY while the
// index key is the MLS-pubkey fallback (so the ek pairs with the key the hint is
// sealed to); a dedicated index key (≠ MLS pubkey, Phase E) withholds it so the
// hint stays classical until Phase E publishes a dedicated index ek.
func TestIndexHintMlkemEkGate(t *testing.T) {
	mlsPubkey := bytes.Repeat([]byte{0xAA}, 32)
	ek := bytes.Repeat([]byte{0x11}, 1184)

	// Fallback: indexPubkey == mlsPubkey → the body ek pairs with it → hybrid hint.
	if got := IndexHintMlkemEk(mlsPubkey, mlsPubkey, ek); !bytes.Equal(got, ek) {
		t.Errorf("fallback (indexPubkey == mlsPubkey) must return the body ek, got len=%d", len(got))
	}
	// Dedicated index key (Phase E): ek does not pair with it → classical hint.
	dedicated := bytes.Repeat([]byte{0xBB}, 32)
	if got := IndexHintMlkemEk(dedicated, mlsPubkey, ek); got != nil {
		t.Errorf("a dedicated index key must withhold the ek (classical until Phase E), got len=%d", len(got))
	}
	// No published ek → nil regardless of key pairing.
	if got := IndexHintMlkemEk(mlsPubkey, mlsPubkey, nil); got != nil {
		t.Errorf("no published ek must yield nil, got len=%d", len(got))
	}
}
