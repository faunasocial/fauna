// Tests for the S3 projection loop (projection.go): a fake nest Caller serves
// the fetch_public_posts / fetch_profile / identity methods off canned data, a
// fake unseal stands in for the FFI HPKE-Open, an httptest server + fake TXT
// resolver play the resolvability gate, and a fake translator stands in for the
// shared-Rust FFI — so the whole roster-sweep → page → project → gate flow runs
// headless. The projection MECHANICS (MST/sign/CAR/idempotency) are covered in
// internal/atprotorepo; these tests lock the LOOP: identity filtering, cursor
// paging + watermark advance, and the first-emit resolvability gate.
package main

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/bluesky-social/indigo/atproto/syntax"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// fakeProjTranslator stands in for the shared-Rust FFI: minimal-but-valid record
// JSON + a real (deterministic) TID clock keyed by post id — the same shape the
// internal/atprotorepo projector tests use.
type fakeProjTranslator struct {
	tids map[string]string
	clk  *syntax.TIDClock
}

func newFakeProjTranslator() *fakeProjTranslator {
	return &fakeProjTranslator{tids: map[string]string{}, clk: syntax.NewTIDClock(0)}
}

func (f *fakeProjTranslator) PostRecord(postBytes []byte, replyJSON, quoteJSON *string, images []atprotorepo.ResolvedImage, video *atprotorepo.ResolvedVideo, selfLabels []string) (string, bool, error) {
	return fmt.Sprintf(`{"$type":%q,"text":%q,"createdAt":"2026-01-01T00:00:00Z","langs":["en"]}`,
		atprotorepo.NsidFeedPost, string(postBytes)), true, nil
}

func (f *fakeProjTranslator) ProfileRecord(profileBytes []byte, avatar, banner *atprotorepo.ResolvedImage) (string, error) {
	return fmt.Sprintf(`{"$type":%q,"displayName":%q}`, atprotorepo.NsidProfile, string(profileBytes)), nil
}

// The loop-level tests carry no profile pictures; the picture path itself is
// covered in internal/atprotorepo, against the blob store it actually writes.
func (f *fakeProjTranslator) ProfileMedia(profileBytes []byte) (avatar, banner *atprotorepo.MediaItem, err error) {
	return nil, nil, nil
}

// The loop-level tests project standalone posts; reference resolution itself is
// covered in internal/atprotorepo, against the store the lookup reads.
func (f *fakeProjTranslator) PostRefs(postBytes []byte) (replyParentPostID, quotePostID string, err error) {
	return "", "", nil
}

// Likewise for media: the byte-fetch/re-hash/store path is covered in
// internal/atprotorepo, against the store and blob source it actually uses.
func (f *fakeProjTranslator) PostMedia(postBytes []byte) ([]atprotorepo.MediaItem, error) {
	return nil, nil
}

// The loop-level tests carry no video either; assembly is covered in
// internal/atprotorepo, against the blob store and ffmpeg it actually uses.
func (f *fakeProjTranslator) PostVideo(postBytes []byte) (*atprotorepo.VideoItem, error) {
	return nil, nil
}

func (f *fakeProjTranslator) DeterministicTID(createdAtMicros int64, postID []byte) string {
	key := hex.EncodeToString(postID)
	if t, ok := f.tids[key]; ok {
		return t
	}
	t := f.clk.Next().String()
	f.tids[key] = t
	return t
}

// fakeProjNest serves the projection loop's four nest methods off canned data,
// paging fetch_public_posts by the (created_at_micros, post_id) cursor.
type fakeProjNest struct {
	mu         sync.Mutex
	identities []wsrpc.AtprotoIdentityView
	posts      []wsrpc.PublicPostItem // oldest-first
	profile    []byte
	blob       []byte

	publicPostsCalls int
	profileCalls     int
}

func (f *fakeProjNest) Call(_ context.Context, method string, body, reply any) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	reencode := func(v any) error {
		b, err := cbor.Marshal(v)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(b, reply)
	}
	decodeBody := func(into any) error {
		b, err := cbor.Marshal(body)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(b, into)
	}
	switch method {
	case wsrpc.MethodAtprotoFetchIdentities:
		return reencode(map[string]any{"identities": f.identities})
	case wsrpc.MethodAtprotoFetchIdentityKeyBlob:
		return reencode(map[string]any{
			"blob":                        f.blob,
			"signing_pub_did_key":         "did:key:zSigning",
			"bridge_rotation_pub_did_key": "did:key:zBridge",
		})
	case wsrpc.MethodAtprotoFetchPublicPosts:
		f.publicPostsCalls++
		var req struct {
			ActorID []byte                   `cbor:"actor_id"`
			Cursor  *wsrpc.PublicPostsCursor `cbor:"cursor"`
			Limit   uint32                   `cbor:"limit"`
		}
		if err := decodeBody(&req); err != nil {
			return err
		}
		items, next := f.pagePosts(req.Cursor, int(req.Limit))
		return reencode(map[string]any{"items": items, "next_cursor": next})
	case wsrpc.MethodAtprotoFetchProfile:
		f.profileCalls++
		var profile any
		if f.profile != nil {
			profile = f.profile
		}
		return reencode(map[string]any{"profile": profile})
	default:
		return fmt.Errorf("fakeProjNest: unexpected method %q", method)
	}
}

