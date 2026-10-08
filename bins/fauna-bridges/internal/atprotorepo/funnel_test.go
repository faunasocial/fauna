package atprotorepo

import (
	"bytes"
	"context"
	"errors"
	"io"
	"sync"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/atdata"
	"github.com/bluesky-social/indigo/atproto/repo"
	"github.com/bluesky-social/indigo/atproto/syntax"
	car "github.com/ipld/go-car/v2"
)

const feedPost = "app.bsky.feed.post"

// sliceClock returns a fixed rev sequence so two runs of identical ops produce
// byte-identical commits (the re-derivability property).
type sliceClock struct {
	revs []string
	i    int
}

func (s *sliceClock) Next() string {
	r := s.revs[s.i%len(s.revs)]
	s.i++
	return r
}

func postRecord(t *testing.T, text, createdAt string) []byte {
	t.Helper()
	b, err := atdata.MarshalCBOR(map[string]any{
		"$type":     feedPost,
		"text":      text,
		"createdAt": createdAt,
		"langs":     []any{"en"},
	})
	if err != nil {
		t.Fatalf("marshal record: %v", err)
	}
	return b
}

func newTestFunnel(t *testing.T, clock RevClock) (*Store, *Funnel) {
	t.Helper()
	st, err := Open(":memory:")
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(func() { st.Close() })
	f, err := NewFunnel(context.Background(), st, clock)
	if err != nil {
		t.Fatalf("new funnel: %v", err)
	}
	return st, f
}

// announced opens did's first-emit gate — the projection loop's blessing that
// this identity resolves ecosystem-side, which every firehose producer requires
// (see ErrFirstEmitGated). A fresh store gates every DID, so a test whose
// subject is NOT the gate declares its account announced, exactly as production
// does once per identity before its first frame.
func announced(t *testing.T, st *Store, did string) {
	t.Helper()
	if err := st.SetFirstEmitGated(context.Background(), did, false); err != nil {
		t.Fatalf("open first-emit gate for %s: %v", did, err)
	}
}

// tids returns n valid, strictly-increasing TID strings for use as rkeys/revs.
func tids(n int) []string {
	clk := syntax.NewTIDClock(0)
	out := make([]string, n)
	for i := range out {
		out[i] = clk.Next().String()
	}
	return out
}

// TestGenesisAndIncremental drives the canonical production flow: a first
// public post is a genesis commit; a second is an incremental commit chained to
// it; the full repo reloads through indigo's own loader with a valid structure
// and signature, and both records are retrievable.
func TestGenesisAndIncremental(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	pub, _ := key.PublicKey()
	did := "did:web:alice.example"
	announced(t, st, did)
	rk := tids(2)

	r1, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rk[0],
		RecordCBOR: postRecord(t, "hello", "2026-01-01T00:00:00Z"), FaunaPostID: "aa",
	}})
	if err != nil {
		t.Fatalf("genesis apply: %v", err)
	}
	if r1.Seq != 1 || r1.NoChange {
		t.Fatalf("genesis result: seq=%d nochange=%v", r1.Seq, r1.NoChange)
	}

	// Reload the genesis repo through indigo's loader.
	carBytes, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatalf("export genesis: %v", err)
	}
	commit, rr, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(carBytes))
	if err != nil {
		t.Fatalf("LoadRepoFromCAR (genesis): %v", err)
	}
	if err := commit.VerifyStructure(); err != nil {
		t.Fatalf("VerifyStructure: %v", err)
	}
	if err := commit.VerifySignature(pub); err != nil {
		t.Fatalf("VerifySignature: %v", err)
	}
	if _, _, err := rr.GetRecordBytes(ctx, syntax.NSID(feedPost), syntax.RecordKey(rk[0])); err != nil {
		t.Fatalf("first record not retrievable: %v", err)
	}

	// Second post: incremental commit chained to the genesis.
	r2, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rk[1],
		RecordCBOR: postRecord(t, "world", "2026-01-02T00:00:00Z"), FaunaPostID: "bb",
	}})
	if err != nil {
		t.Fatalf("incremental apply: %v", err)
	}
	if r2.Seq != 2 {
		t.Errorf("second seq = %d, want 2", r2.Seq)
	}
	if r2.CommitCID == r1.CommitCID {
		t.Error("incremental commit CID must differ from genesis")
	}
	if r2.Rev <= r1.Rev {
		t.Errorf("rev must advance: %q <= %q", r2.Rev, r1.Rev)
	}

	// The full repo now carries both records.
	carBytes2, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatalf("export after incremental: %v", err)
	}
	commit2, rr2, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(carBytes2))
	if err != nil {
		t.Fatalf("LoadRepoFromCAR (incremental): %v", err)
	}
	if err := commit2.VerifySignature(pub); err != nil {
		t.Fatalf("VerifySignature (incremental): %v", err)
	}
	for _, rkey := range rk {
		if _, _, err := rr2.GetRecordBytes(ctx, syntax.NSID(feedPost), syntax.RecordKey(rkey)); err != nil {
			t.Errorf("record %s missing after incremental: %v", rkey, err)
		}
	}
}

