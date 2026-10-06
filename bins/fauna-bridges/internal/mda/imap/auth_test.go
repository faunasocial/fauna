package imap

import (
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/authlock"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc/wsrpctest"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// Fixture parameters mirror libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs.
// Drift here is a test bug; the gen example is the source of truth for what's
// in the committed binary fixtures.
var (
	fixtureActorID = func() []byte {
		b := make([]byte, 32)
		for i := range b {
			b[i] = 0x42
		}
		return b
	}()
	fixtureCredentialID  = "default"
	fixturePlainPassword = []byte("deterministic-pw")
	fixtureOAuthToken    = []byte("oauth-deterministic-token")
	fixtureMLSPubkey     = func() []byte {
		b := make([]byte, 32)
		for i := range b {
			b[i] = 0xAA
		}
		return b
	}()
	fixtureIndexKey = func() []byte {
		b := make([]byte, 32)
		for i := range b {
			b[i] = 0xBB
		}
		return b
	}()
)

// repoTestVectorsPath resolves the canonical fixtures directory from
// this package (5 levels deep in the repo). Keeps tests self-locating
// regardless of where `go test` is invoked from.
func repoTestVectorsPath(name string) string {
	return filepath.Join("..", "..", "..", "..", "..",
		"libs", "fauna-protocol", "schemas", "test_vectors", name)
}

func mustReadFixture(t *testing.T, name string) []byte {
	t.Helper()
	p := repoTestVectorsPath(name)
	b, err := os.ReadFile(p)
	if err != nil {
		t.Fatalf("read fixture %s: %v", p, err)
	}
	return b
}

// mustUnwrapPlainFixture returns a real *MLSCapability constructed
// from the committed PLAIN wrapped-MSEK test vector. Shared by tests
// that need a live capability (Session.Close zeroization,
// post-Close-fails-Decrypt regression).
func mustUnwrapPlainFixture(t *testing.T) *mailfauna.MLSCapability {
	t.Helper()
	blob := mustReadFixture(t, "wrapped_msek.bin")
	cap, err := mailfauna.UnwrapMLSBlob(
		blob,
		fixturePlainPassword,
		fixtureActorID,
		fixtureCredentialID,
		mailfauna.KdfKindArgon2id,
	)
	if err != nil {
		t.Fatalf("unwrap PLAIN fixture: %v", err)
	}
	return cap
}

// ── Programmable caller for AUTH tests ───────────────────────────
//
// authCaller is a wsrpc.Caller fake that dispatches per-method to a
// caller-supplied behaviour. Unlike the session_test.go recordingCaller
// (which only counts calls and returns nil), this fake encodes a real
// reply CBOR and unmarshals into the wrapper's reply pointer — the
// flow exercises the full wire-shape round-trip the wrappers do in
// production.
type authCaller struct {
	mu    sync.Mutex
	calls []recordedAuthCall

	// validateRecipientReplyHex is the hex(actor_id) returned for
	// the canned validate_recipient call. Empty string → returns a
	// "reject" outcome which makes the wrapper error.
	validateRecipientReplyHex string

	// blobBytes is the canonical-CBOR wrapped-MSEK fetch_wrapped_mls_blob
	// returns. nil → nest "no blob on file" (Option<ByteBuf>::None).
	blobBytes []byte

	// mlsPubkey is what fetch_recipient_mls_pubkey returns; nil →
	// recipient has no MLS pubkey provisioned.
	mlsPubkey []byte

	// indexKey is what fetch_recipient_index_key returns; nil →
	// actor has no index pubkey provisioned (Phase E will land
	// production provisioning).
	indexKey []byte

	// mlsSnapshotBlob is the canonical-CBOR `MlsSnapshotBlob`
	// (encrypted under MSEK) `fetch_mls_snapshot_blob` returns; nil →
	// nest has no snapshot on file (user's primary client hasn't
	// provisioned one yet). AUTH still succeeds in that case so the
	// MUA can list mailboxes / inspect flags.
	mlsSnapshotBlob []byte

	// fetchBlobErr (when non-nil) replaces the blob reply with a
	// transport-layer failure — useful for exercising the auth.go
	// "blob fetch failed → audit fail" branch.
	fetchBlobErr error
}

type recordedAuthCall struct {
	method string
	body   []byte
}

func (a *authCaller) Call(_ context.Context, method string, body, reply any) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	a.calls = append(a.calls, recordedAuthCall{method: method, body: enc})

	switch method {
	case wsrpc.MethodValidateRecipient:
		if a.validateRecipientReplyHex == "" {
			r := map[string]any{
				"outcome": "reject",
				"reason":  "unknown recipient",
			}
			return encodeReply(r, reply)
		}
		raw, err := hex.DecodeString(a.validateRecipientReplyHex)
		if err != nil {
			return fmt.Errorf("test fake bad hex: %w", err)
		}
		r := map[string]any{
			"outcome":  "resolved",
			"actor_id": raw,
		}
		return encodeReply(r, reply)

	case wsrpc.MethodFetchWrappedMLSBlob:
		if a.fetchBlobErr != nil {
			return a.fetchBlobErr
		}
		r := struct {
			Blob *[]byte `cbor:"blob"`
		}{Blob: nil}
		if a.blobBytes != nil {
			b := a.blobBytes
			r.Blob = &b
		}
		return encodeReply(r, reply)

	case wsrpc.MethodFetchRecipientMLSPubkey:
		r := struct {
			Key *map[string][]byte `cbor:"key"`
		}{}
		if a.mlsPubkey != nil {
			key := wsrpctest.RecipientSealKey(a.mlsPubkey)
			r.Key = &key
		}
		return encodeReply(r, reply)

	case wsrpc.MethodFetchRecipientIndexKey:
		r := struct {
			Pubkey *[]byte `cbor:"pubkey"`
		}{Pubkey: nil}
		if a.indexKey != nil {
			b := a.indexKey
			r.Pubkey = &b
		}
		return encodeReply(r, reply)

	case wsrpc.MethodFetchMLSSnapshotBlob:
		r := struct {
			Blob *[]byte `cbor:"blob"`
		}{Blob: nil}
		if a.mlsSnapshotBlob != nil {
			b := a.mlsSnapshotBlob
			r.Blob = &b
		}
		return encodeReply(r, reply)

	case wsrpc.MethodReportAuthEvent,
		wsrpc.MethodReportSessionClose:
		r := struct {
			OK bool `cbor:"ok"`
		}{OK: true}
		return encodeReply(r, reply)
	}

	return fmt.Errorf("authCaller: unexpected method %q", method)
}

