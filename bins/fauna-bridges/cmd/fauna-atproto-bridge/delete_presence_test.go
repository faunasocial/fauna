package main

// Loop-level proofs for the delete-presence sweep (S5 slice 5,
// `atproto-pds-bridge.md` § Disable & revocation layer 2). The store-level
// primitives are pinned in internal/atprotorepo; what is pinned here is the
// ORDER — deletes announced as real commits while the repo is still served,
// then one terminal #account(deleted), then the purge — and its convergence
// after a crash at each step.

import (
	"bytes"
	"context"
	"encoding/hex"
	"testing"

	comatproto "github.com/bluesky-social/indigo/api/atproto"
	"github.com/bluesky-social/indigo/events"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// deletedPlc is activePlc with the roster status the sweep converges on.
func (fx *projFixture) deletedPlc(handle, did string, actorSeed byte) wsrpc.AtprotoIdentityView {
	id := fx.activePlc(handle, did, actorSeed)
	id.Status = wsrpc.IdentityStatusDeleted
	return id
}

// frameHeaderType reads a persisted frame's INDIGO header and returns its
// message type, leaving the reader positioned at the body — so every assertion
// below is made against what a relay would actually decode off the wire, not
// against our own `frame_type` bookkeeping column.
func frameHeaderType(t *testing.T, r *bytes.Reader) string {
	t.Helper()
	var hdr events.EventHeader
	if err := hdr.UnmarshalCBOR(r); err != nil {
		t.Fatalf("indigo could not decode our frame header: %v", err)
	}
	return hdr.MsgType
}

// commitFrames decodes did's #commit outbox rows through indigo's own decoders
// and returns them in seq order.
func commitFrames(t *testing.T, store *atprotorepo.Store, did string) []*comatproto.SyncSubscribeRepos_Commit {
	t.Helper()
	evs, err := store.EventsSince(context.Background(), 0, 1000)
	if err != nil {
		t.Fatal(err)
	}
	var out []*comatproto.SyncSubscribeRepos_Commit
	for _, ev := range evs {
		if ev.DID != did || ev.FrameType != atprotorepo.FrameCommit {
			continue
		}
		r := bytes.NewReader(ev.Payload)
		if mt := frameHeaderType(t, r); mt != "#commit" {
			t.Fatalf("frame %d has header type %q, want #commit", ev.Seq, mt)
		}
		var evt comatproto.SyncSubscribeRepos_Commit
		if err := evt.UnmarshalCBOR(r); err != nil {
			t.Fatalf("indigo could not decode our #commit body: %v", err)
		}
		out = append(out, &evt)
	}
	return out
}

// accountFrames decodes every #account frame indigo can read for did.
func decodedAccountFrames(t *testing.T, store *atprotorepo.Store, did string) []*comatproto.SyncSubscribeRepos_Account {
	t.Helper()
	evs, err := store.EventsSince(context.Background(), 0, 1000)
	if err != nil {
		t.Fatal(err)
	}
	var out []*comatproto.SyncSubscribeRepos_Account
	for _, ev := range evs {
		if ev.DID != did || ev.FrameType != atprotorepo.FrameAccount {
			continue
		}
		r := bytes.NewReader(ev.Payload)
		if mt := frameHeaderType(t, r); mt != "#account" {
			t.Fatalf("frame %d has header type %q, want #account", ev.Seq, mt)
		}
		var evt comatproto.SyncSubscribeRepos_Account
		if err := evt.UnmarshalCBOR(r); err != nil {
			t.Fatalf("indigo could not decode our #account body: %v", err)
		}
		out = append(out, &evt)
	}
	return out
}

// accountFrameStatus decodes did's single #account frame and returns its active
// flag + status string.
func accountFrameStatus(t *testing.T, store *atprotorepo.Store, did string) (bool, string) {
	t.Helper()
	frames := decodedAccountFrames(t, store, did)
	if len(frames) != 1 {
		t.Fatalf("%d #account frames emitted for the sweep, want exactly 1", len(frames))
	}
	status := ""
	if frames[0].Status != nil {
		status = *frames[0].Status
	}
	return frames[0].Active, status
}

// projectThenDelete projects a repo for did, then runs one pass with the roster
// reporting the identity deleted.
func projectThenDelete(t *testing.T, fx *projFixture, handle, did string, seed byte, posts []wsrpc.PublicPostItem) {
	t.Helper()
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc(handle, did, seed)}
	fx.nest.posts = posts
	fx.nest.profile = []byte("Alice")
	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())
	if _, _, ok, err := fx.store.Head(context.Background(), did); err != nil || !ok {
		t.Fatalf("precondition: no repo to delete (ok=%v err=%v)", ok, err)
	}

	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.deletedPlc(handle, did, seed)}
	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())
}