// TestDeterministicReDerivation is the C6 guarantee: the same op sequence with
// the same rev clock re-projects to a byte-identical MST — same records, same
// AT-URIs, same root CID — so the carstore is re-derivable and no user data is
// ever irrecoverable. The COMMIT block is not byte-identical across re-projection:
// indigo signs K-256 commits with a low-S but randomized (non-RFC6979) nonce, so
// each signing yields a fresh valid signature and thus a fresh commit CID. That
// is correct — a commit is a re-signable envelope over the deterministic MST;
// re-derivability is a property of the records, not of the signature.
func TestDeterministicReDerivation(t *testing.T) {
	ctx := context.Background()
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:web:alice.example"
	rk := tids(2)
	revs := tids(4) // two runs × two commits
	rec1 := postRecord(t, "hello", "2026-01-01T00:00:00Z")
	rec2 := postRecord(t, "world", "2026-01-02T00:00:00Z")

	run := func() (CommitResult, CommitResult) {
		st, f := newTestFunnel(t, &sliceClock{revs: revs})
		announced(t, st, did)
		a, err := f.ApplyBatch(ctx, did, key, []RepoOp{{Action: ActionCreate, Collection: feedPost, Rkey: rk[0], RecordCBOR: rec1, FaunaPostID: "aa"}})
		if err != nil {
			t.Fatal(err)
		}
		b, err := f.ApplyBatch(ctx, did, key, []RepoOp{{Action: ActionCreate, Collection: feedPost, Rkey: rk[1], RecordCBOR: rec2, FaunaPostID: "bb"}})
		if err != nil {
			t.Fatal(err)
		}
		return a, b
	}
	a1, b1 := run()
	a2, b2 := run()

	// The MST root is a pure function of the records — always identical.
	if a1.DataCID != a2.DataCID || b1.DataCID != b2.DataCID {
		t.Errorf("MST root not re-derivable: %s/%s vs %s/%s", a1.DataCID, b1.DataCID, a2.DataCID, b2.DataCID)
	}
	// The commit CID legitimately differs across runs (randomized signature),
	// but the two commits over the same MST must never be the empty/zero CID
	// and must differ from each other's DataCID.
	if a1.CommitCID == "" || a1.DataCID == "" || a1.CommitCID == a1.DataCID {
		t.Errorf("degenerate commit/data CIDs: commit=%q data=%q", a1.CommitCID, a1.DataCID)
	}
}

// TestDeleteTombstone: deleting a projected post removes it from the repo and
// its post_map entry, and a delete of a never-projected post is a no-op.
func TestDeleteTombstone(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, _ := atcrypto.GeneratePrivateKeyK256()
	did := "did:web:alice.example"
	announced(t, st, did)
	rk := tids(1)[0]

	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{Action: ActionCreate, Collection: feedPost, Rkey: rk, RecordCBOR: postRecord(t, "doomed", "2026-01-01T00:00:00Z"), FaunaPostID: "aa"}}); err != nil {
		t.Fatal(err)
	}
	if _, _, ok, _ := st.PostAtURI(ctx, did, "aa"); !ok {
		t.Fatal("post_map should have the projected post")
	}

	// No-op delete of an unknown post produces no commit.
	noop, err := f.ApplyBatch(ctx, did, key, []RepoOp{{Action: ActionDelete, Collection: feedPost, Rkey: "nonexistent", FaunaPostID: "zz"}})
	if err != nil {
		t.Fatal(err)
	}
	if !noop.NoChange {
		t.Error("delete of unknown record should be NoChange")
	}

	// Real delete removes record + post_map.
	del, err := f.ApplyBatch(ctx, did, key, []RepoOp{{Action: ActionDelete, Collection: feedPost, Rkey: rk, FaunaPostID: "aa"}})
	if err != nil {
		t.Fatal(err)
	}
	if del.NoChange || del.Ops != 1 {
		t.Fatalf("delete result: nochange=%v ops=%d", del.NoChange, del.Ops)
	}
	if _, _, ok, _ := st.PostAtURI(ctx, did, "aa"); ok {
		t.Error("post_map entry should be gone after delete")
	}
	carBytes, _ := st.ExportRepo(ctx, did)
	_, rr, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(carBytes))
	if err != nil {
		t.Fatalf("reload after delete: %v", err)
	}
	if _, _, err := rr.GetRecordBytes(ctx, syntax.NSID(feedPost), syntax.RecordKey(rk)); err == nil {
		t.Error("deleted record should not be retrievable")
	}
}