// encodeReply round-trips a Go value through canonical CBOR into the
// wrapper's reply target. Mirrors the methods_test.go pattern.
func encodeReply(value, reply any) error {
	if reply == nil {
		return nil
	}
	enc, err := dagcbor.Marshal(value)
	if err != nil {
		return fmt.Errorf("encode reply: %w", err)
	}
	return cbor.Unmarshal(enc, reply)
}

func (a *authCaller) callsOf(method string) []recordedAuthCall {
	a.mu.Lock()
	defer a.mu.Unlock()
	var out []recordedAuthCall
	for _, c := range a.calls {
		if c.method == method {
			out = append(out, c)
		}
	}
	return out
}

// ── Tests ─────────────────────────────────────────────────────────

// TestPLAINSuccess pins the happy path: a valid email + correct
// password resolves the actor, fetches the wrapped blob, AEAD-unwraps
// it, caches the capability + MLS pubkey, and fires
// report_auth_event(result=ok). Per imap-server.md § Authentication.
func TestPLAINSuccess(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}
	sess := newTestSession(caller)

	payload := plainPayload("", "alice@example.com", string(fixturePlainPassword))
	if err := sess.authenticate("PLAIN", payload); err != nil {
		t.Fatalf("authenticate: %v", err)
	}

	if !equalBytes(sess.actorID, fixtureActorID) {
		t.Fatalf("actorID = %x, want %x", sess.actorID, fixtureActorID)
	}
	if sess.credentialID != fixtureCredentialID {
		t.Fatalf("credentialID = %q, want %q", sess.credentialID, fixtureCredentialID)
	}
	if sess.mlsUnwrap == nil {
		t.Fatal("mlsUnwrap must be set after successful AUTH")
	}
	if !equalBytes(sess.actorMLSPubkey, fixtureMLSPubkey) {
		t.Fatalf("actorMLSPubkey = %x, want %x", sess.actorMLSPubkey, fixtureMLSPubkey)
	}
	if got := caller.callsOf(wsrpc.MethodReportAuthEvent); len(got) != 1 {
		t.Fatalf("report_auth_event fired %d times, want 1", len(got))
	}
}