// TestDeleteSweepTombstonesEveryRecordThenAnnouncesThenPurges is the slice's
// headline. What matters is not only that the repo ends up gone, but that the
// network was told HOW: real delete ops in a real #commit, which is the only
// part of a deletion that propagates to relays and AppViews. A sweep that
// unserved first and purged quietly would leave every consumer still holding
// the records with nothing to tell them otherwise.
func TestDeleteSweepTombstonesEveryRecordThenAnnouncesThenPurges(t *testing.T) {
	fx := newProjFixture(t)
	ctx := context.Background()
	did := "did:plc:doomed00000000000000000000"
	posts := []wsrpc.PublicPostItem{
		postItem(0xd1, 1000, "first"),
		postItem(0xd2, 2000, "second"),
	}

	projectThenDelete(t, fx, "alice.example.com", did, 0x44, posts)

	// The last commit carries delete ops for every record that existed.
	commits := commitFrames(t, fx.store, did)
	if len(commits) == 0 {
		t.Fatal("no #commit frames at all")
	}
	last := commits[len(commits)-1]
	deleted := map[string]bool{}
	for _, op := range last.Ops {
		if op.Action != atprotorepo.ActionDelete {
			t.Errorf("final commit carries a %q op, want only deletes", op.Action)
		}
		deleted[op.Path] = true
	}
	if len(deleted) < 3 {
		t.Errorf("final commit deleted %d paths, want the 2 posts + the profile: %v", len(deleted), deleted)
	}
	var sawProfile bool
	for path := range deleted {
		if bytes.HasPrefix([]byte(path), []byte("app.bsky.actor.profile/")) {
			sawProfile = true
		}
	}
	if !sawProfile {
		t.Error("the profile record was not swept — a presence is more than its posts")
	}

	// Exactly one terminal announcement, and it says deleted.
	active, status := accountFrameStatus(t, fx.store, did)
	if active {
		t.Error("#account active = true after a delete sweep")
	}
	if status != atprotorepo.AccountStatusDeleted {
		t.Errorf("#account status = %q, want %q", status, atprotorepo.AccountStatusDeleted)
	}

	// And the repo is gone: RepoNotFound is what every read now answers.
	if _, _, exists, err := fx.store.RepoStatus(ctx, did); err != nil || exists {
		t.Errorf("repo survived the sweep (exists=%v err=%v)", exists, err)
	}
	for _, seed := range []byte{0xd1, 0xd2} {
		if _, _, ok, _ := fx.store.PostAtURI(ctx, did, hex.EncodeToString(bytes.Repeat([]byte{seed}, 32))); ok {
			t.Errorf("post_map entry for %x survived the sweep", seed)
		}
	}
}