// TestExportedCARv1IsValid independently validates our hand-written CARv1
// encoder against go-car/v2's BlockReader (a second, unrelated parser): the
// header root is the commit CID and every block's declared CID matches its
// bytes.
func TestExportedCARv1IsValid(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, _ := atcrypto.GeneratePrivateKeyK256()
	did := "did:web:alice.example"
	announced(t, st, did)
	r, err := f.ApplyBatch(ctx, did, key, []RepoOp{{Action: ActionCreate, Collection: feedPost, Rkey: tids(1)[0], RecordCBOR: postRecord(t, "hi", "2026-01-01T00:00:00Z"), FaunaPostID: "aa"}})
	if err != nil {
		t.Fatal(err)
	}
	carBytes, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	br, err := car.NewBlockReader(bytes.NewReader(carBytes))
	if err != nil {
		t.Fatalf("go-car/v2 rejected our CARv1: %v", err)
	}
	if br.Version != 1 {
		t.Errorf("CAR version = %d, want 1", br.Version)
	}
	if len(br.Roots) != 1 || br.Roots[0].String() != r.CommitCID {
		t.Errorf("CAR roots = %v, want [%s]", br.Roots, r.CommitCID)
	}
	n := 0
	for {
		blk, err := br.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatalf("block %d: go-car/v2 could not verify CID/bytes: %v", n, err)
		}
		_ = blk
		n++
	}
	if n < 3 { // commit + MST root + record, at minimum
		t.Errorf("only %d blocks in genesis CAR, want >= 3", n)
	}
}

// TestPerDIDSerialization hammers one repo with concurrent ApplyBatch calls and
// asserts the funnel never corrupts it: the final repo reloads cleanly and
// carries exactly the surviving records.
func TestPerDIDSerialization(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, _ := atcrypto.GeneratePrivateKeyK256()
	did := "did:web:alice.example"
	announced(t, st, did)
	const n = 20
	rk := tids(n)

	var wg sync.WaitGroup
	errs := make([]error, n)
	for i := 0; i < n; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			_, errs[i] = f.ApplyBatch(ctx, did, key, []RepoOp{{
				Action: ActionCreate, Collection: feedPost, Rkey: rk[i],
				RecordCBOR: postRecord(t, "post", "2026-01-01T00:00:00Z"), FaunaPostID: rk[i],
			}})
		}(i)
	}
	wg.Wait()
	for i, err := range errs {
		if err != nil {
			t.Fatalf("concurrent apply %d: %v", i, err)
		}
	}

	carBytes, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	commit, rr, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(carBytes))
	if err != nil {
		t.Fatalf("reload after concurrent writes: %v", err)
	}
	pub, _ := key.PublicKey()
	if err := commit.VerifySignature(pub); err != nil {
		t.Fatalf("final commit signature invalid: %v", err)
	}
	for _, rkey := range rk {
		if _, _, err := rr.GetRecordBytes(ctx, syntax.NSID(feedPost), syntax.RecordKey(rkey)); err != nil {
			t.Errorf("record %s lost under concurrency: %v", rkey, err)
		}
	}
}

