package main

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// These run the seal halves of the helper end-to-end against the FFI
// *unseal* counterparts in the same process — no nest, no driver, fast.
// They are the de-risking step for the e2e submission test: if the Go
// canonical-CBOR encoding (internal/dagcbor) drifted from the Rust
// `to_canonical_bytes()` encoder, the submission-token signature would
// fail to verify here (loudly, in milliseconds) rather than deep inside
// a multi-minute pytest run.

func TestSealTLSCertRoundTrip(t *testing.T) {
	kp := fauna_ffi.GenerateX25519Keypair()
	certPEM := []byte("-----BEGIN CERTIFICATE-----\nfake-cert\n-----END CERTIFICATE-----")
	keyPEM := []byte("-----BEGIN PRIVATE KEY-----\nfake-key\n-----END PRIVATE KEY-----")

	blob, err := sealTLSCert(certPEM, keyPEM, "mta", "bridge-1", "example.com", 1_700_000_000, 1_700_000_000+90*86_400, kp.Pubkey)
	if err != nil {
		t.Fatalf("sealTLSCert: %v", err)
	}
	got, err := fauna_ffi.UnsealTlsCertBlob(blob, kp.Secret)
	if err != nil {
		t.Fatalf("UnsealTlsCertBlob: %v", err)
	}
	if !bytes.Equal(got.CertChain, certPEM) {
		t.Errorf("cert_chain mismatch: got %q", got.CertChain)
	}
	if !bytes.Equal(got.PrivKey, keyPEM) {
		t.Errorf("priv_key mismatch")
	}
	if got.IssuedAt != 1_700_000_000 {
		t.Errorf("issued_at: got %d", got.IssuedAt)
	}
}

// TestSealSubmissionTokenRoundTrip is the load-bearing check: the bridge's
// UnsealSubmissionTokenBlob AEAD-opens the blob *and* verifies the inner
// Ed25519 signature against actor_id-as-verifying-key. A pass proves the
// Go-side canonical encoding + signature match what the Rust verify path
// re-encodes.
func TestSealSubmissionTokenRoundTrip(t *testing.T) {
	seed := make([]byte, ed25519.SeedSize)
	if _, err := rand.Read(seed); err != nil {
		t.Fatalf("rand: %v", err)
	}
	actorID := []byte(ed25519.NewKeyFromSeed(seed).Public().(ed25519.PublicKey))
	credential := []byte("correct horse battery staple")

	blob, err := sealSubmissionToken(seed, "default", 1_700_000_000, 1_700_000_000+86_400, 100, 1000, "plain", credential)
	if err != nil {
		t.Fatalf("sealSubmissionToken: %v", err)
	}

	tok, err := fauna_ffi.UnsealSubmissionTokenBlob(blob, credential, actorID, "default", fauna_ffi.KdfKindArgon2id)
	if err != nil {
		t.Fatalf("UnsealSubmissionTokenBlob (AEAD + inner-sig verify): %v", err)
	}
	if !bytes.Equal(tok.ActorId, actorID) {
		t.Errorf("actor_id mismatch")
	}
	if tok.CredentialId != "default" {
		t.Errorf("credential_id: got %q", tok.CredentialId)
	}
	if tok.MaxRecipients != 100 || tok.MaxMessagesPerDay != 1000 {
		t.Errorf("quota mismatch: max_recipients=%d max_messages_per_day=%d", tok.MaxRecipients, tok.MaxMessagesPerDay)
	}
}

// TestSealWrappedMsekRoundTrip is the load-bearing check for MDA MUA-AUTH:
// the blob the seal-helper produces must be openable by
// the bridge's exact auth path. We assert both the auth-agnostic MSEK recovery
// AND that UnwrapMsekBlob — the function imap/auth.go + caldav/auth.go call on
// PLAIN AUTH — accepts it under KdfKindArgon2id with the AAD rebuilt from
// (actor_id, credential_id).
func TestSealWrappedMsekRoundTrip(t *testing.T) {
	msek := make([]byte, 32)
	if _, err := rand.Read(msek); err != nil {
		t.Fatalf("rand: %v", err)
	}
	actorID := make([]byte, 32)
	if _, err := rand.Read(actorID); err != nil {
		t.Fatalf("rand: %v", err)
	}
	credential := []byte("mda-test-password-1")

	blob, err := sealWrappedMsek(msek, actorID, "default", "plain", credential)
	if err != nil {
		t.Fatalf("sealWrappedMsek: %v", err)
	}

	// Raw MSEK recovery (auth-agnostic unseal).
	got, err := fauna_ffi.UnsealWrappedMsekBlob(blob, "plain", credential)
	if err != nil {
		t.Fatalf("UnsealWrappedMsekBlob: %v", err)
	}
	if !bytes.Equal(got, msek) {
		t.Errorf("recovered MSEK mismatch")
	}

	// The bridge's exact MUA-AUTH path: PLAIN → Argon2id, AAD from
	// (actor_id, credential_id). A non-error proves the seal-helper blob is
	// openable by the live IMAP/CalDAV auth path.
	if _, err := fauna_ffi.UnwrapMsekBlob(blob, credential, actorID, "default", fauna_ffi.KdfKindArgon2id); err != nil {
		t.Fatalf("UnwrapMsekBlob (bridge MUA-AUTH path): %v", err)
	}
}