// TestPLAINBareUsernameResolvesUnderPrimaryDomain pins the uniform
// bare-username fix on the IMAP surface: a SASL username with no `@domain`
// resolves under the box's PrimaryDomain (auth.SplitEmailDefault), matching
// the CalDAV + MTA surfaces, instead of failing AUTH as malformed. (The same
// fix that unblocks macOS Calendar.app on CalDAV; uniform per priority #1.)
func TestPLAINBareUsernameResolvesUnderPrimaryDomain(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}
	sess := newTestSession(caller)
	sess.primaryDomain = "example.com"

	// Bare username — no '@domain'.
	payload := plainPayload("", "alice", string(fixturePlainPassword))
	if err := sess.authenticate("PLAIN", payload); err != nil {
		t.Fatalf("bare username must authenticate under PrimaryDomain: %v", err)
	}
	if !equalBytes(sess.actorID, fixtureActorID) {
		t.Fatalf("actorID = %x, want %x", sess.actorID, fixtureActorID)
	}
	calls := caller.callsOf(wsrpc.MethodValidateRecipient)
	if len(calls) != 1 {
		t.Fatalf("validate_recipient fired %d times, want 1", len(calls))
	}
	var got map[string]any
	if err := cbor.Unmarshal(calls[0].body, &got); err != nil {
		t.Fatalf("decode validate_recipient body: %v", err)
	}
	if lp, _ := got["local_part"].(string); lp != "alice" {
		t.Errorf("validate_recipient local_part = %q, want %q", lp, "alice")
	}
	if d, _ := got["domain"].(string); d != "example.com" {
		t.Errorf("validate_recipient domain = %q, want %q (PrimaryDomain default)", d, "example.com")
	}
}

// TestPLAINFailsWithWrongPassword pins the AEAD-fail-as-auth-fail
// path. The bridge MUST report `report_auth_event(result=fail)` on
// every failure so nest can rate-limit the offending credential.
func TestPLAINFailsWithWrongPassword(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}
	sess := newTestSession(caller)

	payload := plainPayload("", "alice@example.com", "WRONG-PASSWORD")
	err := sess.authenticate("PLAIN", payload)
	if err == nil {
		t.Fatal("wrong password must produce an AUTH error")
	}
	if sess.actorID != nil || sess.mlsUnwrap != nil {
		t.Fatal("AUTH-fail must leave Session unauthenticated")
	}
	if got := caller.callsOf(wsrpc.MethodReportAuthEvent); len(got) != 1 {
		t.Fatalf("report_auth_event fired %d times on fail, want 1", len(got))
	}
}

// TestOAUTHBEARERSuccess pins the HKDF arm: a valid OAUTHBEARER
// GS2-formatted payload carrying the matching token unwraps the
// HKDF-sealed blob and caches the capability.
func TestOAUTHBEARERSuccess(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek_oauth.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}
	sess := newTestSession(caller)

	payload := oauthBearerPayload("alice@example.com", string(fixtureOAuthToken))
	if err := sess.authenticate("OAUTHBEARER", payload); err != nil {
		t.Fatalf("authenticate OAUTHBEARER: %v", err)
	}
	if sess.mlsUnwrap == nil {
		t.Fatal("mlsUnwrap must be set after successful OAUTHBEARER AUTH")
	}
	if got := caller.callsOf(wsrpc.MethodReportAuthEvent); len(got) != 1 {
		t.Fatalf("report_auth_event fired %d times, want 1", len(got))
	}
}

// TestOAUTHBEARERFailsWithWrongToken pins the HKDF AEAD-fail path.
func TestOAUTHBEARERFailsWithWrongToken(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek_oauth.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}
	sess := newTestSession(caller)

	payload := oauthBearerPayload("alice@example.com", "WRONG-TOKEN")
	if err := sess.authenticate("OAUTHBEARER", payload); err == nil {
		t.Fatal("wrong token must produce an AUTH error")
	}
	if sess.mlsUnwrap != nil {
		t.Fatal("AUTH-fail must leave Session unauthenticated")
	}
	if got := caller.callsOf(wsrpc.MethodReportAuthEvent); len(got) != 1 {
		t.Fatalf("report_auth_event fired %d times on fail, want 1", len(got))
	}
}