// FaunaPostIDForRecord is post_map read backwards, and the F2 delete path is
// its only caller: an external `deleteRecord` names a record by rkey, and
// tombstoning the Fauna post behind it needs the id the rkey derivation cannot
// give back. Three properties matter — it finds a mapped record, it reports
// ok=false (rather than erroring) for a record with no Fauna identity, and it
// stops reporting one once the record is deleted.
func TestFaunaPostIDForRecordIsThePostMapReverse(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:web:alice.example"
	announced(t, st, did)
	rk := tids(2)

	// A projected post: mapped, and resolvable back to its Fauna post id.
	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rk[0],
		RecordCBOR: postRecord(t, "hello", "2026-01-01T00:00:00Z"), FaunaPostID: "abcd",
	}}); err != nil {
		t.Fatalf("apply: %v", err)
	}
	got, ok, err := st.FaunaPostIDForRecord(ctx, did, feedPost, rk[0])
	if err != nil || !ok || got != "abcd" {
		t.Fatalf("mapped record: got %q ok=%v err=%v", got, ok, err)
	}

	// A record with no Fauna identity (the journal case) maps to nothing —
	// ok=false, not an error, so the delete path commits without a post id.
	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rk[1],
		RecordCBOR: postRecord(t, "journaled", "2026-01-01T00:00:01Z"),
	}}); err != nil {
		t.Fatalf("apply journal-shaped: %v", err)
	}
	if _, ok, err := st.FaunaPostIDForRecord(ctx, did, feedPost, rk[1]); err != nil || ok {
		t.Fatalf("unmapped record: ok=%v err=%v", ok, err)
	}

	// Never-existed and another repo's DID both report absent, never another
	// account's post id.
	if _, ok, _ := st.FaunaPostIDForRecord(ctx, did, feedPost, "3neverwas"); ok {
		t.Fatal("a record that never existed reported a fauna post id")
	}
	if _, ok, _ := st.FaunaPostIDForRecord(ctx, "did:web:bob.example", feedPost, rk[0]); ok {
		t.Fatal("another DID's lookup resolved alice's record")
	}

	// Deleting the record drops the mapping with it, in the same txn.
	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionDelete, Collection: feedPost, Rkey: rk[0], FaunaPostID: "abcd",
	}}); err != nil {
		t.Fatalf("delete: %v", err)
	}
	if _, ok, _ := st.FaunaPostIDForRecord(ctx, did, feedPost, rk[0]); ok {
		t.Fatal("a deleted record still resolves to a fauna post id")
	}
}

// ── S5 slice 4: the #sync collapse producer ──────────────────────────────────

// TestDeferredBatchMutatesTheRepoButEmitsNoFrame is the collapse primitive's
// core contract (atproto-pds-bridge.md § Projection & backfill, watermark row
// 3): a deferred batch is a NORMAL commit in every respect the repo cares about
// — head moves, record lands, post_map maps, the CAR still loads through
// indigo — and differs only in that no #commit reaches the outbox. The repo is
// therefore ahead of what the network has been told, which is exactly the state
// `sync_owed` exists to record.
func TestDeferredBatchMutatesTheRepoButEmitsNoFrame(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	pub, _ := key.PublicKey()
	did := "did:web:alice.example"
	announced(t, st, did)
	rk := tids(2)

	res, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rk[0],
		RecordCBOR: postRecord(t, "collapsed", "2026-01-01T00:00:00Z"), FaunaPostID: "aa",
	}}, DeferFrameToSync())
	if err != nil {
		t.Fatalf("deferred apply: %v", err)
	}
	if res.NoChange {
		t.Fatal("deferred apply reported NoChange — the commit must still happen")
	}
	if res.Seq != 0 || res.Frame != nil {
		t.Errorf("deferred apply reported seq=%d frame=%d bytes, want no frame", res.Seq, len(res.Frame))
	}

	// The repo really moved: head present, and the exported CAR verifies.
	carBytes, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatalf("export: %v", err)
	}
	commit, _, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(carBytes))
	if err != nil {
		t.Fatalf("LoadRepoFromCAR: %v", err)
	}
	if err := commit.VerifySignature(pub); err != nil {
		t.Fatalf("VerifySignature: %v", err)
	}
	if _, _, mapped, err := st.PostAtURI(ctx, did, "aa"); err != nil || !mapped {
		t.Fatalf("post_map not written by a deferred commit: mapped=%v err=%v", mapped, err)
	}

	// ...but the network was told nothing.
	if _, _, ok, err := st.SeqRange(ctx); err != nil || ok {
		t.Fatalf("deferred commit wrote an outbox row (seq range present=%v, err=%v)", ok, err)
	}
	owed, err := st.SyncOwed(ctx, did)
	if err != nil || !owed {
		t.Fatalf("sync_owed after a deferred commit = %v (err %v), want true", owed, err)
	}

	// A second deferred commit keeps the debt at exactly one #sync.
	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rk[1],
		RecordCBOR: postRecord(t, "also collapsed", "2026-01-01T00:00:01Z"), FaunaPostID: "bb",
	}}, DeferFrameToSync()); err != nil {
		t.Fatalf("second deferred apply: %v", err)
	}

	// EmitSync pays the debt: exactly one frame, and the flag clears.
	seq, err := f.EmitSync(ctx, did)
	if err != nil {
		t.Fatalf("EmitSync: %v", err)
	}
	if seq != 1 {
		t.Errorf("#sync seq = %d, want 1 (the deferred commits burned none)", seq)
	}
	evs, err := st.EventsSince(ctx, 0, 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(evs) != 1 || evs[0].FrameType != FrameSync {
		t.Fatalf("outbox after collapse = %d rows (first %v), want exactly one #sync", len(evs), frameTypes(evs))
	}
	owed, err = st.SyncOwed(ctx, did)
	if err != nil || owed {
		t.Fatalf("sync_owed after EmitSync = %v (err %v), want false", owed, err)
	}
}