// pagePosts returns the items strictly after cursor, capped at limit, with a
// non-nil next_cursor exactly when more items remain beyond the page.
func (f *fakeProjNest) pagePosts(cursor *wsrpc.PublicPostsCursor, limit int) ([]wsrpc.PublicPostItem, *wsrpc.PublicPostsCursor) {
	start := 0
	if cursor != nil {
		for i, it := range f.posts {
			after := it.CreatedAtMicros > cursor.CreatedAtMicros ||
				(it.CreatedAtMicros == cursor.CreatedAtMicros && it.PostID > cursor.PostID)
			if after {
				start = i
				break
			}
			start = i + 1
		}
	}
	end := start + limit
	if end > len(f.posts) {
		end = len(f.posts)
	}
	page := f.posts[start:end]
	var next *wsrpc.PublicPostsCursor
	if end < len(f.posts) && len(page) > 0 {
		last := page[len(page)-1]
		next = &wsrpc.PublicPostsCursor{CreatedAtMicros: last.CreatedAtMicros, PostID: last.PostID}
	}
	return page, next
}

// projFixture wires a real store+funnel+projector (fake translator) to a fake
// nest, with a controllable resolvability gate.
type projFixture struct {
	// expected is every {signing, rotation} published-key pair the unseal was
	// handed — the binding the shared Rust enforces in production.
	expected [][2]string

	nest      *fakeProjNest
	store     *atprotorepo.Store
	deps      *projDeps
	directory *httptest.Server
	resolves  bool // toggles the DID document GET (200 vs 404) for the gate

	// The fake PLC log the rename hook reads and writes: logHandle is the
	// alsoKnownAs the audit log currently reports, submits records every op
	// posted, and auditReads counts log reads (so a test can assert the steady
	// state touches the network zero times).
	mu         sync.Mutex
	logHandle  string
	submits    []*atprotoid.PlcOperation
	auditReads atomic.Int64
}

// testPrevCID is the CID the fake audit log reports for its head op — the value
// a rename must chain `prev` to.
const testPrevCID = "bafyreiprevheadcid"

func newProjFixture(t *testing.T) (fx *projFixture) {
	t.Helper()
	store, err := atprotorepo.Open(":memory:")
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(func() { store.Close() })
	funnel, err := atprotorepo.NewFunnel(context.Background(), store, nil)
	if err != nil {
		t.Fatalf("new funnel: %v", err)
	}
	projector := atprotorepo.NewProjector(store, funnel, newFakeProjTranslator(), nil, nil)

	fx = &projFixture{store: store, resolves: true, logHandle: "alice.example.com"}
	fx.directory = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// POST /{did} — an operation submit. Record it and, since the directory
		// is the rename path's source of truth, let it become the new log head.
		if r.Method == http.MethodPost {
			body, _ := io.ReadAll(r.Body)
			var op atprotoid.PlcOperation
			if err := json.Unmarshal(body, &op); err != nil {
				http.Error(w, "bad op", http.StatusBadRequest)
				return
			}
			fx.mu.Lock()
			fx.submits = append(fx.submits, &op)
			fx.logHandle = atprotoid.PrimaryHandle(&op)
			fx.mu.Unlock()
			w.WriteHeader(http.StatusOK)
			return
		}
		// GET /{did}/log/audit — the standing head of the PLC log.
		if strings.HasSuffix(r.URL.Path, "/log/audit") {
			fx.mu.Lock()
			handle := fx.logHandle
			fx.mu.Unlock()
			fx.auditReads.Add(1)
			w.Header().Set("Content-Type", "application/json")
			fmt.Fprintf(w, `[{"cid":%q,"nullified":false,"operation":{
				"type":"plc_operation",
				"rotationKeys":["did:key:zUserSenior",%q],
				"verificationMethods":{"atproto":"did:key:zSign"},
				"alsoKnownAs":["at://%s"],
				"services":{"atproto_pds":{"type":"AtprotoPersonalDataServer","endpoint":"https://example.com"}},
				"prev":null,"sig":"s"}}]`,
				testPrevCID, didKeyForScalar(t, testScalar("proj-rotation")), handle)
			return
		}
		// GET /{did} — the resolvability gate's DID-document probe.
		if fx.resolves {
			w.WriteHeader(http.StatusOK)
			return
		}
		http.Error(w, "not found", http.StatusNotFound)
	}))
	t.Cleanup(fx.directory.Close)

	fx.nest = &fakeProjNest{blob: []byte("sealed-identity-blob")}
	fx.deps = &projDeps{
		store:        store,
		projector:    projector,
		funnel:       funnel,
		x25519Secret: bytes.Repeat([]byte{0x42}, 32),
		unseal: func(blobBytes, secret []byte, wantSigning, wantRotation string) (*mailfauna.AtprotoIdentityKeyBundle, error) {
			fx.expected = append(fx.expected, [2]string{wantSigning, wantRotation})
			return &mailfauna.AtprotoIdentityKeyBundle{
				SigningPriv:       testScalar("proj-signing"),
				SigningCurve:      "k256",
				RotationPriv:      testScalar("proj-rotation"),
				RotationCurve:     "k256",
				RotationPubDidKey: didKeyForScalar(t, testScalar("proj-rotation")),
			}, nil
		},
		directoryBaseURL: fx.directory.URL,
		httpClient:       fx.directory.Client(),
		resolver:         &fakeTXTResolver{records: map[string][]string{}},
		pageLimit:        50,
	}
	return fx
}