// Wrong credential must fail AEAD on the bridge's unwrap path (the auth-fail signal).
func TestSealWrappedMsekWrongCredentialFails(t *testing.T) {
	msek := make([]byte, 32)
	if _, err := rand.Read(msek); err != nil {
		t.Fatalf("rand: %v", err)
	}
	actorID := make([]byte, 32)
	if _, err := rand.Read(actorID); err != nil {
		t.Fatalf("rand: %v", err)
	}
	blob, err := sealWrappedMsek(msek, actorID, "default", "plain", []byte("right-password"))
	if err != nil {
		t.Fatalf("sealWrappedMsek: %v", err)
	}
	if _, err := fauna_ffi.UnwrapMsekBlob(blob, []byte("wrong-password"), actorID, "default", fauna_ffi.KdfKindArgon2id); err == nil {
		t.Fatalf("unwrap with wrong credential unexpectedly succeeded")
	}
}

// TestSealMlsSnapshotBodyDecryptRoundTrip is the load-bearing check for the
// MDA body-decrypt e2e: it pins the
// MSEK-derived-pubkey ↔ snapshot-leaf-secret consistency the fixture relies
// on, in-process and in milliseconds, so a future drift fails here loudly
// rather than deep inside a multi-minute pytest run.
//
// It walks the exact production decrypt path:
//
//  1. derive-recipient-pubkey: the value the fixture registers via
//     `provision_recipient_mls_pubkey` (MTA/APPEND/CalDAV-PUT seal target).
//  2. SealToRecipient: the seal the MTA inbound path + IMAP APPEND + CalDAV
//     PUT all perform against that pubkey.
//  3. seal-mls-snapshot: the blob the fixture provisions via
//     `provision_mls_snapshot_blob`; its leaf keypair is the SAME MSEK-derived
//     one, so its secret half opens the step-2 envelope.
//  4. MUA-AUTH: UnwrapMsekBlob → cap (the bridge's finishAuth capability),
//     cap.Decrypt(snapshot) → plaintext (finishAuth's snapshot unwrap),
//     NewMailRecordOpener(snapshotPlaintext).Open(envelope) → recovered body
//     (the fetch.go / caldav report.go body-open call site, via the session's
//     per-connection MailRecordOpener).
func TestSealMlsSnapshotBodyDecryptRoundTrip(t *testing.T) {
	msek := make([]byte, 32)
	if _, err := rand.Read(msek); err != nil {
		t.Fatalf("rand: %v", err)
	}
	actorID := make([]byte, 32)
	if _, err := rand.Read(actorID); err != nil {
		t.Fatalf("rand: %v", err)
	}
	credential := []byte("mda-test-password-1")

	// 1. The pubkey the fixture registers as the recipient MLS pubkey.
	pubkey, err := deriveRecipientPubkey(msek)
	if err != nil {
		t.Fatalf("deriveRecipientPubkey: %v", err)
	}

	// 2. A representative body sealed to that pubkey (the MTA/APPEND seal).
	body := []byte("From: alice@example.com\r\n" +
		"To: mda-recipient@mda.fauna.test\r\n" +
		"Subject: snapshot decrypt round-trip\r\n" +
		"\r\n" +
		"Body the MDA must recover from the seal via the snapshot leaf secret.\r\n")
	envelope, err := fauna_ffi.SealToRecipient(body, pubkey)
	if err != nil {
		t.Fatalf("SealToRecipient: %v", err)
	}

	// 3. The sealed snapshot the fixture provisions.
	snapBlob, err := sealMlsSnapshot(msek, actorID)
	if err != nil {
		t.Fatalf("sealMlsSnapshot: %v", err)
	}

	// 4. MUA-AUTH capability + the two-stage open the production path runs.
	wrapped, err := sealWrappedMsek(msek, actorID, "default", "plain", credential)
	if err != nil {
		t.Fatalf("sealWrappedMsek: %v", err)
	}
	cap, err := fauna_ffi.UnwrapMsekBlob(wrapped, credential, actorID, "default", fauna_ffi.KdfKindArgon2id)
	if err != nil {
		t.Fatalf("UnwrapMsekBlob: %v", err)
	}
	defer cap.Zeroize()

	snapPlaintext, err := cap.Decrypt(snapBlob)
	if err != nil {
		t.Fatalf("cap.Decrypt(snapshot): %v", err)
	}
	// The production body-open path is the per-connection MailRecordOpener built
	// from the snapshot plaintext (the method→free-fn refactor: the old
	// cap.OpenMailRecord became NewMailRecordOpener(...).Open, opening with the
	// snapshot's leaf secret internally).
	opener, err := fauna_ffi.NewMailRecordOpener(snapPlaintext)
	if err != nil {
		t.Fatalf("NewMailRecordOpener: %v", err)
	}
	defer opener.Zeroize()
	got, err := opener.Open(envelope)
	if err != nil {
		t.Fatalf("opener.Open: %v", err)
	}
	if !bytes.Equal(got, body) {
		t.Errorf("recovered body mismatch:\n want %q\n  got %q", body, got)
	}
}