// TestEmitSyncOnARepoWithNoHeadIsANoOp guards the crash-recovery path: a pass
// that set the debt but crashed before any commit persisted would otherwise ask
// for a #sync announcing a head that does not exist. Announcing nothing is the
// only honest answer, and the debt must clear so the loop does not spin.
func TestEmitSyncOnARepoWithNoHeadIsANoOp(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	// Announced but headless: the subject here is the missing head, so the gate
	// must not be what refuses.
	announced(t, st, "did:web:nobody.example")
	seq, err := f.EmitSync(ctx, "did:web:nobody.example")
	if err != nil {
		t.Fatalf("EmitSync on a headless repo: %v", err)
	}
	if seq != 0 {
		t.Errorf("EmitSync on a headless repo allocated seq %d, want 0", seq)
	}
	if _, _, ok, err := st.SeqRange(ctx); err != nil || ok {
		t.Fatalf("EmitSync on a headless repo wrote an outbox row (ok=%v err=%v)", ok, err)
	}
}

// TestLiveBatchNeverOwesASync pins the negative: the ordinary path is untouched
// by this slice — it emits its #commit and takes on no #sync debt.
func TestLiveBatchNeverOwesASync(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:web:alice.example"
	announced(t, st, did)
	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: tids(1)[0],
		RecordCBOR: postRecord(t, "live", "2026-01-01T00:00:00Z"), FaunaPostID: "aa",
	}}); err != nil {
		t.Fatalf("live apply: %v", err)
	}
	evs, err := st.EventsSince(ctx, 0, 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(evs) != 1 || evs[0].FrameType != FrameCommit {
		t.Fatalf("outbox = %v, want one #commit", frameTypes(evs))
	}
	if owed, err := st.SyncOwed(ctx, did); err != nil || owed {
		t.Fatalf("a live commit owes a #sync (owed=%v err=%v)", owed, err)
	}
}

func frameTypes(evs []FirehoseEvent) []string {
	out := make([]string, 0, len(evs))
	for _, e := range evs {
		out = append(out, e.FrameType)
	}
	return out
}

// ── Slice 4e: the first-emit chokepoint ────────────────────────────