// activePlc returns an active did:plc identity whose _atproto TXT resolves (so
// the gate depends only on fx.resolves toggling the directory GET).
func (fx *projFixture) activePlc(handle, did string, actorSeed byte) wsrpc.AtprotoIdentityView {
	fx.deps.resolver.(*fakeTXTResolver).records["_atproto."+handle] = []string{"did=" + did}
	d := did
	return wsrpc.AtprotoIdentityView{
		ActorID: bytes.Repeat([]byte{actorSeed}, 32),
		Handle:  handle,
		Method:  "plc",
		Status:  "active",
		DID:     &d,
	}
}

func postItem(seed byte, micros int64, text string) wsrpc.PublicPostItem {
	return wsrpc.PublicPostItem{
		PostID:          hex.EncodeToString(bytes.Repeat([]byte{seed}, 32)),
		CreatedAtMicros: micros,
		Kind:            wsrpc.PublicPostsItemKindPost,
		Payload:         []byte(text),
	}
}

// TestProjectionPassProjectsActiveIdentity: an active identity's posts + profile
// land in a real repo, the watermark advances to the last post, and the
// first-emit gate clears.
func TestProjectionPassProjectsActiveIdentity(t *testing.T) {
	fx := newProjFixture(t)
	did := "did:plc:alice000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("alice.example.com", did, 0x11)}
	fx.nest.posts = []wsrpc.PublicPostItem{
		postItem(0xa1, 1000, "first"),
		postItem(0xa2, 2000, "second"),
	}
	fx.nest.profile = []byte("Alice")

	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())

	ctx := context.Background()
	if _, _, ok, err := fx.store.Head(ctx, did); err != nil || !ok {
		t.Fatalf("no repo head after projection (ok=%v err=%v)", ok, err)
	}
	// Every unseal on the signing path was bound to the fetch reply's published
	// keys — the expectation the shared Rust refuses a substituted blob against.
	if len(fx.expected) == 0 {
		t.Fatal("the projection pass never unsealed a signer")
	}
	for _, got := range fx.expected {
		if got != [2]string{"did:key:zSigning", "did:key:zBridge"} {
			t.Errorf("unseal expectation = %v, want the fetch reply's published keys", got)
		}
	}
	// Both posts mapped.
	for _, seed := range []byte{0xa1, 0xa2} {
		if _, _, ok, err := fx.store.PostAtURI(ctx, did, hex.EncodeToString(bytes.Repeat([]byte{seed}, 32))); err != nil || !ok {
			t.Errorf("post %x not projected (ok=%v err=%v)", seed, ok, err)
		}
	}
	// Watermark at the last post; gate cleared.
	state, err := fx.store.ProjectionState(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	if state.LastCreatedAtMicros != 2000 || state.LastPostID != hex.EncodeToString(bytes.Repeat([]byte{0xa2}, 32)) {
		t.Errorf("watermark = (%d,%s), want (2000, ...a2)", state.LastCreatedAtMicros, state.LastPostID)
	}
	if state.FirstEmitGated {
		t.Error("first-emit gate not cleared after emit")
	}
}

// TestProjectionPassSkipsPendingAndDidless: a pending row and an active-but-nil-DID
// row are both left untouched (the mint loop owns them).
func TestProjectionPassSkipsPendingAndDidless(t *testing.T) {
	fx := newProjFixture(t)
	pending := fx.activePlc("bob.example.com", "did:plc:bob0000000000000000000000000", 0x22)
	pending.Status = "pending"
	didless := fx.activePlc("carol.example.com", "did:plc:carol00000000000000000000000", 0x33)
	didless.DID = nil
	fx.nest.identities = []wsrpc.AtprotoIdentityView{pending, didless}
	fx.nest.posts = []wsrpc.PublicPostItem{postItem(0xb1, 1000, "hi")}

	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if fx.nest.publicPostsCalls != 0 {
		t.Errorf("fetch_public_posts called %d times for skipped identities, want 0", fx.nest.publicPostsCalls)
	}
}

// accountFrames counts #account outbox rows for did.
func accountFrames(t *testing.T, store *atprotorepo.Store, did string) int {
	t.Helper()
	var n int
	if err := store.DB().QueryRow(
		`SELECT COUNT(*) FROM firehose_events WHERE did = ? AND frame_type = ?`,
		did, atprotorepo.FrameAccount).Scan(&n); err != nil {
		t.Fatal(err)
	}
	return n
}