// TestAuthCachesActorIndexKey pins the I5 Phase D.5 addition: after a
// successful AUTH, the Session caches the actor's own index pubkey so
// APPEND can seal the search-index hint to it without a per-call RPC.
// Parallel to actorMLSPubkey, which Phase C.3 already wired.
func TestAuthCachesActorIndexKey(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
		indexKey:                  fixtureIndexKey,
	}
	sess := newTestSession(caller)

	payload := plainPayload("", "alice@example.com", string(fixturePlainPassword))
	if err := sess.authenticate("PLAIN", payload); err != nil {
		t.Fatalf("authenticate: %v", err)
	}
	if !equalBytes(sess.actorIndexKey, fixtureIndexKey) {
		t.Fatalf("actorIndexKey = %x, want %x", sess.actorIndexKey, fixtureIndexKey)
	}
	// Exactly one round-trip to nest for the index key — the AUTH path
	// must not refetch on every APPEND.
	if got := caller.callsOf(wsrpc.MethodFetchRecipientIndexKey); len(got) != 1 {
		t.Fatalf("fetch_recipient_index_key fired %d times, want 1", len(got))
	}
}

// TestAuthSucceedsWithoutIndexKeyProvisioned covers the Phase E gap:
// production provisioning of the actor's index pubkey is a Phase E
// concern, so today the RPC returns None for every actor.  AUTH must
// still succeed; APPEND surfaces the missing-index-key error at call
// time instead of failing AUTH and locking the user out of every
// other IMAP command.
func TestAuthSucceedsWithoutIndexKeyProvisioned(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
		indexKey:                  nil, // not yet provisioned
	}
	sess := newTestSession(caller)

	payload := plainPayload("", "alice@example.com", string(fixturePlainPassword))
	if err := sess.authenticate("PLAIN", payload); err != nil {
		t.Fatalf("authenticate: %v", err)
	}
	if sess.actorIndexKey != nil {
		t.Fatalf("actorIndexKey must be nil when nest returns None, got %x", sess.actorIndexKey)
	}
	if sess.actorID == nil {
		t.Fatal("AUTH must succeed even without an index pubkey")
	}
}

// fixtureMSEK mirrors the raw MSEK `[0x11; 32]` the gen example
// (libs/fauna-mls/examples/gen_wrapped_blob_vectors.rs) sealed into
// `wrapped_msek.bin`, so tests can mint additional blobs under the same
// key at test time (e.g. a snapshot blob whose plaintext is a VALID
// MlsSnapshotPlaintext — the committed `mls_snapshot.bin` carries
// non-parsing placeholder bytes).
var fixtureMSEK = func() []byte {
	b := make([]byte, 32)
	for i := range b {
		b[i] = 0x11
	}
	return b
}()

// validSnapshotBlobFixture builds an MSEK-sealed MlsSnapshotBlob whose
// plaintext is a real one-leaf MlsSnapshotPlaintext, plus the leaf keypair
// it carries — what a user's primary client provisions in production.
func validSnapshotBlobFixture(t *testing.T) ([]byte, faunaFfi.X25519Keypair) {
	t.Helper()
	leaf := faunaFfi.GenerateX25519Keypair()
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextV1(
		[]faunaFfi.X25519Keypair{leaf},
	)
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextV1: %v", err)
	}
	blob, err := faunaFfi.SealMlsSnapshotBlob(snapshotPlaintext, fixtureActorID, fixtureMSEK)
	if err != nil {
		t.Fatalf("SealMlsSnapshotBlob: %v", err)
	}
	return blob, leaf
}

// TestAuthConstructsRecordOpener pins the Phase-3 S2 shape of the I5
// Phase F addition: the AUTH path fetches the encrypted MlsSnapshotBlob
// via `fauna.bridges.fetch_mls_snapshot_blob`, AEAD-unwraps it under MSEK
// via the existing `MlsCapability.Decrypt`, parses it ONCE into the
// per-connection `recordOpener` (the F2 perf fold), and the opener can
// actually open a record sealed to the snapshot's leaf key.
func TestAuthConstructsRecordOpener(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	snapshotBlob, leaf := validSnapshotBlobFixture(t)
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
		indexKey:                  fixtureIndexKey,
		mlsSnapshotBlob:           snapshotBlob,
	}
	sess := newTestSession(caller)

	payload := plainPayload("", "alice@example.com", string(fixturePlainPassword))
	if err := sess.authenticate("PLAIN", payload); err != nil {
		t.Fatalf("authenticate: %v", err)
	}
	if sess.recordOpener == nil {
		t.Fatalf("AUTH with a snapshot on file must construct the record opener")
	}
	// The opener must open a record sealed to the snapshot's leaf key —
	// the production serve-path round-trip.
	want := []byte("post-AUTH round-trip body")
	envelope, err := mailfauna.EncryptToRecipient(want, leaf.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}
	got, err := mailfauna.OpenStoredRecord(sess.recordOpener, envelope)
	if err != nil {
		t.Fatalf("OpenStoredRecord via the AUTH-built opener: %v", err)
	}
	if !equalBytes(got, want) {
		t.Fatalf("opened plaintext = %q, want %q", got, want)
	}
	// Exactly one round-trip — the AUTH path must not refetch on
	// every FETCH BODY[].
	if got := caller.callsOf(wsrpc.MethodFetchMLSSnapshotBlob); len(got) != 1 {
		t.Fatalf("fetch_mls_snapshot_blob fired %d times, want 1", len(got))
	}
}

