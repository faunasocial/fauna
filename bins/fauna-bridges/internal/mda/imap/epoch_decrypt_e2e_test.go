package imap

import (
	"bytes"
	"context"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// TestIMAPBodyDecryptEpochSealedRoundTrip is TestIMAPBodyDecryptRoundTrip's
// epoch-aware sibling — the content-sealing-epochs design § 4 MSEK-holder
// opener chain, proven through the EXACT production construction path:
// auth.go's `mailfauna.NewEpochAwareMailRecordOpener` (not the plain
// `NewMailRecordOpener` davauth/CalDAV uses) and fetch.go's
// `mailfauna.OpenStoredRecordAt` (not `OpenStoredRecord`).
//
// Round-trip:
//  1. Derive the actor's per-epoch recipient-mail keypair for a target
//     epoch from the SAME MSEK the `wrapped_msek.bin` fixture wraps
//     ([0x11u8;32], per libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs)
//     via the new `derive_recipient_epoch_hpke_keypair` FFI export.
//  2. Seal a representative RFC 5322 body to that epoch pubkey — the same
//     `EncryptToRecipient` primitive the MTA uses for standing-key mail,
//     now targeting an epoch key instead.
//  3. Unwrap the wrapped-MSEK fixture into a real MlsCapability.
//  4. Build the opener via `NewEpochAwareMailRecordOpener` from an
//     UNRELATED standing snapshot keypair — proves the epoch trial is what
//     opens this record, not an accidental standing-chain fallthrough.
//  5. Open via `OpenStoredRecordAt`, passing the record's own timestamp
//     (the real FETCH call site's exact shape); assert plaintext-equality.
func TestIMAPBodyDecryptEpochSealedRoundTrip(t *testing.T) {
	const week = 7 * 24 * 60 * 60
	recordTs := uint64(500*week + 10)
	targetEpoch := mailfauna.MailSealingEpochOf(recordTs)

	// The wrapped_msek.bin fixture's known MSEK.
	msek := bytes.Repeat([]byte{0x11}, 32)
	epochKp, err := faunaFfi.DeriveRecipientEpochHpkeKeypair(msek, targetEpoch)
	if err != nil {
		t.Fatalf("DeriveRecipientEpochHpkeKeypair: %v", err)
	}

	plaintext := []byte("From: alice@example.com\r\n" +
		"To: alice@example.com\r\n" +
		"Subject: epoch-sealed body\r\n" +
		"Date: Wed, 15 May 2026 12:00:00 +0000\r\n" +
		"\r\n" +
		"Body sealed under the record's own mail content-sealing epoch.\r\n")
	envelope, err := mailfauna.EncryptToRecipient(plaintext, epochKp.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}

	// Real wrapped-MSEK fixture → real MlsCapability holding [0x11u8;32].
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

	// An UNRELATED standing leaf keypair — the epoch trial must be what
	// opens this record, since the standing chain cannot.
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextV1(
		[]faunaFfi.X25519Keypair{leaf},
	)
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}

	// The production construction shape (auth.go): the epoch-aware
	// constructor, not the plain one davauth/CalDAV uses.
	opener, err := mailfauna.NewEpochAwareMailRecordOpener(cap, snapshotPlaintext)
	if err != nil {
		t.Fatalf("NewEpochAwareMailRecordOpener: %v", err)
	}
	defer opener.Zeroize()

	messageID := make([]byte, 32)
	for i := range messageID {
		messageID[i] = 0xA2
	}
	caller := &bodyDecryptCaller{envelope: envelope}
	ct, err := wsrpc.FetchMessageCiphertext(context.Background(), caller, fixtureActorID, messageID)
	if err != nil {
		t.Fatalf("FetchMessageCiphertext: %v", err)
	}

	// The production FETCH call site's exact shape (fetch.go): opens via
	// the record's own stored ingest instant, not "now".
	got, err := mailfauna.OpenStoredRecordAt(opener, ct.EncryptedBody, recordTs)
	if err != nil {
		t.Fatalf("OpenStoredRecordAt: %v", err)
	}
	if string(got) != string(plaintext) {
		t.Fatalf("plaintext mismatch\nwant: %q\ngot:  %q", plaintext, got)
	}
}