// TestDeleteSweepIsIdempotent: the pass runs every 30 s, so a swept identity
// whose nest row still says `deleted` must cost nothing and — crucially — must
// not announce a second time. A relay that receives #account(deleted) twice is
// unharmed, but a sweep that re-announced on every tick would be a slow leak of
// meaningless frames into a stream consumers pay to read.
func TestDeleteSweepIsIdempotent(t *testing.T) {
	fx := newProjFixture(t)
	did := "did:plc:once0000000000000000000000"
	projectThenDelete(t, fx, "alice.example.com", did, 0x55, []wsrpc.PublicPostItem{postItem(0xe1, 1000, "hi")})

	before := accountFrames(t, fx.store, did)
	for range 3 {
		runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())
	}
	if after := accountFrames(t, fx.store, did); after != before {
		t.Errorf("re-running the sweep emitted %d more #account frames, want 0", after-before)
	}
	if before != 1 {
		t.Errorf("the sweep emitted %d #account frames, want exactly 1", before)
	}
}

// TestDeleteSweepNeverAnnouncesDeactivatedFirst guards the pre-emption in
// runProjectionPass. A `deleted` identity is inactive too, so falling through
// to the S4-D reconcile would tell the network "deactivated" — a reversible
// state — moments before telling it "deleted". Consumers act on the first
// frame they see; the sweep must not lie to them in passing.
func TestDeleteSweepNeverAnnouncesDeactivatedFirst(t *testing.T) {
	fx := newProjFixture(t)
	did := "did:plc:direct00000000000000000000"
	projectThenDelete(t, fx, "alice.example.com", did, 0x66, []wsrpc.PublicPostItem{postItem(0xf1, 1000, "hi")})

	for _, f := range decodedAccountFrames(t, fx.store, did) {
		if s := f.Status; s != nil && *s == atprotorepo.AccountStatusDeactivated {
			t.Fatal("the sweep announced 'deactivated' before 'deleted' — a reversible state the user did not ask for")
		}
	}
}

// TestDeleteSweepConvergesAfterACrashBeforeTheAnnouncement is the crash story
// for the window the ordering deliberately opens: records already swept, the
// #account not yet written. The next pass must finish the job rather than
// conclude there is nothing left to do because the repo is already empty.
func TestDeleteSweepConvergesAfterACrashBeforeTheAnnouncement(t *testing.T) {
	fx := newProjFixture(t)
	ctx := context.Background()
	did := "did:plc:crash000000000000000000000"
	handle := "alice.example.com"

	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc(handle, did, 0x77)}
	fx.nest.posts = []wsrpc.PublicPostItem{postItem(0xc1, 1000, "hi")}
	fx.nest.profile = []byte("Alice")
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())

	// Simulate the crash: the sweep's commits landed, the process died before
	// EmitAccount. Deleting the records directly reproduces exactly that state
	// — an empty but still-present repo.
	paths, _, err := fx.store.RecordPathsPage(ctx, did, "", 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(paths) == 0 {
		t.Fatal("precondition: nothing projected")
	}
	if _, err := fx.store.DB().ExecContext(ctx, `DELETE FROM records WHERE did = ?`, did); err != nil {
		t.Fatal(err)
	}
	if n := accountFrames(t, fx.store, did); n != 0 {
		t.Fatalf("precondition: %d #account frames already emitted", n)
	}

	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.deletedPlc(handle, did, 0x77)}
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())

	if n := accountFrames(t, fx.store, did); n != 1 {
		t.Errorf("%d #account frames after the resumed sweep, want 1 — the announcement must not be lost", n)
	}
	if _, _, exists, _ := fx.store.RepoStatus(ctx, did); exists {
		t.Error("the resumed sweep did not purge the repo")
	}
}

// TestDeleteSweepOnANeverProjectedIdentityIsSilent: a user who confirms the
// delete before the bridge ever projected them has nothing to destroy and
// nothing the network has seen to retract. Announcing a deletion for a repo no
// consumer ever knew about would be noise, not honesty.
func TestDeleteSweepOnANeverProjectedIdentityIsSilent(t *testing.T) {
	fx := newProjFixture(t)
	did := "did:plc:never000000000000000000000"
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.deletedPlc("alice.example.com", did, 0x88)}

	runProjectionPass(context.Background(), fx.nest, fx.deps, discardLogger())

	if n := accountFrames(t, fx.store, did); n != 0 {
		t.Errorf("%d #account frames for a never-projected identity, want 0", n)
	}
	if fx.nest.publicPostsCalls != 0 {
		t.Errorf("a deleted identity was polled for posts %d times, want 0", fx.nest.publicPostsCalls)
	}
}