// TestAuthFailsOnUnparseableSnapshot: a snapshot blob that AEAD-unwraps
// fine but does NOT parse as an MlsSnapshotPlaintext is tampered — AUTH
// must fail, exactly like a snapshot AEAD failure. The committed
// `mls_snapshot.bin` fixture (placeholder plaintext
// `b"deterministic snapshot bytes"`) is precisely such a blob.
func TestAuthFailsOnUnparseableSnapshot(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	snapshotBlob := mustReadFixture(t, "mls_snapshot.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
		indexKey:                  fixtureIndexKey,
		mlsSnapshotBlob:           snapshotBlob,
	}
	sess := newTestSession(caller)

	payload := plainPayload("", "alice@example.com", string(fixturePlainPassword))
	if err := sess.authenticate("PLAIN", payload); err == nil {
		t.Fatal("an unparseable MLS snapshot must fail AUTH")
	}
	if sess.actorID != nil || sess.recordOpener != nil {
		t.Fatal("failed AUTH must leave the session unauthenticated with no opener")
	}
}

// TestAuthSucceedsWithoutMLSSnapshotProvisioned mirrors the indexKey
// gap test: the user's primary client hasn't provisioned an
// `MlsSnapshotBlob` yet (Phase E / cross-device provisioning is
// out-of-scope for this track), so nest returns None. AUTH must
// still succeed so the MUA can complete the LIST / SELECT dance;
// per-message FETCH BODY[] surfaces the missing-snapshot error at
// call time.
func TestAuthSucceedsWithoutMLSSnapshotProvisioned(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
		indexKey:                  fixtureIndexKey,
		mlsSnapshotBlob:           nil, // not yet provisioned
	}
	sess := newTestSession(caller)

	payload := plainPayload("", "alice@example.com", string(fixturePlainPassword))
	if err := sess.authenticate("PLAIN", payload); err != nil {
		t.Fatalf("authenticate: %v", err)
	}
	if sess.recordOpener != nil {
		t.Fatal("recordOpener must be nil when nest returns None")
	}
	if sess.actorID == nil {
		t.Fatal("AUTH must succeed even without an MLS snapshot")
	}
	// Even when nest has no snapshot, the AUTH path MUST fire the
	// fetch RPC — the per-message FETCH BODY[] path can't be the
	// thing that discovers "nest has no snapshot yet" or it'll do so
	// once per message.
	if got := caller.callsOf(wsrpc.MethodFetchMLSSnapshotBlob); len(got) != 1 {
		t.Fatalf("fetch_mls_snapshot_blob fired %d times on missing-snapshot, want 1", len(got))
	}
}

// TestAuthFailsWhenNestHasNoBlob covers the "user not provisioned for
// IMAP" path: validate_recipient resolves the actor but
// fetch_wrapped_mls_blob returns None. The bridge MUST treat this as
// AUTH-fail (no decryption capability available) AND report it.
func TestAuthFailsWhenNestHasNoBlob(t *testing.T) {
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 nil, // nest: no blob on file
		mlsPubkey:                 fixtureMLSPubkey,
	}
	sess := newTestSession(caller)

	err := sess.authenticate("PLAIN", plainPayload("", "alice@example.com", "any"))
	if err == nil {
		t.Fatal("missing blob must produce an AUTH error")
	}
	if got := caller.callsOf(wsrpc.MethodReportAuthEvent); len(got) != 1 {
		t.Fatalf("report_auth_event fired %d times on missing-blob, want 1", len(got))
	}
}