// TestIMAPBodyDecryptEpochSealedStaysDarkPastHorizon proves the fail-closed
// half of the same chain: content sealed further back than
// MAIL_EPOCH_PUBLISH_HORIZON epochs stays unreachable even to the MSEK
// holder — the bounded back-scan (design § 4) is not an unbounded search.
func TestIMAPBodyDecryptEpochSealedStaysDarkPastHorizon(t *testing.T) {
	const week = 7 * 24 * 60 * 60
	const horizon = 26 // MAIL_EPOCH_PUBLISH_HORIZON (libs/fauna-mls)
	recordTs := uint64(500*week + 10)
	targetEpoch := mailfauna.MailSealingEpochOf(recordTs)
	tooStaleEpoch := targetEpoch - (horizon + 1)

	msek := bytes.Repeat([]byte{0x11}, 32)
	epochKp, err := faunaFfi.DeriveRecipientEpochHpkeKeypair(msek, tooStaleEpoch)
	if err != nil {
		t.Fatalf("DeriveRecipientEpochHpkeKeypair: %v", err)
	}
	envelope, err := mailfauna.EncryptToRecipient([]byte("unreachable"), epochKp.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}

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

	leaf := faunaFfi.GenerateX25519Keypair()
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextV1(
		[]faunaFfi.X25519Keypair{leaf},
	)
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}
	opener, err := mailfauna.NewEpochAwareMailRecordOpener(cap, snapshotPlaintext)
	if err != nil {
		t.Fatalf("NewEpochAwareMailRecordOpener: %v", err)
	}
	defer opener.Zeroize()

	messageID := make([]byte, 32)
	for i := range messageID {
		messageID[i] = 0xA3
	}
	caller := &bodyDecryptCaller{envelope: envelope}
	ct, err := wsrpc.FetchMessageCiphertext(context.Background(), caller, fixtureActorID, messageID)
	if err != nil {
		t.Fatalf("FetchMessageCiphertext: %v", err)
	}

	if _, err := mailfauna.OpenStoredRecordAt(opener, ct.EncryptedBody, recordTs); err == nil {
		t.Fatal("content sealed beyond the publish horizon must stay dark, got nil error")
	}
}

// TestIMAPBodyDecryptGraceRootOpensPreRotationEpochMail is Track 1c's
// MSEK-rotation grace proof through the exact production path: a record
// epoch-sealed under the OLD generation's root opens on a session whose
// capability holds the NEW MSEK, because the post-rotation snapshot
// (EncodeMlsSnapshotPlaintextFromMseks — the shared
// build_mls_snapshot_plaintext builder rotation.rs uses) carries the old
// generation's mail_epoch_grace_root. A snapshot without the grace root
// leaves the same record dark — the grace material is load-bearing.
func TestIMAPBodyDecryptGraceRootOpensPreRotationEpochMail(t *testing.T) {
	const week = 7 * 24 * 60 * 60
	recordTs := uint64(500*week + 10)
	targetEpoch := mailfauna.MailSealingEpochOf(recordTs)

	// The fixture capability's MSEK is the NEW (post-rotation) generation.
	newMsek := bytes.Repeat([]byte{0x11}, 32)
	oldMsek := bytes.Repeat([]byte{0x22}, 32)
	oldEpochKp, err := faunaFfi.DeriveRecipientEpochHpkeKeypair(oldMsek, targetEpoch)
	if err != nil {
		t.Fatalf("DeriveRecipientEpochHpkeKeypair: %v", err)
	}
	plaintext := []byte("Body epoch-sealed under the pre-rotation root.\r\n")
	envelope, err := mailfauna.EncryptToRecipient(plaintext, oldEpochKp.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}

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

	// The post-rotation snapshot: [new, old] — grace root included.
	rotated, err := faunaFfi.EncodeMlsSnapshotPlaintextFromMseks([][]byte{newMsek, oldMsek})
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextFromMseks: %v", err)
	}
	opener, err := mailfauna.NewEpochAwareMailRecordOpener(cap, rotated)
	if err != nil {
		t.Fatalf("NewEpochAwareMailRecordOpener: %v", err)
	}
	got, err := mailfauna.OpenStoredRecordAt(opener, envelope, recordTs)
	if err != nil {
		t.Fatalf("OpenStoredRecordAt (grace root): %v", err)
	}
	if string(got) != string(plaintext) {
		t.Fatalf("plaintext mismatch\nwant: %q\ngot:  %q", plaintext, got)
	}
	opener.Zeroize()

	// Single-generation snapshot (no grace root) → the record stays dark.
	bare, err := faunaFfi.EncodeMlsSnapshotPlaintextFromMseks([][]byte{newMsek})
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextFromMseks: %v", err)
	}
	bareOpener, err := mailfauna.NewEpochAwareMailRecordOpener(cap, bare)
	if err != nil {
		t.Fatalf("NewEpochAwareMailRecordOpener: %v", err)
	}
	defer bareOpener.Zeroize()
	if _, err := mailfauna.OpenStoredRecordAt(bareOpener, envelope, recordTs); err == nil {
		t.Fatal("pre-rotation epoch mail must stay dark without the grace root, got nil error")
	}
}