// TestReconcileAccountStatusEmitsOnBothTransitions is the S4-D loop proof: a
// step-down emits exactly one #account(inactive) and stops projecting; a repeat
// pass emits nothing more (idempotent, keyed to repo_heads.active); re-entry
// emits #account(active) and serving resumes. The initial active projection
// emits NO #account — an account's first activeness is implicit in its #commits.
func TestReconcileAccountStatusEmitsOnBothTransitions(t *testing.T) {
	fx := newProjFixture(t)
	// handle == the fake directory's default log handle, so the rename hook is a
	// no-op and cannot add #identity noise to the reconcile flow.
	did := "did:plc:frank0000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("alice.example.com", did, 0x66)}
	fx.nest.posts = []wsrpc.PublicPostItem{postItem(0xf1, 1000, "hi")}
	ctx := context.Background()

	// Pass 1 (active): projects; head created active; no #account.
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	if n := accountFrames(t, fx.store, did); n != 0 {
		t.Fatalf("#account frames after first active pass = %d, want 0", n)
	}
	if active, _, _ := fx.store.RepoActive(ctx, did); !active {
		t.Fatal("repo not active after projection")
	}

	// Step down: roster says deactivated → one #account(inactive), flag flipped,
	// projection skipped.
	fx.nest.identities[0].Status = "deactivated"
	postsBefore := fx.nest.publicPostsCalls
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	if n := accountFrames(t, fx.store, did); n != 1 {
		t.Fatalf("#account frames after step-down = %d, want 1", n)
	}
	if active, _, _ := fx.store.RepoActive(ctx, did); active {
		t.Error("repo still active after step-down")
	}
	if fx.nest.publicPostsCalls != postsBefore {
		t.Errorf("fetch_public_posts ran for a deactivated identity (%d -> %d)", postsBefore, fx.nest.publicPostsCalls)
	}

	// Idempotent: a second deactivated pass emits no further #account.
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	if n := accountFrames(t, fx.store, did); n != 1 {
		t.Fatalf("#account frames after a second deactivated pass = %d, want 1 (idempotent)", n)
	}

	// Re-entry: roster active again → #account(active) + serving resumes.
	fx.nest.identities[0].Status = "active"
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	if n := accountFrames(t, fx.store, did); n != 2 {
		t.Fatalf("#account frames after re-entry = %d, want 2", n)
	}
	if active, _, _ := fx.store.RepoActive(ctx, did); !active {
		t.Error("repo not active after re-entry")
	}
}

// TestProjectionFirstEmitGate: an unresolvable identity projects NOTHING and
// keeps its gate; once it resolves, the same pass projects and clears the gate.
func TestProjectionFirstEmitGate(t *testing.T) {
	fx := newProjFixture(t)
	did := "did:plc:dave00000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("dave.example.com", did, 0x44)}
	fx.nest.posts = []wsrpc.PublicPostItem{postItem(0xd1, 1000, "gated")}
	ctx := context.Background()

	// Gate closed: directory 404 → not resolvable → defer.
	fx.resolves = false
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	if _, _, ok, _ := fx.store.Head(ctx, did); ok {
		t.Fatal("gated identity was projected while unresolvable")
	}
	state, _ := fx.store.ProjectionState(ctx, did)
	if !state.FirstEmitGated {
		t.Error("gate cleared despite unresolvable identity")
	}

	// Now resolvable: projects + clears the gate.
	fx.resolves = true
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	if _, _, ok, _ := fx.store.Head(ctx, did); !ok {
		t.Fatal("resolvable identity still not projected")
	}
	state, _ = fx.store.ProjectionState(ctx, did)
	if state.FirstEmitGated {
		t.Error("gate not cleared after first emit")
	}
}

// TestProjectionPassIdempotent: a second identical pass produces no new commits
// (post_map skip) — the head is unchanged.
func TestProjectionPassIdempotent(t *testing.T) {
	fx := newProjFixture(t)
	did := "did:plc:erin00000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("erin.example.com", did, 0x55)}
	fx.nest.posts = []wsrpc.PublicPostItem{postItem(0xe1, 1000, "once")}
	ctx := context.Background()

	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	_, head1, ok, _ := fx.store.Head(ctx, did)
	if !ok {
		t.Fatal("no head after first pass")
	}
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	_, head2, _, _ := fx.store.Head(ctx, did)
	if head1 != head2 {
		t.Errorf("second pass changed the head (%s -> %s); projection is not idempotent", head1, head2)
	}
}

// ── Rename hook (task 8 / D-s3-6) ────────────────────────────────────────────

// identityFrames returns the #identity frames sitting in the outbox, with the
// handle each announces, read back through the store the broadcaster reads.
func identityFrames(t *testing.T, st *atprotorepo.Store) []atprotorepo.FirehoseEvent {
	t.Helper()
	all, err := st.EventsSince(context.Background(), 0, 512)
	if err != nil {
		t.Fatalf("EventsSince: %v", err)
	}
	var out []atprotorepo.FirehoseEvent
	for _, e := range all {
		if e.FrameType == atprotorepo.FrameIdentity {
			out = append(out, e)
		}
	}
	return out
}