// TestDeleteSweepStopsProjecting: the roster still lists a deleted identity (it
// must, or the bridge could not react at all), so the pass has to skip the
// projection arm — otherwise the next tick would re-project the very posts the
// sweep just deleted, and the two would fight forever.
func TestDeleteSweepStopsProjecting(t *testing.T) {
	fx := newProjFixture(t)
	ctx := context.Background()
	did := "did:plc:stopped0000000000000000000"
	projectThenDelete(t, fx, "alice.example.com", did, 0x99, []wsrpc.PublicPostItem{postItem(0xb2, 1000, "hi")})

	callsAfterSweep := fx.nest.publicPostsCalls
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())

	if fx.nest.publicPostsCalls != callsAfterSweep {
		t.Errorf("a deleted identity was polled for posts again (%d -> %d)",
			callsAfterSweep, fx.nest.publicPostsCalls)
	}
	if _, _, exists, _ := fx.store.RepoStatus(ctx, did); exists {
		t.Error("a repo reappeared after the sweep — projection is still running")
	}
}

// tombstonedPlc is activePlc with the terminal roster status (S5 slice 5b).
func (fx *projFixture) tombstonedPlc(handle, did string, actorSeed byte) wsrpc.AtprotoIdentityView {
	id := fx.activePlc(handle, did, actorSeed)
	id.Status = wsrpc.IdentityStatusTombstoned
	return id
}

// TestTombstonedIdentityIsInertToTheBridge: once the user's client has retired
// the DID at the PLC directory, the bridge must do nothing at all with the row —
// not project, not announce, not re-serve.
//
// The DID no longer resolves, and a relay cannot verify a frame from an identity
// it cannot resolve, so any frame emitted here would be unverifiable noise at
// best. The sweep that destroyed the presence is a PRECONDITION of this status
// nest-side, so there is also nothing left to destroy.
//
// The assertion that matters is the negative one, and it is checked against a
// repo that still exists: the pass is fed a projected repo whose roster row then
// reports `tombstoned` directly (a real ordering, since nest can record the
// client's report before the bridge's next poll observes the deletion). Without
// the explicit skip the pass would fall into the deactivation reconcile and
// announce #account(deactivated) for an identity that is gone for good.
func TestTombstonedIdentityIsInertToTheBridge(t *testing.T) {
	fx := newProjFixture(t)
	ctx := context.Background()
	did := "did:plc:retired0000000000000000000"

	// Project a real repo first, so the negative assertions below are made
	// against a bridge that HAS something it could wrongly act on.
	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.activePlc("alice.example.com", did, 0x77)}
	fx.nest.posts = []wsrpc.PublicPostItem{postItem(0xc1, 1000, "hi")}
	fx.nest.profile = []byte("Alice")
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	if _, _, ok, err := fx.store.Head(ctx, did); err != nil || !ok {
		t.Fatalf("precondition: nothing was projected (ok=%v err=%v)", ok, err)
	}
	framesBefore := accountFrames(t, fx.store, did)
	callsBefore := fx.nest.publicPostsCalls

	fx.nest.identities = []wsrpc.AtprotoIdentityView{fx.tombstonedPlc("alice.example.com", did, 0x77)}
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())
	runProjectionPass(ctx, fx.nest, fx.deps, discardLogger())

	if n := accountFrames(t, fx.store, did); n != framesBefore {
		t.Errorf("a retired identity emitted %d new #account frame(s), want 0", n-framesBefore)
	}
	if fx.nest.publicPostsCalls != callsBefore {
		t.Errorf("a retired identity was polled for posts (%d -> %d)",
			callsBefore, fx.nest.publicPostsCalls)
	}
}