// A snapshot sealed under a DIFFERENT MSEK must NOT open a body sealed to the
// first MSEK's derived pubkey — proves the leaf secret, not some ambient
// state, is what opens the envelope (and that a mismatched fixture would fail
// rather than silently pass).
func TestSealMlsSnapshotWrongMsekFails(t *testing.T) {
	msekA := make([]byte, 32)
	msekB := make([]byte, 32)
	actorID := make([]byte, 32)
	for _, b := range [][]byte{msekA, msekB, actorID} {
		if _, err := rand.Read(b); err != nil {
			t.Fatalf("rand: %v", err)
		}
	}
	credential := []byte("mda-test-password-1")

	pubkeyA, err := deriveRecipientPubkey(msekA)
	if err != nil {
		t.Fatalf("deriveRecipientPubkey: %v", err)
	}
	envelope, err := fauna_ffi.SealToRecipient([]byte("secret body"), pubkeyA)
	if err != nil {
		t.Fatalf("SealToRecipient: %v", err)
	}

	// Snapshot sealed under msekB; its leaf secret derives from msekB and
	// cannot open an envelope sealed to msekA's pubkey.
	snapBlobB, err := sealMlsSnapshot(msekB, actorID)
	if err != nil {
		t.Fatalf("sealMlsSnapshot: %v", err)
	}
	wrappedB, err := sealWrappedMsek(msekB, actorID, "default", "plain", credential)
	if err != nil {
		t.Fatalf("sealWrappedMsek: %v", err)
	}
	cap, err := fauna_ffi.UnwrapMsekBlob(wrappedB, credential, actorID, "default", fauna_ffi.KdfKindArgon2id)
	if err != nil {
		t.Fatalf("UnwrapMsekBlob: %v", err)
	}
	defer cap.Zeroize()
	snapPlaintextB, err := cap.Decrypt(snapBlobB)
	if err != nil {
		t.Fatalf("cap.Decrypt(snapshot): %v", err)
	}
	openerB, err := fauna_ffi.NewMailRecordOpener(snapPlaintextB)
	if err != nil {
		t.Fatalf("NewMailRecordOpener: %v", err)
	}
	defer openerB.Zeroize()
	if _, err := openerB.Open(envelope); err == nil {
		t.Fatalf("opener.Open unexpectedly succeeded with a wrong-MSEK snapshot")
	}
}

// Once a recipient publishes its ML-KEM ek (what `_publish_recipient_ek` does to
// the session-shared inbound recipient), the MTA seals every later inbound
// message X-Wing (`mailfauna.EncryptToRecipientHybrid`). The fixture's snapshot
// must open that too, as the production `build_mls_snapshot_plaintext` does by
// carrying each MSEK's ML-KEM decaps half. A classical-only snapshot leaves the
// MDA with `HPKE open failed: no matching leaf keypair or epoch key in
// snapshot` for every message after the ek lands — which is what reddened the
// spam scorers that run after the hybrid re-score drain in whole-suite order.
func TestSealMlsSnapshotOpensHybridInbound(t *testing.T) {
	msek := make([]byte, 32)
	if _, err := rand.Read(msek); err != nil {
		t.Fatalf("rand: %v", err)
	}
	actorID := make([]byte, 32)
	if _, err := rand.Read(actorID); err != nil {
		t.Fatalf("rand: %v", err)
	}
	credential := []byte("mda-test-password-1")

	pubkey, err := deriveRecipientPubkey(msek)
	if err != nil {
		t.Fatalf("deriveRecipientPubkey: %v", err)
	}
	ek, err := deriveRecipientMlkemEk(msek)
	if err != nil {
		t.Fatalf("deriveRecipientMlkemEk: %v", err)
	}
	body := []byte("From: sender@external.test\r\n" +
		"To: inbound-recipient@mda.fauna.test\r\n" +
		"Subject: hybrid inbound after the ek is published\r\n" +
		"\r\n" +
		"Body the MDA must open with the provisioned snapshot.\r\n")
	envelope, err := mailfauna.EncryptToRecipientHybrid(body, pubkey, ek)
	if err != nil {
		t.Fatalf("EncryptToRecipientHybrid: %v", err)
	}
	// The seal really is hybrid: the classical secret alone cannot open it.
	classical, err := fauna_ffi.DeriveRecipientHpkeKeypair(msek)
	if err != nil {
		t.Fatalf("DeriveRecipientHpkeKeypair: %v", err)
	}
	if _, err := fauna_ffi.OpenMailRecordWithKey(envelope, classical.Secret); err == nil {
		t.Fatalf("classical key opened the record — it was not sealed X-Wing, so this test proves nothing")
	}

	snapBlob, err := sealMlsSnapshot(msek, actorID)
	if err != nil {
		t.Fatalf("sealMlsSnapshot: %v", err)
	}
	wrapped, err := sealWrappedMsek(msek, actorID, "default", "plain", credential)
	if err != nil {
		t.Fatalf("sealWrappedMsek: %v", err)
	}
	cap, err := fauna_ffi.UnwrapMsekBlob(wrapped, credential, actorID, "default", fauna_ffi.KdfKindArgon2id)
	if err != nil {
		t.Fatalf("UnwrapMsekBlob: %v", err)
	}
	defer cap.Zeroize()
	snapPlaintext, err := cap.Decrypt(snapBlob)
	if err != nil {
		t.Fatalf("cap.Decrypt(snapshot): %v", err)
	}
	opener, err := fauna_ffi.NewMailRecordOpener(snapPlaintext)
	if err != nil {
		t.Fatalf("NewMailRecordOpener: %v", err)
	}
	defer opener.Zeroize()
	got, err := opener.Open(envelope)
	if err != nil {
		t.Fatalf("the provisioned snapshot cannot open X-Wing inbound mail: %v", err)
	}
	if !bytes.Equal(got, body) {
		t.Errorf("recovered body mismatch:\n want %q\n  got %q", body, got)
	}
}