// renameFixture puts one active did:plc identity in the store already published
// under `published`, then presents it to the loop under `current`.
func renameFixture(t *testing.T, published, current string) (*projFixture, wsrpc.AtprotoIdentityView, string) {
	t.Helper()
	fx := newProjFixture(t)
	did := "did:plc:renametest"
	id := fx.activePlc(current, did, 0x11)
	fx.nest.identities = []wsrpc.AtprotoIdentityView{id}
	ctx := context.Background()
	if published != "" {
		if err := fx.store.SetPublishedHandle(ctx, did, published); err != nil {
			t.Fatal(err)
		}
	}
	// Past the first-emit gate: this identity is already live.
	if err := fx.store.SetFirstEmitGated(ctx, did, false); err != nil {
		t.Fatal(err)
	}
	return fx, id, did
}

// runRename runs the hook once against the fixture's identity.
func runRename(t *testing.T, fx *projFixture, id wsrpc.AtprotoIdentityView, did string) {
	t.Helper()
	ctx := context.Background()
	state, err := fx.store.ProjectionState(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	if err := reconcileHandle(ctx, fx.nest, fx.deps, id, state, discardLogger()); err != nil {
		t.Fatalf("reconcileHandle: %v", err)
	}
}

// TestRenameHookSubmitsUpdateOpAndEmitsIdentity is the task-8 happy path: a
// changed Fauna handle republishes alsoKnownAs at the directory (chained to the
// log head, signed with the bridge's junior rotation key) and announces itself
// on the firehose, so the network re-resolves.
func TestRenameHookSubmitsUpdateOpAndEmitsIdentity(t *testing.T) {
	fx, id, did := renameFixture(t, "alice.example.com", "renamed.example.com")

	runRename(t, fx, id, did)

	fx.mu.Lock()
	submits := fx.submits
	fx.mu.Unlock()
	if len(submits) != 1 {
		t.Fatalf("submitted %d ops, want exactly 1", len(submits))
	}
	op := submits[0]
	if got := atprotoid.PrimaryHandle(op); got != "renamed.example.com" {
		t.Errorf("submitted alsoKnownAs = %q, want the new handle", got)
	}
	if op.Prev == nil || *op.Prev != testPrevCID {
		t.Errorf("submitted prev = %v, want the log head CID %q", op.Prev, testPrevCID)
	}
	if op.RotationKeys[0] != "did:key:zUserSenior" {
		t.Errorf("rotationKeys[0] = %q, want the user's senior key carried forward", op.RotationKeys[0])
	}
	if op.VerificationMethods["atproto"] != "did:key:zSign" {
		t.Errorf("verificationMethods carried forward wrong: %v", op.VerificationMethods)
	}
	if op.Sig == "" {
		t.Error("submitted op is unsigned")
	}

	frames := identityFrames(t, fx.store)
	if len(frames) != 1 {
		t.Fatalf("emitted %d #identity frames, want 1", len(frames))
	}
	if frames[0].DID != did {
		t.Errorf("#identity did = %q, want %q", frames[0].DID, did)
	}

	state, _ := fx.store.ProjectionState(context.Background(), did)
	if state.PublishedHandle != "renamed.example.com" {
		t.Errorf("published_handle = %q, want the new handle", state.PublishedHandle)
	}

	// Idempotent: a second pass with nothing changed is a pure no-op.
	before := fx.auditReads.Load()
	runRename(t, fx, id, did)
	if n := len(identityFrames(t, fx.store)); n != 1 {
		t.Errorf("second pass emitted another frame (%d total)", n)
	}
	if fx.auditReads.Load() != before {
		t.Error("steady state read the PLC log; the handle comparison must short-circuit first")
	}
}

// TestRenameHookAdoptsSilentlyOnFirstObservation: an identity this bridge has
// never recorded a published handle for (fresh mint, or a wiped store) converges
// WITHOUT announcing a rename — there is no evidence one happened, and the
// genesis op already carries the handle.
func TestRenameHookAdoptsSilentlyOnFirstObservation(t *testing.T) {
	fx, id, did := renameFixture(t, "", "alice.example.com")

	runRename(t, fx, id, did)

	fx.mu.Lock()
	n := len(fx.submits)
	fx.mu.Unlock()
	if n != 0 {
		t.Errorf("submitted %d ops, want 0 — the directory already agrees", n)
	}
	if f := identityFrames(t, fx.store); len(f) != 0 {
		t.Errorf("emitted %d #identity frames on first observation, want 0", len(f))
	}
	state, _ := fx.store.ProjectionState(context.Background(), did)
	if state.PublishedHandle != "alice.example.com" {
		t.Errorf("published_handle = %q, want the adopted handle", state.PublishedHandle)
	}
}

// TestRenameHookEmitsAfterCrashBetweenSubmitAndFrame: the directory already has
// the new handle but published_handle is still the old one — the signature of a
// crash after the submit. The op must NOT be re-submitted, and the owed
// #identity must still go out.
func TestRenameHookEmitsAfterCrashBetweenSubmitAndFrame(t *testing.T) {
	fx, id, did := renameFixture(t, "alice.example.com", "renamed.example.com")
	fx.mu.Lock()
	fx.logHandle = "renamed.example.com" // the submit landed before the crash
	fx.mu.Unlock()

	runRename(t, fx, id, did)

	fx.mu.Lock()
	n := len(fx.submits)
	fx.mu.Unlock()
	if n != 0 {
		t.Errorf("re-submitted %d ops after a crash; the directory already agreed", n)
	}
	if f := identityFrames(t, fx.store); len(f) != 1 {
		t.Fatalf("emitted %d #identity frames, want the owed 1", len(f))
	}
	state, _ := fx.store.ProjectionState(context.Background(), did)
	if state.PublishedHandle != "renamed.example.com" {
		t.Errorf("published_handle = %q", state.PublishedHandle)
	}
}

// TestRenameHookDefersUntilNewHandleVerifies: the DID document must never be
// moved onto a handle that does not verify yet (`_atproto.<new>` unpublished).
// Defer instead — the old document keeps resolving meanwhile.
func TestRenameHookDefersUntilNewHandleVerifies(t *testing.T) {
	fx, id, did := renameFixture(t, "alice.example.com", "renamed.example.com")
	// activePlc published the TXT for the new handle; take it away again.
	delete(fx.deps.resolver.(*fakeTXTResolver).records, "_atproto.renamed.example.com")

	runRename(t, fx, id, did)

	fx.mu.Lock()
	n := len(fx.submits)
	fx.mu.Unlock()
	if n != 0 {
		t.Errorf("submitted %d ops for an unverifiable handle, want 0", n)
	}
	if f := identityFrames(t, fx.store); len(f) != 0 {
		t.Errorf("emitted %d #identity frames for an unverifiable handle, want 0", len(f))
	}
	state, _ := fx.store.ProjectionState(context.Background(), did)
	if state.PublishedHandle != "alice.example.com" {
		t.Errorf("published_handle = %q, want the OLD handle kept so the next pass retries", state.PublishedHandle)
	}

	// Once the TXT lands, the same pass shape completes the rename.
	fx.deps.resolver.(*fakeTXTResolver).records["_atproto.renamed.example.com"] = []string{"did=" + did}
	runRename(t, fx, id, did)
	fx.mu.Lock()
	n = len(fx.submits)
	fx.mu.Unlock()
	if n != 1 {
		t.Errorf("submitted %d ops after the TXT landed, want 1", n)
	}
	if f := identityFrames(t, fx.store); len(f) != 1 {
		t.Errorf("emitted %d #identity frames after the TXT landed, want 1", len(f))
	}
}

// TestRenameHookDidWebCannotFollow: did:web's DID IS its handle, so a rename is
// a different identity — surface the limit, publish nothing, announce nothing,
// and leave the divergence observable (atproto-pds-bridge.md § Identity).
func TestRenameHookDidWebCannotFollow(t *testing.T) {
	fx, id, did := renameFixture(t, "alice.example.com", "renamed.example.com")
	id.Method = "web"

	runRename(t, fx, id, did)

	fx.mu.Lock()
	n := len(fx.submits)
	fx.mu.Unlock()
	if n != 0 {
		t.Errorf("submitted %d ops for a did:web identity, want 0", n)
	}
	if f := identityFrames(t, fx.store); len(f) != 0 {
		t.Errorf("emitted %d #identity frames for a did:web identity, want 0", len(f))
	}
	if fx.auditReads.Load() != 0 {
		t.Error("did:web reconcile touched the PLC directory")
	}
	state, _ := fx.store.ProjectionState(context.Background(), did)
	if state.PublishedHandle != "alice.example.com" {
		t.Errorf("published_handle = %q, want the divergence left observable", state.PublishedHandle)
	}
}

// TestProjectionPassRunsTheRenameHook wires the hook to the loop: a full pass
// over a renamed identity both republishes the identity and keeps projecting
// posts — a stale handle must never stall the repo.
func TestProjectionPassRunsTheRenameHook(t *testing.T) {
	fx, _, did := renameFixture(t, "alice.example.com", "renamed.example.com")
	fx.nest.posts = []wsrpc.PublicPostItem{postItem(1, 1000, "after the rename")}

	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if f := identityFrames(t, fx.store); len(f) != 1 {
		t.Fatalf("pass emitted %d #identity frames, want 1", len(f))
	}
	if _, _, ok, err := fx.store.PostAtURI(context.Background(), did, fx.nest.posts[0].PostID); err != nil || !ok {
		t.Errorf("the pass stopped projecting posts across a rename (ok=%v err=%v)", ok, err)
	}
}

// ── S5 slice 4: downtime catch-up / #sync collapse ───────────────────────────

// framesOfType counts outbox rows of one frame type for did.
func framesOfType(t *testing.T, store *atprotorepo.Store, did, frameType string) int {
	t.Helper()
	var n int
	if err := store.DB().QueryRow(
		`SELECT COUNT(*) FROM firehose_events WHERE did = ? AND frame_type = ?`,
		did, frameType).Scan(&n); err != nil {
		t.Fatal(err)
	}
	return n
}

// TestSmallGapReplaysAsCommits is the watermark table's row-3 FIRST clause: an
// ordinary downtime catch-up — one that fits in a single fetch page — replays
// as individual #commits, exactly as before this slice. The collapse must not
// fire on the common case, because a #sync costs the relay a whole getRepo.
func TestSmallGapReplaysAsCommits(t *testing.T) {
	fx := newProjFixture(t)
	fx.deps.pageLimit = 10
	did := "did:plc:alice000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("alice.example.com", did, 0x11)}
	fx.nest.posts = []wsrpc.PublicPostItem{
		postItem(0xa1, 1000, "one"),
		postItem(0xa2, 2000, "two"),
		postItem(0xa3, 3000, "three"),
	}

	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if n := framesOfType(t, fx.store, did, atprotorepo.FrameCommit); n != 3 {
		t.Errorf("#commit frames = %d, want 3 (one per post — a small gap replays)", n)
	}
	if n := framesOfType(t, fx.store, did, atprotorepo.FrameSync); n != 0 {
		t.Errorf("#sync frames = %d, want 0 — a small gap must not degrade the relay", n)
	}
	if owed, err := fx.store.SyncOwed(context.Background(), did); err != nil || owed {
		t.Errorf("small gap left a #sync owed (owed=%v err=%v)", owed, err)
	}
}

// TestHugeGapCollapsesToOneSync is the row-3 SECOND clause: a backlog that does
// not fit in one fetch page is a huge gap, so every post still lands in the repo
// but the network is told once, with a #sync, and refetches with getRepo. The
// decision is made from the first page's next_cursor — BEFORE any frame is
// emitted — so a collapsing pass never emits a #commit it is about to make
// redundant.
func TestHugeGapCollapsesToOneSync(t *testing.T) {
	fx := newProjFixture(t)
	fx.deps.pageLimit = 2 // 5 posts => 3 pages: a "huge" gap at test scale
	did := "did:plc:alice000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("alice.example.com", did, 0x11)}
	fx.nest.posts = []wsrpc.PublicPostItem{
		postItem(0xa1, 1000, "one"),
		postItem(0xa2, 2000, "two"),
		postItem(0xa3, 3000, "three"),
		postItem(0xa4, 4000, "four"),
		postItem(0xa5, 5000, "five"),
	}
	fx.nest.profile = []byte("Alice")

	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())

	ctx := context.Background()
	// Every post is in the repo — the collapse withholds frames, never content.
	for _, seed := range []byte{0xa1, 0xa2, 0xa3, 0xa4, 0xa5} {
		if _, _, ok, err := fx.store.PostAtURI(ctx, did, hex.EncodeToString(bytes.Repeat([]byte{seed}, 32))); err != nil || !ok {
			t.Errorf("post %x missing from the repo after a collapsed catch-up (ok=%v err=%v)", seed, ok, err)
		}
	}
	if n := framesOfType(t, fx.store, did, atprotorepo.FrameCommit); n != 0 {
		t.Errorf("#commit frames = %d, want 0 — the whole gap collapses", n)
	}
	if n := framesOfType(t, fx.store, did, atprotorepo.FrameSync); n != 1 {
		t.Errorf("#sync frames = %d, want exactly 1", n)
	}
	if owed, err := fx.store.SyncOwed(ctx, did); err != nil || owed {
		t.Errorf("#sync debt not cleared after the collapse (owed=%v err=%v)", owed, err)
	}
	// The watermark still advanced, so the next pass is a no-op.
	state, err := fx.store.ProjectionState(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	if state.LastCreatedAtMicros != 5000 {
		t.Errorf("watermark = %d, want 5000", state.LastCreatedAtMicros)
	}
}

// TestCollapseIsIdempotentAcrossPasses: once the gap is drained a second pass
// must be silent — no second #sync, no re-projection.
func TestCollapseIsIdempotentAcrossPasses(t *testing.T) {
	fx := newProjFixture(t)
	fx.deps.pageLimit = 2
	did := "did:plc:alice000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("alice.example.com", did, 0x11)}
	fx.nest.posts = []wsrpc.PublicPostItem{
		postItem(0xa1, 1000, "one"),
		postItem(0xa2, 2000, "two"),
		postItem(0xa3, 3000, "three"),
	}

	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())
	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if n := framesOfType(t, fx.store, did, atprotorepo.FrameSync); n != 1 {
		t.Errorf("#sync frames after two passes = %d, want 1", n)
	}
	if n := framesOfType(t, fx.store, did, atprotorepo.FrameCommit); n != 0 {
		t.Errorf("#commit frames after two passes = %d, want 0", n)
	}
}