// TestAuthRejectsUnknownMechanism pins the SASL-mech enumeration: only
// PLAIN and OAUTHBEARER are wired in Phase C. Anything else returns
// an error and DOES NOT consult the wsrpc client.
func TestAuthRejectsUnknownMechanism(t *testing.T) {
	caller := &authCaller{}
	sess := newTestSession(caller)

	if err := sess.authenticate("LOGIN", "alice"); err == nil {
		t.Fatal("LOGIN mech must be rejected")
	}
	if err := sess.authenticate("CRAM-MD5", ""); err == nil {
		t.Fatal("CRAM-MD5 mech must be rejected")
	}
	if len(caller.calls) != 0 {
		t.Fatalf("unknown-mech path must not call wsrpc; got %d calls", len(caller.calls))
	}
}

// TestLoginCommandSuccess pins the IMAP `LOGIN <user> <pass>` command
// (Session.Login) onto the same AEAD-unwrap-as-auth path as
// AUTHENTICATE PLAIN: a valid email + correct password authenticates
// the session. LOGIN-over-TLS == PLAIN-over-TLS (imap-server.md §
// Authentication); the fork gates LOGIN behind TLS so this method is
// only reached post-TLS.
func TestLoginCommandSuccess(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}
	sess := newTestSession(caller)

	if err := sess.Login("alice@example.com", string(fixturePlainPassword)); err != nil {
		t.Fatalf("Login: %v", err)
	}
	if !equalBytes(sess.actorID, fixtureActorID) {
		t.Fatalf("actorID = %x, want %x", sess.actorID, fixtureActorID)
	}
	if sess.mlsUnwrap == nil {
		t.Fatal("mlsUnwrap must be set after successful LOGIN")
	}
	if got := caller.callsOf(wsrpc.MethodReportAuthEvent); len(got) != 1 {
		t.Fatalf("report_auth_event fired %d times, want 1", len(got))
	}
}

// TestLoginCommandFailsWithWrongPassword pins that a bad LOGIN
// credential returns a tagged-NO *imap.Error (NOT the [SERVERBUG] a
// bare Go error would map to in the fork's command dispatch), fires the
// result=fail audit event, and leaves the session unauthenticated.
func TestLoginCommandFailsWithWrongPassword(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}
	sess := newTestSession(caller)

	err := sess.Login("alice@example.com", "WRONG-PASSWORD")
	if err == nil {
		t.Fatal("wrong password must produce a LOGIN error")
	}
	var imapErr *imap.Error
	if !errors.As(err, &imapErr) || imapErr.Type != imap.StatusResponseTypeNo {
		t.Fatalf("LOGIN failure must be a NO *imap.Error (avoids [SERVERBUG]); got %T %v", err, err)
	}
	if sess.actorID != nil || sess.mlsUnwrap != nil {
		t.Fatal("LOGIN-fail must leave Session unauthenticated")
	}
	if got := caller.callsOf(wsrpc.MethodReportAuthEvent); len(got) != 1 {
		t.Fatalf("report_auth_event fired %d times on LOGIN-fail, want 1", len(got))
	}
}

// TestSessionSASLAdvertisesMechs covers AuthenticateMechanisms — the
// emersion/go-imap SessionSASL interface emersion consults during the
// CAPABILITY response (post-TLS).
func TestSessionSASLAdvertisesMechs(t *testing.T) {
	sess := newTestSession(&authCaller{})
	mechs := sess.AuthenticateMechanisms()
	want := map[string]bool{"PLAIN": true, "OAUTHBEARER": true}
	for _, m := range mechs {
		if !want[m] {
			t.Errorf("advertised unexpected mech %q", m)
		}
		delete(want, m)
	}
	for m := range want {
		t.Errorf("missing required mech %q", m)
	}
}

// TestSessionSASLAuthenticatePlainViaSASL covers the SessionSASL
// integration: emersion's handleAuthenticate calls Session.Authenticate
// (the SASL-mech entry, not the lowercase helper), and the returned
// sasl.Server drives the multi-round exchange. For PLAIN, the
// authenticator receives identity+user+pass directly.
func TestSessionSASLAuthenticatePlainViaSASL(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	caller := &authCaller{
		validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
		blobBytes:                 blob,
		mlsPubkey:                 fixtureMLSPubkey,
	}
	sess := newTestSession(caller)

	server, err := sess.Authenticate("PLAIN")
	if err != nil {
		t.Fatalf("Authenticate(PLAIN): %v", err)
	}
	// Drive the SASL server with the standard PLAIN initial response.
	resp := []byte("\x00alice@example.com\x00" + string(fixturePlainPassword))
	_, done, err := server.Next(resp)
	if err != nil {
		t.Fatalf("server.Next: %v", err)
	}
	if !done {
		t.Fatal("PLAIN SASL must be one-shot")
	}
	if sess.mlsUnwrap == nil {
		t.Fatal("SASL PLAIN didn't populate the capability")
	}
}

