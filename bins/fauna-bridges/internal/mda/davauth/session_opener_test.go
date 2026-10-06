package davauth

// Phase-3 S2 (F2 per-session opener): resolve() builds the per-session
// MailRecordOpener ONCE where the MLS-snapshot plaintext lands, so serve-path
// opens skip the per-call snapshot marshal + re-parse. These tests pin the
// three lifecycle arms: built-and-usable on a snapshot-carrying AUTH, nil on a
// snapshot-less AUTH (AUTH still succeeds), and AUTH-FAIL on a snapshot that
// decrypts but does not parse (same failure class as a snapshot AEAD failure —
// the MSEK already unwrapped, so a mangled snapshot is a real integrity
// problem).

import (
	"context"
	"log/slog"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// fixtureMSEK mirrors `libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs`
// (`let msek = [0x11u8; 32]`), so the snapshot the test seals matches the MSEK
// the AUTH flow unwraps from `wrapped_msek.bin` (same convention as the
// caldav/carddav server-test fixtures).
var fixtureMSEK = func() []byte {
	b := make([]byte, 32)
	for i := range b {
		b[i] = 0x11
	}
	return b
}()

// testAuthMiddleware builds the minimal authMiddleware resolve() needs
// (client + cache + clock); the HTTP realm/lockout plumbing is exercised by
// the caldav/carddav server tests.
func testAuthMiddleware(caller *mockCaller) *authMiddleware {
	return &authMiddleware{
		client: caller,
		logger: slog.Default(),
		now:    time.Now,
		cache:  newAuthMaterialCache(),
	}
}

// TestResolveBuildsRecordOpener pins the happy path: an AUTH that fetches +
// decrypts a valid MLS snapshot leaves a non-nil per-session opener on the
// Session, and that opener actually HPKE-opens a record sealed to the
// snapshot's leaf key. Close() then zeroizes it.
func TestResolveBuildsRecordOpener(t *testing.T) {
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextV1(
		[]faunaFfi.X25519Keypair{leaf},
	)
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}
	snapshotBlob, err := faunaFfi.SealMlsSnapshotBlob(snapshotPlaintext, fixtureActorID, fixtureMSEK)
	if err != nil {
		t.Fatalf("SealMlsSnapshotBlob: %v", err)
	}
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            mustReadFixture(t, "wrapped_msek.bin"),
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
		mlsSnapshotBlob:        snapshotBlob,
	}
	m := testAuthMiddleware(caller)

	sess, err := m.resolve(context.Background(), fixtureLocalPart, fixtureDomain, "default", string(fixturePlainPassword))
	if err != nil {
		t.Fatalf("resolve: %v", err)
	}
	defer sess.Close()

	opener := sess.RecordOpener()
	if opener == nil {
		t.Fatal("RecordOpener() = nil after a snapshot-carrying AUTH, want the per-session opener")
	}
	sealed, err := mailfauna.EncryptToRecipient([]byte("body"), leaf.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}
	pt, err := opener.Open(sealed)
	if err != nil {
		t.Fatalf("opener.Open: %v", err)
	}
	if string(pt) != "body" {
		t.Fatalf("opener.Open = %q, want %q", pt, "body")
	}

	sess.Close()
	if sess.RecordOpener() != nil {
		t.Fatal("RecordOpener() must be nil after Close (zeroized alongside the capability)")
	}
}

// TestResolveNoSnapshotLeavesNilOpener pins the snapshot-less arm: AUTH still
// succeeds (the MUA must complete the well-known-URL dance) and the opener is
// nil — per-record opens surface the missing-snapshot error at call time.
func TestResolveNoSnapshotLeavesNilOpener(t *testing.T) {
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            mustReadFixture(t, "wrapped_msek.bin"),
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
		// mlsSnapshotBlob nil — nest has no snapshot on file.
	}
	m := testAuthMiddleware(caller)

	sess, err := m.resolve(context.Background(), fixtureLocalPart, fixtureDomain, "default", string(fixturePlainPassword))
	if err != nil {
		t.Fatalf("resolve without a snapshot must still AUTH: %v", err)
	}
	defer sess.Close()
	if sess.RecordOpener() != nil {
		t.Fatal("RecordOpener() must be nil when nest has no snapshot on file")
	}
}

// TestResolveUnparseableSnapshotFailsAuth pins the integrity arm: a snapshot
// blob that AEAD-decrypts fine (sealed under the right MSEK) but carries
// garbage instead of a canonical MlsSnapshotPlaintext fails the AUTH — the
// same failure class as a snapshot AEAD failure — and leaves no live
// capability on the session.
func TestResolveUnparseableSnapshotFailsAuth(t *testing.T) {
	garbageBlob, err := faunaFfi.SealMlsSnapshotBlob(
		[]byte("not-an-mls-snapshot-plaintext"), fixtureActorID, fixtureMSEK,
	)
	if err != nil {
		t.Fatalf("SealMlsSnapshotBlob: %v", err)
	}
	caller := &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            mustReadFixture(t, "wrapped_msek.bin"),
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
		mlsSnapshotBlob:        garbageBlob,
	}
	m := testAuthMiddleware(caller)

	sess, err := m.resolve(context.Background(), fixtureLocalPart, fixtureDomain, "default", string(fixturePlainPassword))
	if err == nil {
		t.Fatal("resolve with an unparseable snapshot must fail AUTH, got nil error")
	}
	if sess.MLSUnwrap() != nil {
		t.Fatal("capability must be zeroized + cleared on the unparseable-snapshot failure path")
	}
	if sess.RecordOpener() != nil {
		t.Fatal("no opener may survive the unparseable-snapshot failure path")
	}
	sess.Close()
}