// TestCrashMidCollapseStillAnnouncesTheHead is the crash-discipline proof, and
// the reason the debt is persisted rather than held in the pass's local state.
// A pass that deferred commits and died before its #sync leaves the repo ahead
// of what the network was told — and the next ordinary #commit would then carry
// a prevData no relay has. A standing debt therefore FORCES collapse mode for
// the whole next pass and pays itself off, however small the remaining gap.
func TestCrashMidCollapseStillAnnouncesTheHead(t *testing.T) {
	fx := newProjFixture(t)
	fx.deps.pageLimit = 10 // a gap that would NOT collapse on its own
	did := "did:plc:alice000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("alice.example.com", did, 0x11)}
	fx.nest.posts = []wsrpc.PublicPostItem{postItem(0xa1, 1000, "one")}

	ctx := context.Background()
	// Simulate the crashed pass: a repo whose head moved with frames deferred.
	fx.seedDeferredHead(t, did)
	if owed, err := fx.store.SyncOwed(ctx, did); err != nil || !owed {
		t.Fatalf("fixture did not leave a standing #sync debt (owed=%v err=%v)", owed, err)
	}

	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())

	if n := framesOfType(t, fx.store, did, atprotorepo.FrameCommit); n != 0 {
		t.Errorf("#commit frames = %d, want 0 — a standing debt forces collapse mode", n)
	}
	if n := framesOfType(t, fx.store, did, atprotorepo.FrameSync); n != 1 {
		t.Errorf("#sync frames = %d, want 1 — the owed announcement must be paid", n)
	}
	if owed, err := fx.store.SyncOwed(ctx, did); err != nil || owed {
		t.Errorf("debt still standing after the recovery pass (owed=%v err=%v)", owed, err)
	}
}