// ── AUTH-failure lockout (security review § D4/M1) ────────────────

// newLockedTestSession builds a Session sharing one lockout + a fixed
// source IP, so a test can simulate many connections from the same
// attacker (the production Backend shares one lockout across sessions).
func newLockedTestSession(c *authCaller, lockout *authlock.Lockout, sourceIP string) *Session {
	return &Session{client: c, lockout: lockout, sourceIP: sourceIP}
}

// TestPLAINLockoutShortCircuitsBeforeKDF pins the headline D4/M1 fix:
// once a (username, source-IP) has racked up `limit` failures, the next
// attempt is refused BEFORE validate_recipient and the Argon2id unwrap —
// so a brute-forcer can't keep paying the bridge's KDF cost or harvest an
// AEAD-timing oracle. The lockout is shared across sessions (one per TCP
// conn), keyed by (username, credential, IP).
func TestPLAINLockoutShortCircuitsBeforeKDF(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	lockout := authlock.New(3, time.Minute, nil)
	wrong := plainPayload("", "alice@example.com", "WRONG-PASSWORD")

	newSess := func() (*Session, *authCaller) {
		c := &authCaller{
			validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
			blobBytes:                 blob,
			mlsPubkey:                 fixtureMLSPubkey,
		}
		return newLockedTestSession(c, lockout, "203.0.113.7"), c
	}

	// 3 failed connections from the same (user, IP) trip the lockout.
	for i := 0; i < 3; i++ {
		s, c := newSess()
		if err := s.authenticate("PLAIN", wrong); err == nil {
			t.Fatalf("attempt %d: wrong password must fail", i+1)
		}
		if got := len(c.callsOf(wsrpc.MethodValidateRecipient)); got != 1 {
			t.Fatalf("attempt %d: expected 1 validate_recipient, got %d", i+1, got)
		}
	}

	// 4th connection is locked out: refused before any nest call / KDF.
	s4, c4 := newSess()
	if err := s4.authenticate("PLAIN", wrong); err == nil {
		t.Fatal("locked-out attempt must still fail")
	}
	if got := len(c4.callsOf(wsrpc.MethodValidateRecipient)); got != 0 {
		t.Fatalf("locked-out attempt called validate_recipient %d times; must short-circuit before the KDF", got)
	}
	if got := len(c4.callsOf(wsrpc.MethodReportAuthEvent)); got != 0 {
		t.Fatalf("locked-out attempt fired report_auth_event %d times; the lockout branch must skip the audit (mirror MTA)", got)
	}
}

// TestPLAINLockoutResetsOnSuccess pins that a correct password clears the
// counter, so earlier typos never lock out a legitimate user. limit=3:
// 2 fails, 1 success (reset), then it takes a fresh 3 fails to lock again.
func TestPLAINLockoutResetsOnSuccess(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	lockout := authlock.New(3, time.Minute, nil)
	wrong := plainPayload("", "alice@example.com", "WRONG-PASSWORD")
	good := plainPayload("", "alice@example.com", string(fixturePlainPassword))

	newSess := func() (*Session, *authCaller) {
		c := &authCaller{
			validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
			blobBytes:                 blob,
			mlsPubkey:                 fixtureMLSPubkey,
		}
		return newLockedTestSession(c, lockout, "203.0.113.7"), c
	}

	for i := 0; i < 2; i++ {
		s, _ := newSess()
		_ = s.authenticate("PLAIN", wrong)
	}
	// Success resets the (user, IP) counter.
	sOK, _ := newSess()
	if err := sOK.authenticate("PLAIN", good); err != nil {
		t.Fatalf("good password after 2 typos must succeed: %v", err)
	}
	// Two more fails must NOT lock (counter was reset; only 2 accrued).
	for i := 0; i < 2; i++ {
		s, c := newSess()
		_ = s.authenticate("PLAIN", wrong)
		if got := len(c.callsOf(wsrpc.MethodValidateRecipient)); got != 1 {
			t.Fatalf("post-reset fail %d: lockout fired early (validate_recipient called %d times)", i+1, got)
		}
	}
}

