package atprotorepo

// Store-level proofs for the delete-presence sweep's two primitives
// (atproto-pds-bridge.md § Disable & revocation layer 2): the enumerator the
// sweep walks, and the purge that erases the repo once the network has been
// told. The sweep's ORDERING and its firehose announcement are proven in the
// cmd package, where the roster and the funnel meet.

import (
	"context"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
)

const profileNSID = "app.bsky.actor.profile"

// TestRecordPathsPageWalksEveryCollection pins the enumerator's contract: it
// spans collections (a presence is posts AND the profile singleton — sweeping
// only `app.bsky.feed.post` would leave the profile record served, which is not
// a deleted presence), it is bounded by its limit, and its cursor is a keyset on
// `path` so a bounded walk resumes exactly where it stopped.
func TestRecordPathsPageWalksEveryCollection(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, _ := atcrypto.GeneratePrivateKeyK256()
	did := "did:web:alice.example"
	announced(t, st, did)
	rks := tids(3)

	ops := []RepoOp{
		{Action: ActionCreate, Collection: profileNSID, Rkey: "self", RecordCBOR: postRecord(t, "profile", "2026-01-01T00:00:00Z")},
	}
	for i, rk := range rks {
		ops = append(ops, RepoOp{
			Action: ActionCreate, Collection: feedPost, Rkey: rk,
			RecordCBOR:  postRecord(t, "post", "2026-01-01T00:00:00Z"),
			FaunaPostID: string(rune('a'+i)) + "a",
		})
	}
	if _, err := f.ApplyBatch(ctx, did, key, ops); err != nil {
		t.Fatal(err)
	}

	all, _, err := st.RecordPathsPage(ctx, did, "", 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(all) != 4 {
		t.Fatalf("unbounded walk returned %d paths, want 4 (3 posts + the profile)", len(all))
	}
	seen := map[string]int{}
	for _, p := range all {
		seen[p.Collection]++
	}
	if seen[profileNSID] != 1 || seen[feedPost] != 3 {
		t.Fatalf("collections walked = %v, want 1 profile + 3 posts", seen)
	}

	// Bounded walk + keyset resume covers the same set exactly once.
	first, next, err := st.RecordPathsPage(ctx, did, "", 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(first) != 2 {
		t.Fatalf("bounded page returned %d, want 2", len(first))
	}
	rest, _, err := st.RecordPathsPage(ctx, did, next, 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(rest) != 2 {
		t.Fatalf("resumed page returned %d, want the remaining 2", len(rest))
	}
	if rest[0] == first[1] {
		t.Error("keyset resume repeated the last row of the previous page")
	}
}

// TestPurgeRepoErasesEveryDerivedTableAndSpareTheOutbox is the destruction
// half's contract, and the list is the point: a presence delete that leaves the
// blobs served is not a delete (the images stay fetchable at their CIDs), and
// one that leaves projection_state behind would make a later re-enable rebuild
// only the posts NEWER than the sweep — silently withholding history inside the
// user's own consent floor.
//
// The outbox is deliberately NOT purged: those frames are the network's record
// that the deletion happened, and dropping them would retract the announcement
// from every relay still replaying the retention window.
func TestPurgeRepoErasesEveryDerivedTableAndSparesTheOutbox(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, _ := atcrypto.GeneratePrivateKeyK256()
	did := "did:web:alice.example"
	announced(t, st, did)
	rk := tids(1)[0]

	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rk,
		RecordCBOR: postRecord(t, "doomed", "2026-01-01T00:00:00Z"), FaunaPostID: "aa",
	}}); err != nil {
		t.Fatal(err)
	}
	if err := st.PutBlob(ctx, did, "bafkreiblob", "faunacid", "image/jpeg", []byte("jpegbytes")); err != nil {
		t.Fatal(err)
	}
	if err := st.SetProjectionWatermark(ctx, did, 1234, "aa"); err != nil {
		t.Fatal(err)
	}
	evsBefore, err := st.EventsSince(ctx, 0, 100)
	if err != nil {
		t.Fatal(err)
	}
	if len(evsBefore) == 0 {
		t.Fatal("expected the create commit in the outbox")
	}

	if err := st.PurgeRepo(ctx, did); err != nil {
		t.Fatalf("purge: %v", err)
	}

	if _, _, exists, err := st.RepoStatus(ctx, did); err != nil || exists {
		t.Errorf("repo head survived the purge (exists=%v err=%v)", exists, err)
	}
	if paths, _, err := st.RecordPathsPage(ctx, did, "", 0); err != nil || len(paths) != 0 {
		t.Errorf("records survived the purge: %v (err=%v)", paths, err)
	}
	if _, _, ok, _ := st.PostAtURI(ctx, did, "aa"); ok {
		t.Error("post_map survived the purge")
	}
	if _, ok, _ := st.GetBlob(ctx, did, "bafkreiblob"); ok {
		t.Error("blob survived the purge — the images would still be served")
	}
	cur, err := st.ProjectionState(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	if cur.LastCreatedAtMicros != 0 || cur.LastPostID != "" {
		t.Errorf("projection watermark survived the purge: %+v — a re-enable would skip history", cur)
	}
	if !cur.FirstEmitGated {
		t.Error("first-emit gate should be re-armed for a repo rebuilt from nothing")
	}

	evsAfter, err := st.EventsSince(ctx, 0, 100)
	if err != nil {
		t.Fatal(err)
	}
	if len(evsAfter) != len(evsBefore) {
		t.Errorf("outbox lost %d frames to the purge — the network's record of the deletion",
			len(evsBefore)-len(evsAfter))
	}
}

// TestPurgeRepoOnAnUnknownDIDIsANoOp is the crash-convergence property: the
// sweep retries from the top after an interruption, so purging an
// already-purged DID must succeed silently rather than error the pass into a
// retry loop.
func TestPurgeRepoOnAnUnknownDIDIsANoOp(t *testing.T) {
	ctx := context.Background()
	st, _ := newTestFunnel(t, nil)
	if err := st.PurgeRepo(ctx, "did:web:nobody.example"); err != nil {
		t.Fatalf("purge of an unknown did should be a no-op, got %v", err)
	}
}

// TestEmitAccountRefusesAnIncoherentStatusPair guards the frame producer's one
// invariant: Sync v1.1 puts the reason in `status` only when active=false, so
// "active, and by the way deleted" — or an inactive account with no stated
// reason — has no honest reading for the relay keying its bookkeeping off the
// pair, and is refused rather than serialized.
func TestEmitAccountRefusesAnIncoherentStatusPair(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	did := "did:web:alice.example"
	announced(t, st, did)

	if _, err := f.EmitAccount(ctx, did, true, AccountStatusDeleted); err == nil {
		t.Error("active=true with a status should be refused")
	}
	if _, err := f.EmitAccount(ctx, did, false, ""); err == nil {
		t.Error("active=false with no status should be refused")
	}
	if _, err := f.EmitAccount(ctx, did, false, AccountStatusDeleted); err != nil {
		t.Errorf("the terminal delete announcement should be accepted: %v", err)
	}
}