// seedDeferredHead applies one commit with its frame deferred, leaving exactly
// the state a pass that crashed mid-collapse leaves behind: a repo head the
// network has never been told about, and a standing #sync debt.
func (fx *projFixture) seedDeferredHead(t *testing.T, did string) {
	t.Helper()
	ctx := context.Background()
	bundle, err := fx.deps.unseal(nil, nil, "", "")
	if err != nil {
		t.Fatalf("unseal test signer: %v", err)
	}
	signer, err := atprotoid.PrivateKeyFromK256Scalar(bundle.SigningPriv)
	if err != nil {
		t.Fatalf("signer from scalar: %v", err)
	}
	rec, err := atprotorepo.JSONRecordToDagCBOR(
		`{"$type":"app.bsky.feed.post","text":"pre-crash","createdAt":"2026-01-01T00:00:00Z"}`)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := fx.deps.funnel.ApplyBatch(ctx, did, signer, []atprotorepo.RepoOp{{
		Action: atprotorepo.ActionCreate, Collection: "app.bsky.feed.post",
		Rkey: "3lpreecrash000", RecordCBOR: rec, FaunaPostID: "precrash",
	}}, atprotorepo.DeferFrameToSync()); err != nil {
		t.Fatalf("seed deferred commit: %v", err)
	}
}