// TestFirehoseProducersRefuseAFirstEmitGatedDID is the structural half of the
// pre-firehose resolvability gate (atproto-pds-full.md § Ecosystem reality). The
// projection loop has always checked the gate before committing; the F2 write
// path did not, and went straight through ApplyBatch — so an external app could
// put an account's FIRST firehose event on the network while its DID/handle
// still failed to resolve, which permanently 404s the account on the AppView.
//
// The gate now lives on the producers themselves, so the guarantee is a property
// of the funnel rather than of whoever happens to call it. Each arm carries an
// announced control on the same call, so a refusal cannot pass vacuously.
func TestFirehoseProducersRefuseAFirstEmitGatedDID(t *testing.T) {
	ctx := context.Background()
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:web:alice.example"
	rk := tids(1)[0]
	batch := func() []RepoOp {
		return []RepoOp{{
			Action: ActionCreate, Collection: feedPost, Rkey: rk,
			RecordCBOR: postRecord(t, "hello", "2026-01-01T00:00:00Z"), FaunaPostID: "aa",
		}}
	}

	t.Run("ApplyBatch", func(t *testing.T) {
		st, f := newTestFunnel(t, nil)
		if _, err := f.ApplyBatch(ctx, did, key, batch()); !errors.Is(err, ErrFirstEmitGated) {
			t.Fatalf("a gated DID's commit must be refused, got %v", err)
		}
		// Refused BEFORE anything persisted: no outbox row, no repo.
		if _, _, ok, err := st.SeqRange(ctx); err != nil || ok {
			t.Fatalf("a gated commit wrote an outbox row (ok=%v err=%v)", ok, err)
		}
		if _, exists, err := st.RepoActive(ctx, did); err != nil || exists {
			t.Fatalf("a gated commit created a repo (exists=%v err=%v)", exists, err)
		}
		announced(t, st, did)
		if _, err := f.ApplyBatch(ctx, did, key, batch()); err != nil {
			t.Fatalf("announced control commit failed: %v", err)
		}
	})

	// The gate guards the FIREHOSE, not the repo. A DeferFrameToSync batch
	// appends no outbox row (it books a #sync debt instead), so it carries
	// nothing onto the network and is deliberately allowed through — the
	// downtime-catch-up collapse must not need an announced identity to make
	// progress. Pinned so the carve-out stays a decision, not an oversight.
	t.Run("DeferFrameToSync is exempt", func(t *testing.T) {
		st, f := newTestFunnel(t, nil)
		if _, err := f.ApplyBatch(ctx, did, key, batch(), DeferFrameToSync()); err != nil {
			t.Fatalf("a deferred batch emits no frame and must be allowed: %v", err)
		}
		if _, _, ok, err := st.SeqRange(ctx); err != nil || ok {
			t.Fatalf("the deferred batch wrote an outbox row (ok=%v err=%v)", ok, err)
		}
	})

	t.Run("EmitIdentity", func(t *testing.T) {
		st, f := newTestFunnel(t, nil)
		if _, err := f.EmitIdentity(ctx, did, "alice.example"); !errors.Is(err, ErrFirstEmitGated) {
			t.Fatalf("a gated DID's #identity must be refused, got %v", err)
		}
		announced(t, st, did)
		if _, err := f.EmitIdentity(ctx, did, "alice.example"); err != nil {
			t.Fatalf("announced control #identity failed: %v", err)
		}
	})

	t.Run("EmitSync", func(t *testing.T) {
		st, f := newTestFunnel(t, nil)
		if _, err := f.EmitSync(ctx, did); !errors.Is(err, ErrFirstEmitGated) {
			t.Fatalf("a gated DID's #sync must be refused, got %v", err)
		}
		announced(t, st, did)
		if _, err := f.EmitSync(ctx, did); err != nil {
			t.Fatalf("announced control #sync failed: %v", err)
		}
	})

	t.Run("EmitAccount", func(t *testing.T) {
		st, f := newTestFunnel(t, nil)
		if _, err := f.EmitAccount(ctx, did, false, AccountStatusDeactivated); !errors.Is(err, ErrFirstEmitGated) {
			t.Fatalf("a gated DID's #account must be refused, got %v", err)
		}
		announced(t, st, did)
		if _, err := f.EmitAccount(ctx, did, false, AccountStatusDeactivated); err != nil {
			t.Fatalf("announced control #account failed: %v", err)
		}
	})
}

