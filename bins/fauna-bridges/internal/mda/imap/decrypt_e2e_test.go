package imap

import (
	"context"
	"fmt"
	"sync"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// TestIMAPBodyDecryptRoundTrip is the IMAP-side mail-record-open
// round-trip pin: the same MTA seal primitive
// (`mailfauna.EncryptToRecipient`) that produces inbound bodies on
// the wire feeds into the MDA's body-decrypt call site (the
// per-connection `mailfauna.MailRecordOpener`); the plaintext must
// match what the MTA originally sealed.
//
// This is the smallest reproducer (the full FETCH BODY[] wire-shape
// test lives in tier-3 `tests/e2e-full/`); it bypasses the imap
// `Session.Fetch` plumbing so a future refactor of that plumbing
// can't silently regress the FFI contract.
//
// Round-trip:
//  1. Generate a fresh X25519 leaf init keypair via FFI.
//  2. Build a canonical-CBOR `MlsSnapshotPlaintext` containing only
//     that keypair (the wire shape the user's primary client
//     provisions via `provision_mls_snapshot_blob`).
//  3. Seal a representative RFC 5322 body to the keypair's pubkey
//     via the same `EncryptToRecipient` the MTA uses.
//  4. Unwrap the wrapped-MSEK fixture so we have a real MlsCapability.
//  5. Build a `MailRecordOpener` from the snapshot plaintext and open
//     the envelope via `OpenStoredRecord`; assert plaintext-equality.
//
// Step 5's primitive history: `MlsCapability.Decrypt` is strictly typed
// to MlsSnapshotBlob but the MTA seals via `EncryptToRecipient`
// (MailRecordEnvelope shape); the mail-record open first landed as
// `MlsCapability.OpenMailRecord` (per-call snapshot passing) and moved
// to the parse-once `MailRecordOpener` in Phase-3 S2 (the F2 fix).
func TestIMAPBodyDecryptRoundTrip(t *testing.T) {
	// Fresh leaf init keypair — what the user's primary client
	// would have generated and provisioned.
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextV1(
		[]faunaFfi.X25519Keypair{leaf},
	)
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}

	// Real MTA-side seal of a representative RFC 5322 body to the
	// leaf pubkey. EncryptToRecipient emits canonical-CBOR
	// MailRecordEnvelope bytes.
	plaintext := []byte("From: alice@example.com\r\n" +
		"To: alice@example.com\r\n" +
		"Subject: TDD body decrypt round-trip\r\n" +
		"Date: Wed, 15 May 2026 12:00:00 +0000\r\n" +
		"\r\n" +
		"Body that the MDA must be able to recover from the seal.\r\n")
	envelope, err := mailfauna.EncryptToRecipient(plaintext, leaf.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}

	// Real wrapped MSEK fixture → real MlsCapability. The cap's
	// inner MSEK is only used for snapshot AEAD-unwrap; the leaf
	// init secret rides on `snapshotPlaintext`, so this test
	// covers the OpenMailRecord path independently of the
	// snapshot-decryption path (which has its own tests).
	blob := mustReadFixture(t, "wrapped_msek.bin")
	cap, err := mailfauna.UnwrapMLSBlob(
		blob,
		fixturePlainPassword,
		fixtureActorID,
		fixtureCredentialID,
		mailfauna.KdfKindArgon2id,
	)
	if err != nil {
		t.Fatalf("UnwrapMLSBlob: %v", err)
	}
	defer cap.Zeroize()

	// Mocked nest serves the envelope verbatim — same shape the
	// MTA stored via ingest_inbound_mail and nest serves back from
	// fetch_message_ciphertext.
	messageID := make([]byte, 32)
	for i := range messageID {
		messageID[i] = 0xA1
	}
	caller := &bodyDecryptCaller{envelope: envelope}

	// Real MDA fetch — same wire path internal/mda/imap/fetch.go
	// uses in production.
	ct, err := wsrpc.FetchMessageCiphertext(context.Background(), caller, fixtureActorID, messageID)
	if err != nil {
		t.Fatalf("FetchMessageCiphertext: %v", err)
	}

	// The FFI mail-record-open primitive, through the production shape:
	// the per-connection `mailfauna.MailRecordOpener` (built once from the
	// snapshot plaintext at AUTH) + the `OpenStoredRecord` serve path
	// (Phase-3 S2; the old per-call snapshot-passing
	// `MlsCapability.OpenMailRecord` is deleted).
	opener, err := mailfauna.NewMailRecordOpener(snapshotPlaintext)
	if err != nil {
		t.Fatalf("NewMailRecordOpener: %v", err)
	}
	defer opener.Zeroize()
	got, err := mailfauna.OpenStoredRecord(opener, ct.EncryptedBody)
	if err != nil {
		t.Fatalf("OpenStoredRecord: %v", err)
	}
	if string(got) != string(plaintext) {
		t.Fatalf("plaintext mismatch\nwant: %q\ngot:  %q", plaintext, got)
	}
}

// bodyDecryptCaller is a minimal wsrpc.Caller fake for
// TestIMAPBodyDecryptRoundTrip. Returns the canned envelope for
// fetch_message_ciphertext; rejects every other method so a
// future test addition doesn't silently rely on unhandled paths.
type bodyDecryptCaller struct {
	mu       sync.Mutex
	envelope []byte
	calls    int
}

func (b *bodyDecryptCaller) Call(_ context.Context, method string, body, reply any) error {
	b.mu.Lock()
	defer b.mu.Unlock()
	if method != wsrpc.MethodFetchMessageCiphertext {
		return fmt.Errorf("bodyDecryptCaller: unexpected method %q", method)
	}
	b.calls++
	r := map[string]any{
		"outcome":         "found",
		"encrypted_body":  b.envelope,
		"ciphertext_size": uint32(len(b.envelope)),
		"internal_date":   int64(1747310400),
	}
	enc, err := cbor.Marshal(r)
	if err != nil {
		return err
	}
	return cbor.Unmarshal(enc, reply)
}

// fixtureActorID, fixtureCredentialID, fixturePlainPassword,
// fixtureMLSPubkey, and mustReadFixture live in `auth_test.go` and
// are re-used here. The shared fixture set keeps the two tier-1
// tests (AUTH happy path + body-decrypt round-trip) honest about
// the same wire shapes.