// TestReportAuthEventCarriesSourceIP pins the M1 audit-plumbing fix: the
// bridge now stamps the real client IP on report_auth_event (it used to
// send an empty source_ip, which nest rejects as malformed). Covers both
// the fail and success paths.
func TestReportAuthEventCarriesSourceIP(t *testing.T) {
	blob := mustReadFixture(t, "wrapped_msek.bin")
	const wantIP = "203.0.113.9"

	sourceIPOf := func(rec recordedAuthCall) string {
		var m map[string]any
		if err := cbor.Unmarshal(rec.body, &m); err != nil {
			t.Fatalf("decode report_auth_event body: %v", err)
		}
		ip, _ := m["source_ip"].(string)
		return ip
	}

	t.Run("fail", func(t *testing.T) {
		c := &authCaller{
			validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
			blobBytes:                 blob,
			mlsPubkey:                 fixtureMLSPubkey,
		}
		s := newLockedTestSession(c, nil, wantIP)
		_ = s.authenticate("PLAIN", plainPayload("", "alice@example.com", "WRONG"))
		reports := c.callsOf(wsrpc.MethodReportAuthEvent)
		if len(reports) != 1 {
			t.Fatalf("report_auth_event fired %d times, want 1", len(reports))
		}
		if got := sourceIPOf(reports[0]); got != wantIP {
			t.Fatalf("fail report source_ip = %q, want %q", got, wantIP)
		}
	})

	t.Run("ok", func(t *testing.T) {
		c := &authCaller{
			validateRecipientReplyHex: hex.EncodeToString(fixtureActorID),
			blobBytes:                 blob,
			mlsPubkey:                 fixtureMLSPubkey,
		}
		s := newLockedTestSession(c, nil, wantIP)
		if err := s.authenticate("PLAIN", plainPayload("", "alice@example.com", string(fixturePlainPassword))); err != nil {
			t.Fatalf("auth: %v", err)
		}
		reports := c.callsOf(wsrpc.MethodReportAuthEvent)
		if len(reports) != 1 {
			t.Fatalf("report_auth_event fired %d times, want 1", len(reports))
		}
		if got := sourceIPOf(reports[0]); got != wantIP {
			t.Fatalf("ok report source_ip = %q, want %q", got, wantIP)
		}
	})
}

// ── Helpers ───────────────────────────────────────────────────────

func newTestSession(c *authCaller) *Session {
	return &Session{client: c}
}

func plainPayload(authzID, username, password string) string {
	return authzID + "\x00" + username + "\x00" + password
}

// oauthBearerPayload builds the GS2-style OAUTHBEARER initial response
// per RFC 7628 § 3.1: `n,a=<authzid>,\x01auth=Bearer <token>\x01\x01`.
func oauthBearerPayload(authzID, token string) string {
	return "n,a=" + authzID + ",\x01auth=Bearer " + token + "\x01\x01"
}

func equalBytes(a, b []byte) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// Sentinel: assert mailfauna error wording surface for the test
// matchers. If UniFFI regen renames the error, fix here so the
// other tests' substring assertions stay specific. Not a behavioral
// test — just documents what "AEAD" / "decode" wording looks like
// at the Go layer today.
func TestErrorWordingSurface(t *testing.T) {
	_, err := mailfauna.UnwrapMLSBlob(
		[]byte("not-cbor"),
		fixturePlainPassword,
		fixtureActorID,
		fixtureCredentialID,
		mailfauna.KdfKindArgon2id,
	)
	if err == nil {
		t.Fatal("malformed blob must error")
	}
	if !strings.Contains(strings.ToLower(err.Error()), "decode") &&
		!strings.Contains(strings.ToLower(err.Error()), "cbor") &&
		!strings.Contains(strings.ToLower(err.Error()), "format") {
		t.Fatalf("unexpected error wording: %v", err)
	}
}

// Failsafe: if a test mis-types something the compiler won't catch
// (e.g. an authCaller method missing), this anchor keeps the test
// imports honest.
var _ wsrpcCaller = (*authCaller)(nil)

// wsrpcCaller mirrors wsrpc.Caller so we can statically assert
// authCaller satisfies it. Defining it here (rather than importing
// wsrpc.Caller into the assertion) keeps the local test surface
// independent of the wsrpc package layout — only the call shape
// matters.
type wsrpcCaller interface {
	Call(ctx context.Context, method string, body, reply any) error
}