// Wrong credential must fail AEAD (the auth-failure signal).
func TestSealSubmissionTokenWrongCredentialFails(t *testing.T) {
	seed := make([]byte, ed25519.SeedSize)
	if _, err := rand.Read(seed); err != nil {
		t.Fatalf("rand: %v", err)
	}
	actorID := []byte(ed25519.NewKeyFromSeed(seed).Public().(ed25519.PublicKey))

	blob, err := sealSubmissionToken(seed, "default", 1_700_000_000, 1_700_000_000+86_400, 100, 1000, "plain", []byte("right"))
	if err != nil {
		t.Fatalf("sealSubmissionToken: %v", err)
	}
	if _, err := fauna_ffi.UnsealSubmissionTokenBlob(blob, []byte("wrong"), actorID, "default", fauna_ffi.KdfKindArgon2id); err == nil {
		t.Fatalf("unseal with wrong credential unexpectedly succeeded")
	}
}

// TestSealSpamModelRoundTrip pins the tier-1 spam-model seed the agent-side
// `\Junk`-train tier_3 relies on: a plaintext
// SpamModel sealed to the recipient's MSEK-derived pubkey must open with the MDA
// session's snapshot leaf secret via its MailRecordOpener — the EXACT open
// `imap/store.go` trainJunkAgentSide runs (`store.go:421`) before
// ApplySpamTraining. If the seal shape drifts from what the MDA opens, this fails
// here in milliseconds rather than as a silent agent-side no-op deep inside a
// multi-minute pytest run. It walks the same MSEK-derived-pubkey ↔
// snapshot-leaf-secret path as TestSealMlsSnapshotBodyDecryptRoundTrip, but for a
// sealed spam-model blob rather than a mail body (they ride the identical seal).
func TestSealSpamModelRoundTrip(t *testing.T) {
	msek := make([]byte, 32)
	if _, err := rand.Read(msek); err != nil {
		t.Fatalf("rand: %v", err)
	}
	actorID := make([]byte, 32)
	if _, err := rand.Read(actorID); err != nil {
		t.Fatalf("rand: %v", err)
	}
	credential := []byte("mda-test-password-1")

	// A representative plaintext SpamModel (the shape _seed_spam_model writes,
	// and apply_spam_training mutates); its exact contents are opaque here.
	modelJSON := []byte(`{"version":1,"ngrams":{},"spam_messages":0,"ham_messages":0}`)
	sealed, err := sealSpamModel(msek, modelJSON)
	if err != nil {
		t.Fatalf("sealSpamModel: %v", err)
	}

	// The nest's is_sealed_model_blob (non-empty + not serde_json) must see this
	// as sealed — the sealed bytes are CBOR+AEAD, never valid model JSON.
	if json.Valid(sealed) {
		t.Errorf("sealed model must NOT be valid JSON (would read as plaintext to the nest)")
	}

	// The MDA session's exact model open at AUTH + train time (store.go:421):
	// UnwrapMsekBlob → cap.Decrypt(snapshot) → NewMailRecordOpener(snapshot).Open(sealedModel).
	snapBlob, err := sealMlsSnapshot(msek, actorID)
	if err != nil {
		t.Fatalf("sealMlsSnapshot: %v", err)
	}
	wrapped, err := sealWrappedMsek(msek, actorID, "default", "plain", credential)
	if err != nil {
		t.Fatalf("sealWrappedMsek: %v", err)
	}
	cap, err := fauna_ffi.UnwrapMsekBlob(wrapped, credential, actorID, "default", fauna_ffi.KdfKindArgon2id)
	if err != nil {
		t.Fatalf("UnwrapMsekBlob: %v", err)
	}
	defer cap.Zeroize()
	snapPlaintext, err := cap.Decrypt(snapBlob)
	if err != nil {
		t.Fatalf("cap.Decrypt(snapshot): %v", err)
	}
	opener, err := fauna_ffi.NewMailRecordOpener(snapPlaintext)
	if err != nil {
		t.Fatalf("NewMailRecordOpener: %v", err)
	}
	defer opener.Zeroize()
	got, err := opener.Open(sealed)
	if err != nil {
		t.Fatalf("opener.Open(sealed model): %v", err)
	}
	if !bytes.Equal(got, modelJSON) {
		t.Errorf("recovered model mismatch:\n want %q\n  got %q", modelJSON, got)
	}
}