// TestCASIsEnforcedAgainstTheHeadTheCommitChainsOnto is the race-free half of
// compare-and-swap: the write path pre-checks the caller's swapCommit/
// swapRecord before it calls the nest (so a doomed write ingests nothing), but
// that read cannot be atomic with the commit, so ApplyBatch repeats the
// comparison under its own per-DID lock. This is the site that actually keeps
// the caller's concurrency guarantee — atproto-pds-full.md § F2 detail, the
// CAS bullet's two-site enforcement.
func TestCASIsEnforcedAgainstTheHeadTheCommitChainsOnto(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:web:cas.example"
	announced(t, st, did)
	rk := tids(3)

	create := func(rkey, text, postID string, opts ...BatchOption) (CommitResult, error) {
		return f.ApplyBatch(ctx, did, key, []RepoOp{{
			Action: ActionCreate, Collection: feedPost, Rkey: rkey,
			RecordCBOR: postRecord(t, text, "2026-01-01T00:00:00Z"), FaunaPostID: postID,
		}}, opts...)
	}

	// A swapCommit on a repo with no commit yet is a mismatch, not a vacuous
	// pass: there is no head to have named.
	if _, err := create(rk[0], "first", "aa", ExpectCommit("bafyimagined")); !errors.Is(err, ErrSwapMismatch) {
		t.Fatalf("swapCommit against an empty repo: %v, want ErrSwapMismatch", err)
	}
	// ...and it applied nothing, which is the whole point of a failed CAS.
	if _, _, ok, err := st.Head(ctx, did); err != nil || ok {
		t.Fatalf("a failed CAS committed anyway (head exists=%v, err=%v)", ok, err)
	}

	genesis, err := create(rk[0], "first", "aa")
	if err != nil {
		t.Fatalf("genesis: %v", err)
	}

	// Stale head → refused; current head → served. The pair is what proves the
	// comparison is real rather than the parameter simply being ignored.
	if _, err := create(rk[1], "second", "bb", ExpectCommit("bafystale")); !errors.Is(err, ErrSwapMismatch) {
		t.Fatalf("stale swapCommit: %v, want ErrSwapMismatch", err)
	}
	if _, err := create(rk[1], "second", "bb", ExpectCommit(genesis.CommitCID)); err != nil {
		t.Fatalf("swapCommit naming the current head: %v", err)
	}

	// Record-level CAS, all three assertions the wire can carry.
	path := feedPost + "/" + rk[0]
	recordCID, _, ok, err := st.GetRecord(ctx, did, feedPost, rk[0])
	if err != nil || !ok {
		t.Fatalf("read back the first record: ok=%v err=%v", ok, err)
	}
	if _, err := create(rk[2], "third", "cc",
		ExpectRecords(map[string]RecordSwap{path: {CID: "bafywrong"}})); !errors.Is(err, ErrSwapMismatch) {
		t.Fatalf("stale swapRecord: %v, want ErrSwapMismatch", err)
	}
	if _, err := create(rk[2], "third", "cc",
		ExpectRecords(map[string]RecordSwap{path: {MustNotExist: true}})); !errors.Is(err, ErrSwapMismatch) {
		t.Fatalf("must-not-exist against a record that DOES exist: %v, want ErrSwapMismatch", err)
	}
	if _, err := create(rk[2], "third", "cc", ExpectRecords(map[string]RecordSwap{
		path:                         {CID: recordCID},
		feedPost + "/" + "3neverwas": {MustNotExist: true},
	})); err != nil {
		t.Fatalf("swapRecord naming the current CID (and a genuinely absent record): %v", err)
	}
}

// ── reconciler race: a projection pass landing inside the write path's own
//    ingest→commit window ──