// TestSelfCheckIsEmptyNotFailedBeforeTheFirstCommit: the pass that OPENS the
// first-emit gate reaches the signature self-check with an EMPTY repo by design
// — the gate clears before the first commit, so there is nothing to verify yet.
// VerifyRepo answers "no repo for did" there, and reporting that as a FAILED
// self-check is a false alarm reading exactly like the slice-4c DID-convergence
// symptom (two halves writing different repos), which is the one thing this log
// line exists to make unmissable.
//
// The second half is the load-bearing one: the empty pass must NOT consume the
// once-per-DID slot, or the real check would never run on the pass that finally
// has a head to check.
func TestSelfCheckIsEmptyNotFailedBeforeTheFirstCommit(t *testing.T) {
	fx := newProjFixture(t)
	did := "did:plc:norepo0000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("norepo.example.com", did, 0x66)}
	fx.nest.posts = nil // nothing to project, so the repo stays empty
	fx.resolves = true
	ctx := context.Background()

	var buf bytes.Buffer
	logger := slog.New(slog.NewJSONHandler(&buf, nil))
	runProjectionPass(ctx, fx.nest, fx.deps, logger)

	if _, _, ok, _ := fx.store.Head(ctx, did); ok {
		t.Fatal("fixture is vacuous: this test needs a pass that leaves the repo empty")
	}
	if strings.Contains(buf.String(), "self-check FAILED") {
		t.Errorf("an account with no repo yet reported a FAILED self-check:\n%s", buf.String())
	}
	if _, marked := fx.deps.selfChecked.Load(did); marked {
		t.Error("the empty pass consumed the once-per-DID self-check slot, so the " +
			"real signature check would never run for this account")
	}

	// Control, in two passes because the self-check reads the repo AS FOUND at
	// the top of a pass: the pass that first commits still sees an empty repo,
	// so the check runs on the pass after it. That is the behaviour worth
	// pinning — the slot survives every empty pass and is spent on the first one
	// with a head.
	fx.nest.posts = []wsrpc.PublicPostItem{postItem(0x67, 1000, "first post")}
	buf.Reset()
	runProjectionPass(ctx, fx.nest, fx.deps, logger)
	if _, _, ok, _ := fx.store.Head(ctx, did); !ok {
		t.Fatal("control pass did not project, so the self-check had nothing to run on")
	}
	if _, marked := fx.deps.selfChecked.Load(did); marked {
		t.Error("the committing pass spent the slot on the repo it found EMPTY at its top")
	}

	buf.Reset()
	runProjectionPass(ctx, fx.nest, fx.deps, logger)
	if _, marked := fx.deps.selfChecked.Load(did); !marked {
		t.Error("the self-check never ran on the pass that had a head to check")
	}
	if strings.Contains(buf.String(), "self-check FAILED") {
		t.Errorf("a freshly committed repo must verify under its own signing key:\n%s", buf.String())
	}
}