// TestSealMailRecordRoundTrip proves `seal-mail-record` emits a genuine
// recipient envelope: the recipient's own X25519 secret opens it back to the
// exact plaintext — the property the nest's `SealedRecordBytes::verify` gate
// and every MDA/client open rely on.
func TestSealMailRecordRoundTrip(t *testing.T) {
	kp := fauna_ffi.GenerateX25519Keypair()
	plaintext := []byte("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
	sealed, err := sealMailRecord(plaintext, kp.Pubkey)
	if err != nil {
		t.Fatalf("sealMailRecord: %v", err)
	}
	if bytes.Contains(sealed, plaintext) {
		t.Fatalf("the envelope carries the plaintext in the clear")
	}
	got, err := fauna_ffi.OpenMailRecordWithKey(sealed, kp.Secret)
	if err != nil {
		t.Fatalf("OpenMailRecordWithKey: %v", err)
	}
	if !bytes.Equal(got, plaintext) {
		t.Errorf("recovered plaintext mismatch:\n want %q\n  got %q", plaintext, got)
	}
}

// TestMintGrantRoundTrip proves the seal-helper's `mint-grant` mode builds a
// capability GrantBlob the MDA capability holder unseals — the content.read{mail}
// key it recovers is the SAME MSEK-derived recipient-mail secret that opens a
// sealed mail record, and the keyless content.label-write scope is declared but
// carries no wrapped key. This is the tier_3 re-score-drain test's mint
// precondition, de-risked in-process (design § Phase 2 Step 2 § 2.1).
func TestMintGrantRoundTrip(t *testing.T) {
	holder := fauna_ffi.GenerateX25519Keypair()

	msek := make([]byte, 32)
	if _, err := rand.Read(msek); err != nil {
		t.Fatalf("rand msek: %v", err)
	}
	owner := make([]byte, 32)
	if _, err := rand.Read(owner); err != nil {
		t.Fatalf("rand owner: %v", err)
	}
	grantID := make([]byte, 16)
	if _, err := rand.Read(grantID); err != nil {
		t.Fatalf("rand grant_id: %v", err)
	}

	// nil holder ek ⇒ the classical X25519 wrap; the mail payload is the 32+2400
	// superset either way (the X-Wing variant is TestMintGrantXwingRoundTrip).
	blob, err := mintGrant(owner, grantID, holder.Pubkey, msek, nil, 1_700_000_000, 1_700_000_000+90*86_400, nil)
	if err != nil {
		t.Fatalf("mintGrant: %v", err)
	}

	grant, err := fauna_ffi.UnsealCapabilityGrant(blob, holder.Secret, nil)
	if err != nil {
		t.Fatalf("UnsealCapabilityGrant: %v", err)
	}
	if !bytes.Equal(grant.OwnerActorId, owner) {
		t.Errorf("owner mismatch: got %x", grant.OwnerActorId)
	}
	if !bytes.Equal(grant.GrantId, grantID) {
		t.Errorf("grant_id mismatch: got %x", grant.GrantId)
	}
	if grant.EpochStart != 1_700_000_000 || grant.EpochEnd != 1_700_000_000+90*86_400 {
		t.Errorf("window mismatch: [%d, %d]", grant.EpochStart, grant.EpochEnd)
	}

	// Only the key-bearing content.read{mail} tuple yields an unsealed key; the
	// keyless content.label-write tuple carries none.
	if len(grant.Keys) != 1 {
		t.Fatalf("expected 1 unsealed key (mail; label-write is keyless), got %d", len(grant.Keys))
	}
	k := grant.Keys[0]
	if k.Class != "content.read" || k.Kind == nil || *k.Kind != "mail" {
		t.Errorf("scope mismatch: class=%q kind=%v", k.Class, k.Kind)
	}

	// The recovered mail-read key is the `32 + 2400`-byte X-Wing capability secret
	// (x25519_secret ∥ ml-kem-dk) — the SAME superset the production
	// derive_scope_payload wraps, so a holder opens both classical and hybrid mail
	// records with it.
	material, err := fauna_ffi.DeriveRecipientMailXwingMaterial(msek)
	if err != nil {
		t.Fatalf("DeriveRecipientMailXwingMaterial: %v", err)
	}
	if !bytes.Equal(k.Key, material.CapabilitySecret) {
		t.Errorf("mail-read key is not the MSEK-derived X-Wing capability secret (len %d)", len(k.Key))
	}
	// Its first 32 bytes ARE the recipient's classical secret — the superset property
	// that keeps classical mail drainable under the same grant key.
	kp, err := fauna_ffi.DeriveRecipientHpkeKeypair(msek)
	if err != nil {
		t.Fatalf("DeriveRecipientHpkeKeypair: %v", err)
	}
	if len(k.Key) < 32 || !bytes.Equal(k.Key[:32], kp.Secret) {
		t.Errorf("X-Wing capability secret's X25519 half is not the recipient's classical secret")
	}
}

// TestMintSpamModelGrantIsKeyless proves the seal-helper's `mint-spam-model-grant`
// mode builds a capability GrantBlob the holder unseals to ZERO wrapped keys — the
// keyless `content.read{spam-model}` shape (mail-spam.md § Encrypted-mode
// interaction). This is the security-critical property: contributing to the
// deployment spam baseline mints an audit/revocation grant that conveys no standing
// read of the contributor's mailbox — the holder aggregates the contributor's
// `SpamModelCopyBlob` with its OWN key halves, never a wrapped contributor secret.
// The end-to-end negative (a holder bearing only this grant cannot open mail) is
// the tier_3 spam-baseline-drain proof; this de-risks the mint half in-process.
func TestMintSpamModelGrantIsKeyless(t *testing.T) {
	holder := fauna_ffi.GenerateX25519Keypair()

	owner := make([]byte, 32)
	grantID := make([]byte, 16)
	for _, b := range [][]byte{owner, grantID} {
		if _, err := rand.Read(b); err != nil {
			t.Fatalf("rand: %v", err)
		}
	}

	blob, err := mintSpamModelGrant(owner, grantID, holder.Pubkey, 1_700_000_000, 1_700_000_000+90*86_400)
	if err != nil {
		t.Fatalf("mintSpamModelGrant: %v", err)
	}

	grant, err := fauna_ffi.UnsealCapabilityGrant(blob, holder.Secret, nil)
	if err != nil {
		t.Fatalf("UnsealCapabilityGrant: %v", err)
	}
	if !bytes.Equal(grant.OwnerActorId, owner) {
		t.Errorf("owner mismatch: got %x", grant.OwnerActorId)
	}
	if !bytes.Equal(grant.GrantId, grantID) {
		t.Errorf("grant_id mismatch: got %x", grant.GrantId)
	}
	if grant.EpochStart != 1_700_000_000 || grant.EpochEnd != 1_700_000_000+90*86_400 {
		t.Errorf("window mismatch: [%d, %d]", grant.EpochStart, grant.EpochEnd)
	}

	// The whole point: NO wrapped key rides a spam-model grant. `content.read{mail}`
	// would yield one key here; `content.read{spam-model}` yields none — so the grant
	// cannot open the contributor's mailbox, only authorize the holder-computed
	// baseline merge over the owner's own `SpamModelCopyBlob`.
	if len(grant.Keys) != 0 {
		t.Fatalf("spam-model grant must be keyless, got %d wrapped key(s)", len(grant.Keys))
	}
}

// TestMintGrantXwingRoundTrip proves the seal-helper's `mint-grant` X-Wing mode
// (PQ-CAP-4): passing the holder's published 1184-B ML-KEM ek makes the grant's
// WrappedScopeKey ride the X-Wing suite, and the holder recovers the SAME 32+2400
// content.read{mail} key by opening with its X25519 secret + its ML-KEM
// decapsulation key. Classical-only open (nil dk) MUST fail — the wrap is genuinely
// hybrid, so a harvested grant row is not a CRQC-openable bypass of the closed mail
// seal. In-process de-risk of the tier_3 hybrid-mail-drain harness's mint half
// (design § Phase 2; `post-quantum.md` § surface-A capability-grant note).
func TestMintGrantXwingRoundTrip(t *testing.T) {
	holderX := fauna_ffi.GenerateX25519Keypair()
	// The holder's ML-KEM half derives from its keyfile Ed25519 seed (PQ-CAP-2),
	// independent of the X25519 wrap-target scalar.
	holderSeed := make([]byte, ed25519.SeedSize)
	if _, err := rand.Read(holderSeed); err != nil {
		t.Fatalf("rand holder seed: %v", err)
	}
	holderMlkem, err := fauna_ffi.DeriveBridgeServiceUserMlkem768(holderSeed)
	if err != nil {
		t.Fatalf("DeriveBridgeServiceUserMlkem768: %v", err)
	}

	msek := make([]byte, 32)
	owner := make([]byte, 32)
	grantID := make([]byte, 16)
	for _, b := range [][]byte{msek, owner, grantID} {
		if _, err := rand.Read(b); err != nil {
			t.Fatalf("rand: %v", err)
		}
	}

	blob, err := mintGrant(owner, grantID, holderX.Pubkey, msek, holderMlkem.MlkemEk, 1_700_000_000, 1_700_000_000+90*86_400, nil)
	if err != nil {
		t.Fatalf("mintGrant (X-Wing): %v", err)
	}

	// Classical-only open (nil dk) must fail: the wrap is genuinely X-Wing.
	if _, err := fauna_ffi.UnsealCapabilityGrant(blob, holderX.Secret, nil); err == nil {
		t.Fatalf("classical-only open of an X-Wing grant unexpectedly succeeded")
	}

	// Hybrid open with the holder's ML-KEM dk recovers the superset key.
	dk := holderMlkem.MlkemDk
	grant, err := fauna_ffi.UnsealCapabilityGrant(blob, holderX.Secret, &dk)
	if err != nil {
		t.Fatalf("UnsealCapabilityGrant (hybrid): %v", err)
	}
	if len(grant.Keys) != 1 {
		t.Fatalf("expected 1 unsealed key (mail; label-write is keyless), got %d", len(grant.Keys))
	}
	material, err := fauna_ffi.DeriveRecipientMailXwingMaterial(msek)
	if err != nil {
		t.Fatalf("DeriveRecipientMailXwingMaterial: %v", err)
	}
	if !bytes.Equal(grant.Keys[0].Key, material.CapabilitySecret) {
		t.Errorf("hybrid-opened mail-read key is not the 32+2400 X-Wing capability secret (len %d)", len(grant.Keys[0].Key))
	}
}

// TestMintGrantXwingDrainsHybridMail proves the closure of the content-at-rest gap
// at the crypto layer: a hybrid (X-Wing) sealed mail
// record IS drainable by a capability holder under an X-Wing grant. It walks the
// exact background-drain open (`rescore_drain.go` openFn = OpenMailRecordWithKey):
//
//  1. Seal a mail record X-Wing to the owner's recipient key (the MTA inbound seal:
//     a recipient's key always carries its ML-KEM ek).
//  2. NEGATIVE CONTROL: the owner's 32-byte classical secret must FAIL to
//     open it — proof the record is GENUINELY X-Wing, not a classical fallback that
//     the 32+2400 superset key would also open (the false-green PQ-4b-degrade would
//     otherwise hide behind the superset).
//  3. Mint an X-Wing grant (holder ek) carrying the owner's 32+2400 mail secret; the
//     holder unseals it with its X25519 secret + ML-KEM dk → recovers that key.
//  4. The recovered key opens the hybrid record → plaintext == the sealed body.
//
// In-process (no nest / MDA process); the tier_3 real-pipeline proof is
// tests/e2e-unified/tests/test_capability_rescore_drain.py's hybrid variant.
func TestMintGrantXwingDrainsHybridMail(t *testing.T) {
	msek := make([]byte, 32)
	if _, err := rand.Read(msek); err != nil {
		t.Fatalf("rand msek: %v", err)
	}
	// The recipient's X-Wing public key = mlkem_ek(1184) ∥ x25519_pubkey(32); the
	// 32+2400 capability secret is its private counterpart — all MSEK-derived.
	recipient, err := fauna_ffi.DeriveRecipientHpkeKeypair(msek)
	if err != nil {
		t.Fatalf("DeriveRecipientHpkeKeypair: %v", err)
	}
	material, err := fauna_ffi.DeriveRecipientMailXwingMaterial(msek)
	if err != nil {
		t.Fatalf("DeriveRecipientMailXwingMaterial: %v", err)
	}
	xwingPubkey := make([]byte, 0, len(material.MlkemEk)+len(recipient.Pubkey))
	xwingPubkey = append(xwingPubkey, material.MlkemEk...)
	xwingPubkey = append(xwingPubkey, recipient.Pubkey...)

	body := []byte("From: sender@external.test\r\n" +
		"To: owner@mda.fauna.test\r\n" +
		"Subject: hybrid drain crypto proof\r\n" +
		"\r\n" +
		"Body the drain must recover from an X-Wing record via the grant key.\r\n")
	sealed, err := fauna_ffi.SealToRecipientXwing(body, xwingPubkey)
	if err != nil {
		t.Fatalf("SealToRecipientXwing: %v", err)
	}

	// NEGATIVE CONTROL: a 32-byte classical key cannot open an X-Wing record (no
	// ML-KEM decaps half), so a pass here proves the record is genuinely hybrid.
	if _, err := fauna_ffi.OpenMailRecordWithKey(sealed, recipient.Secret); err == nil {
		t.Fatalf("classical 32-byte key unexpectedly opened an X-Wing-sealed record — record is not genuinely hybrid")
	}

	// Mint an X-Wing grant to a holder and have it recover the 32+2400 mail key.
	holderX := fauna_ffi.GenerateX25519Keypair()
	holderSeed := make([]byte, ed25519.SeedSize)
	if _, err := rand.Read(holderSeed); err != nil {
		t.Fatalf("rand holder seed: %v", err)
	}
	holderMlkem, err := fauna_ffi.DeriveBridgeServiceUserMlkem768(holderSeed)
	if err != nil {
		t.Fatalf("DeriveBridgeServiceUserMlkem768: %v", err)
	}
	owner := make([]byte, 32)
	grantID := make([]byte, 16)
	for _, b := range [][]byte{owner, grantID} {
		if _, err := rand.Read(b); err != nil {
			t.Fatalf("rand: %v", err)
		}
	}
	blob, err := mintGrant(owner, grantID, holderX.Pubkey, msek, holderMlkem.MlkemEk, 1_700_000_000, 1_700_000_000+90*86_400, nil)
	if err != nil {
		t.Fatalf("mintGrant (X-Wing): %v", err)
	}
	dk := holderMlkem.MlkemDk
	grant, err := fauna_ffi.UnsealCapabilityGrant(blob, holderX.Secret, &dk)
	if err != nil {
		t.Fatalf("UnsealCapabilityGrant (hybrid): %v", err)
	}
	if len(grant.Keys) != 1 {
		t.Fatalf("expected 1 unsealed key (mail), got %d", len(grant.Keys))
	}

	// The drain's exact open: the recovered 32+2400 key opens the X-Wing record.
	got, err := fauna_ffi.OpenMailRecordWithKey(sealed, grant.Keys[0].Key)
	if err != nil {
		t.Fatalf("OpenMailRecordWithKey under the grant key: %v", err)
	}
	if !bytes.Equal(got, body) {
		t.Errorf("drained hybrid body mismatch:\n want %q\n  got %q", body, got)
	}
}

// TestPublishLabelerRoundTrip proves the seal-helper's `publish-labeler` mode
// builds an AlgorithmLabeler metadata_blob the holder accepts: RunWasmLabelerScore
// re-verifies the signature + wasm_hash (security review B1) before running, so a
// non-error proves the blob passes the exact `verify_labeler_metadata` the nest
// publish gate uses — de-risking the tier_3 `test_capability_labeler_drain`
// publish precondition in-process (design § 5). The 10-positional-arg wrapper is
// the drift-prone surface; a wrong order would produce a blob that fails verify.
func TestPublishLabelerRoundTrip(t *testing.T) {
	seed := make([]byte, ed25519.SeedSize)
	if _, err := rand.Read(seed); err != nil {
		t.Fatalf("rand: %v", err)
	}
	// A minimal wasmi-parseable module speaking the label() ABI: writes a
	// length-0 output prefix and returns that pointer (empty Vec<Label> → 0).
	wasm := []byte(`(module
  (memory (export "memory") 1)
  (global $top (mut i32) (i32.const 1024))
  (func (export "alloc") (param $n i32) (result i32)
    (local $p i32) (local.set $p (global.get $top))
    (global.set $top (i32.add (global.get $top) (local.get $n))) (local.get $p))
  (func (export "label") (param i32 i32) (result i32)
    (i32.store (i32.const 100) (i32.const 0)) (i32.const 100)))`)

	blob, err := publishLabeler(seed, wasm, 1, true, false, false, false, 16*1024*1024, 100_000, 1_700_000_000)
	if err != nil {
		t.Fatalf("publishLabeler: %v", err)
	}
	if len(blob) == 0 {
		t.Fatalf("publishLabeler produced an empty metadata_blob")
	}

	// The holder's re-verify-then-run path: a non-error proves the blob's
	// signature + wasm_hash verify against algorithm_id (== seed's verify key),
	// and that the id the drain hands over — the one its factor names — is
	// that same key.
	labelerID := []byte(ed25519.NewKeyFromSeed(seed).Public().(ed25519.PublicKey))
	score, err := fauna_ffi.RunWasmLabelerScore(blob, wasm, labelerID, []byte{})
	if err != nil {
		t.Fatalf("RunWasmLabelerScore (verify + run): %v", err)
	}
	if score != 0 {
		t.Errorf("empty-output module must score 0, got %d", score)
	}
	// Asked for another labeler, the same self-consistent blob runs nothing
	// (the expected-id check: a store cannot answer inspect(A) with B's
	// module).
	otherID := make([]byte, 32)
	otherID[0] = 0x42
	if _, err := fauna_ffi.RunWasmLabelerScore(blob, wasm, otherID, []byte{}); err == nil {
		t.Fatalf("a module for another labeler must be refused by the expected-id check")
	}

	// The blob is signed over the ORIGINAL bytes: a swapped module (same length)
	// must be rejected by the wasm_hash bind (B1), proving the blob is genuinely
	// bound to this module and not degenerate.
	swapped := append([]byte{}, wasm...)
	swapped[len(swapped)/2] ^= 0xFF
	if _, err := fauna_ffi.RunWasmLabelerScore(blob, swapped, labelerID, []byte{}); err == nil {
		t.Fatalf("RunWasmLabelerScore accepted a module the metadata was not signed over")
	}
}

// TestMintBoundedGrantEpochFieldRoundTrip closes a gap: a REAL
// bounded (epoch-wrapped) grant blob driven through the generated Go unseal,
// pinning the `UnsealedScopeKey.Epoch` lift end-to-end. Before this, every
// Go-side grant was master-key-shaped, so a codegen regression that dropped
// or zeroed Epoch — turning a bounded grant into a master-key masquerade,
// the FAIL-OPEN direction — was invisible to every Go test.
func TestMintBoundedGrantEpochFieldRoundTrip(t *testing.T) {
	const week = 7 * 24 * 60 * 60
	holder := fauna_ffi.GenerateX25519Keypair()
	msek := make([]byte, 32)
	owner := make([]byte, 32)
	grantID := make([]byte, 16)
	for _, b := range [][]byte{msek, owner, grantID} {
		if _, err := rand.Read(b); err != nil {
			t.Fatalf("rand: %v", err)
		}
	}

	// A 3-epoch window: [100*week, 102*week + 10] intersects epochs 100..102.
	windowStart := uint64(100 * week)
	windowEnd := uint64(102*week + 10)
	blob, err := mintBoundedGrant(owner, grantID, holder.Pubkey, msek, nil, windowStart, windowEnd, nil)
	if err != nil {
		t.Fatalf("mintBoundedGrant: %v", err)
	}

	grant, err := fauna_ffi.UnsealCapabilityGrant(blob, holder.Secret, nil)
	if err != nil {
		t.Fatalf("UnsealCapabilityGrant: %v", err)
	}
	if grant.EpochStart != windowStart || grant.EpochEnd != windowEnd {
		t.Errorf("window mismatch: [%d, %d]", grant.EpochStart, grant.EpochEnd)
	}
	if len(grant.Keys) != 3 {
		t.Fatalf("expected 3 per-epoch keys (epochs 100..102; label-write keyless), got %d", len(grant.Keys))
	}
	seen := map[uint64]bool{}
	for i, k := range grant.Keys {
		if k.Class != "content.read" || k.Kind == nil || *k.Kind != "mail" {
			t.Errorf("key[%d] scope mismatch: class=%q kind=%v", i, k.Class, k.Kind)
		}
		// The regression pin: the epoch index survives the wire + the generated
		// Go lift. A nil Epoch here is the bounded→master masquerade.
		if k.Epoch == nil {
			t.Fatalf("key[%d].Epoch is nil — a bounded wrap lifted as master-key (fail-open)", i)
		}
		e := *k.Epoch
		if e < 100 || e > 102 {
			t.Fatalf("key[%d].Epoch = %d, want 100..102", i, e)
		}
		if seen[e] {
			t.Fatalf("duplicate epoch %d", e)
		}
		seen[e] = true
		// Each payload is that epoch's 32+2400 capability secret; its X25519
		// half is exactly the per-epoch classical secret.
		if len(k.Key) != 32+2400 {
			t.Fatalf("key[%d] payload length = %d, want 2432", i, len(k.Key))
		}
		kp, err := fauna_ffi.DeriveRecipientEpochHpkeKeypair(msek, e)
		if err != nil {
			t.Fatalf("DeriveRecipientEpochHpkeKeypair: %v", err)
		}
		if !bytes.Equal(k.Key[:32], kp.Secret) {
			t.Errorf("key[%d] X25519 half is not epoch %d's derived secret", i, e)
		}
		// The bounded mint must NEVER include the standing secret (the
		// bounded-XOR-master-key policy): no payload equals the standing key.
		material, err := fauna_ffi.DeriveRecipientMailXwingMaterial(msek)
		if err != nil {
			t.Fatalf("DeriveRecipientMailXwingMaterial: %v", err)
		}
		if bytes.Equal(k.Key, material.CapabilitySecret) {
			t.Fatalf("key[%d] wraps the STANDING secret — bounded mint policy violated", i)
		}
	}
}