// TestASecondCommitAtAnOccupiedDeterministicRkeyConvergesRatherThanCollides
// proves the no-CAS direction of the reconciler race is benign by
// construction, not by luck. Production shape: `ingest_external_write`
// answers a new Fauna post synchronously, but the bridge's own funnel commit
// for it happens strictly AFTER that nest round trip returns
// (repo_write.go's applyRecordWrite). In between, nothing holds the per-DID
// lock, so the projection loop's own 30s pass can see the same unmapped
// Fauna post and commit it FIRST — at the identical deterministic rkey
// (D1) the write path's own commit will target next, with the
// SAME FaunaPostID.
//
// Two DESIGNED properties are what keep the second, later commit from
// erroring or losing data (the review's "do not simplify either"): the rkey
// is deterministic, so both writers land at the same path rather than the
// second creating a duplicate record at a second key; and post_map is an
// UPSERT keyed on (did, fauna_post_id) — funnel.go's `ON CONFLICT(did,
// fauna_post_id) DO UPDATE` — not a bare INSERT, so the second commit's
// post_map write converges instead of colliding. Sequencing the two
// ApplyBatch calls in this exact order reproduces the race's effect on the
// funnel deterministically: the funnel has no notion of which caller ran
// first, only of what is already at a path when a commit lands, so ordering
// the calls IS driving the race — no sleep, no goroutine, no pause hook
// needed to prove this half of it.
func TestASecondCommitAtAnOccupiedDeterministicRkeyConvergesRatherThanCollides(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:web:race.example"
	announced(t, st, did)
	rkey := tids(1)[0]
	const faunaPostID = "reconciler-race-post"

	// The projection pass wins the race: it observes the unmapped Fauna post
	// first and commits its OWN rendering at the deterministic rkey.
	projected := postRecord(t, "the projection's rendering", "2026-01-01T00:00:00Z")
	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rkey,
		RecordCBOR: projected, FaunaPostID: faunaPostID,
	}}); err != nil {
		t.Fatalf("projection-pass commit (simulating the pass winning the race): %v", err)
	}

	// The write path's own commit lands second — same rkey, same FaunaPostID,
	// but the caller's own distinct bytes, exactly what repo_write.go sends
	// after the nest ingest call returns.
	callers := postRecord(t, "the caller's own exact bytes", "2026-01-01T00:00:00Z")
	res, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionCreate, Collection: feedPost, Rkey: rkey,
		RecordCBOR: callers, FaunaPostID: faunaPostID,
	}})
	if err != nil {
		t.Fatalf("write path's own commit after a projection-pass race: %v", err)
	}
	if res.NoChange || res.Ops != 1 {
		t.Fatalf("write-path commit reported NoChange=%v Ops=%d, want one real op (the bytes differ)", res.NoChange, res.Ops)
	}

	// The caller's bytes win — never the projection's. A consumer reading the
	// repo (or the firehose two frames later) sees exactly what the caller
	// wrote, which is what makes the synchronously-answered cid honest.
	carBytes, err := st.ExportRepo(ctx, did)
	if err != nil {
		t.Fatalf("export: %v", err)
	}
	_, rr, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(carBytes))
	if err != nil {
		t.Fatalf("reload: %v", err)
	}
	gotBytes, _, err := rr.GetRecordBytes(ctx, syntax.NSID(feedPost), syntax.RecordKey(rkey))
	if err != nil {
		t.Fatalf("get record: %v", err)
	}
	if !bytes.Equal(gotBytes, callers) {
		t.Fatal("stored record is not the caller's bytes — the projection's rendering won the race instead")
	}

	// post_map converges rather than colliding: it names the CALLER's record
	// CID, not the projection's stale one — exactly the property a bare
	// INSERT (instead of the upsert) would break.
	wantCID, err := RecordCID(callers)
	if err != nil {
		t.Fatal(err)
	}
	atURI, recordCID, mapped, err := st.PostAtURI(ctx, did, faunaPostID)
	if err != nil || !mapped {
		t.Fatalf("post_map lookup: mapped=%v err=%v", mapped, err)
	}
	if recordCID != wantCID {
		t.Fatalf("post_map record_cid = %s, want the caller's %s (the projection's stale row survived)", recordCID, wantCID)
	}
	if want := ATURI(did, feedPost, rkey); atURI != want {
		t.Fatalf("post_map at_uri = %s, want %s", atURI, want)
	}
}

// TestASecondCommitOfByteIdenticalBytesEmitsNoRedundantFrameWithSkipUnchanged
// covers the PROFILE variant of the same race (slice 3's note: since both
// writers render through the same RenderProfileRecord, a profile race
// commits byte-identical records, not a content flip). Passing
// SkipUnchangedRecords on the write path's own commit — the cheap half of
// the recon's fix menu — turns that redundant second `#commit` into a
// NoChange no-op instead of a second broadcast frame naming the exact bytes
// a consumer already has.
func TestASecondCommitOfByteIdenticalBytesEmitsNoRedundantFrameWithSkipUnchanged(t *testing.T) {
	ctx := context.Background()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	did := "did:web:race-profile.example"
	announced(t, st, did)

	record := postRecord(t, "identical either way", "2026-01-01T00:00:00Z")
	if _, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionUpdate, Collection: NsidProfile, Rkey: RkeyProfile,
		RecordCBOR: record,
	}}); err != nil {
		t.Fatalf("first (projection-pass) commit: %v", err)
	}

	res, err := f.ApplyBatch(ctx, did, key, []RepoOp{{
		Action: ActionUpdate, Collection: NsidProfile, Rkey: RkeyProfile,
		RecordCBOR: record,
	}}, SkipUnchangedRecords())
	if err != nil {
		t.Fatalf("second (write-path) commit with SkipUnchangedRecords: %v", err)
	}
	if !res.NoChange {
		t.Fatal("byte-identical second commit still produced a change — SkipUnchangedRecords did not short-circuit it")
	}
	if res.Frame != nil {
		t.Fatalf("byte-identical second commit still emitted a #commit frame (%d bytes)", len(res.Frame))
	}
}