// TestBodySearchOpensEpochSealedIndexHint is Track 1b's search-leg proof: an
// index hint sealed under the record's mail sealing epoch (the hint shares
// the body's key schedule — `seal_and_persist_local` seals both to the same
// epoch-gated recipient key in one transaction) opens through the production
// `bodySearch` path when the segment carries its seal instant
// (`IndexSegment.StoredAt`), and the epoch-aware opener + a real epoch seal
// are what make it pass. The second half pins the timestamp THREADING: the
// same segment with StoredAt unknown (0 — a failed append-time clock read,
// only ever a standing-sealed hint) must fail to open this epoch-sealed hint, so a
// blind switch that ignored the segment's own timestamp cannot go green.
func TestBodySearchOpensEpochSealedIndexHint(t *testing.T) {
	const week = 7 * 24 * 60 * 60
	recordTs := uint64(500*week + 10)
	targetEpoch := mailfauna.MailSealingEpochOf(recordTs)

	msek := bytes.Repeat([]byte{0x11}, 32)
	epochKp, err := faunaFfi.DeriveRecipientEpochHpkeKeypair(msek, targetEpoch)
	if err != nil {
		t.Fatalf("DeriveRecipientEpochHpkeKeypair: %v", err)
	}
	hintPlain := mailfauna.Tokenize("subject epoch searchable body").CanonicalBytes
	sealedHint, err := mailfauna.EncryptToRecipient(hintPlain, epochKp.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}

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

	// An UNRELATED standing leaf keypair — the epoch trial must be what
	// opens the hint, not a standing-chain fallthrough.
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextV1(
		[]faunaFfi.X25519Keypair{leaf},
	)
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}
	opener, err := mailfauna.NewEpochAwareMailRecordOpener(cap, snapshotPlaintext)
	if err != nil {
		t.Fatalf("NewEpochAwareMailRecordOpener: %v", err)
	}
	defer opener.Zeroize()

	messageID := bytes.Repeat([]byte{0xA4}, 32)
	segments := []wsrpc.IndexSegment{{
		MessageID:          messageID,
		Mailbox:            "INBOX",
		Modseq:             7,
		EncryptedIndexHint: sealedHint,
		StoredAt:           int64(recordTs),
	}}
	uidByMid := map[string]uint32{string(messageID): 42}

	matched, err := bodySearch(opener, segments, []string{"searchable"}, uidByMid, nil)
	if err != nil {
		t.Fatalf("bodySearch over an epoch-sealed hint: %v", err)
	}
	if len(matched) != 1 || matched[0] != 42 {
		t.Fatalf("matched = %v, want [42]", matched)
	}

	// Threading pin: drop the seal instant and the epoch-sealed hint must
	// NOT open (epoch_of(0)'s trials + the standing arm + a saturated
	// back-scan all miss epoch 500) — proving bodySearch really passes the
	// segment's own StoredAt, not a constant.
	segments[0].StoredAt = 0
	if _, err := bodySearch(opener, segments, []string{"searchable"}, uidByMid, nil); err == nil {
		t.Fatal("an epoch-sealed hint with no seal instant must fail to open, got nil error")
	}
}
