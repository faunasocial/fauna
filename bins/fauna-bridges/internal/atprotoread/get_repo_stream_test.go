package atprotoread

import (
	"context"
	"database/sql"
	"io"
	"net/http"
	"path/filepath"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/ipfs/go-cid"
	_ "modernc.org/sqlite" // the same pure-Go driver the store registers

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
)

// seedFileRepo is seedRepoRecords over a real file-backed store, so a test can
// reach into the sqlite the store is serving from and corrupt it the way disk
// corruption would. Returns the store, its path, and the rkeys in commit order.
func seedFileRepo(t *testing.T, n int) (*atprotorepo.Store, string, []string) {
	t.Helper()
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "repo.db")
	store, err := atprotorepo.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close() })
	funnel, err := atprotorepo.NewFunnel(ctx, store, testClock)
	if err != nil {
		t.Fatal(err)
	}
	announced(t, store, testDID)
	signer, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	rkeys := make([]string, n)
	for i := range rkeys {
		recCBOR, err := atprotorepo.JSONRecordToDagCBOR(
			`{"$type":"app.bsky.feed.post","text":"post ` + string(rune('a'+i)) + `","createdAt":"2026-01-01T00:00:00Z"}`)
		if err != nil {
			t.Fatal(err)
		}
		rkeys[i] = testClock.Next()
		if _, err := funnel.ApplyBatch(ctx, testDID, signer, []atprotorepo.RepoOp{{
			Action:      atprotorepo.ActionCreate,
			Collection:  feedPostNSID,
			Rkey:        rkeys[i],
			RecordCBOR:  recCBOR,
			FaunaPostID: string(rune('a' + i)),
		}}); err != nil {
			t.Fatalf("commit %d: %v", i, err)
		}
	}
	return store, path, rkeys
}

// dropBlock deletes one block from the repo's blockstore — a corrupt tree, the
// only way ExportRepoTo fails on a repo requireServed has already admitted.
func dropBlock(t *testing.T, path, did, cidStr string) {
	t.Helper()
	c, err := cid.Decode(cidStr)
	if err != nil {
		t.Fatalf("decode cid %q: %v", cidStr, err)
	}
	db, err := sql.Open("sqlite", "file:"+path+"?_pragma=busy_timeout(5000)")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	res, err := db.Exec(`DELETE FROM repo_blocks WHERE did = ? AND cid_keystring = ?`, did, []byte(c.KeyString()))
	if err != nil {
		t.Fatalf("drop block: %v", err)
	}
	if n, _ := res.RowsAffected(); n != 1 {
		t.Fatalf("drop block %s: %d rows affected, want 1", cidStr, n)
	}
}

// TestGetRepoTearsDownRatherThanServeATruncatedCAR is the error half of streaming.
//
// Streaming a whole-repo CAR is what keeps an anonymous call from materialising
// the repo in memory, but it spends the status code on the first byte. So a
// failure part-way through the walk must NOT be allowed to end the response
// normally: Go would close a well-formed chunked body and the consumer would read
// a CAR that parses fine while silently missing records — a repo quietly short of
// the truth is worse than no answer. The handler aborts the response instead, so
// the consumer's read fails.
//
// Mutation barrier: replace the abort with a plain `return` and the ReadAll below
// succeeds, turning this red.
func TestGetRepoTearsDownRatherThanServeATruncatedCAR(t *testing.T) {
	ctx := context.Background()
	store, path, rkeys := seedFileRepo(t, 6)
	ts := serve(t, store)

	// Baseline: the whole repo serves cleanly, and it is more than one block —
	// otherwise "truncated" and "complete" would be the same bytes.
	full, err := store.ExportRepo(ctx, testDID)
	if err != nil {
		t.Fatal(err)
	}
	resp, err := ts.Client().Get(ts.URL + "/xrpc/com.atproto.sync.getRepo?did=" + testDID)
	if err != nil {
		t.Fatal(err)
	}
	body, rerr := io.ReadAll(resp.Body)
	resp.Body.Close()
	if rerr != nil {
		t.Fatalf("healthy getRepo body read failed: %v", rerr)
	}
	if len(body) != len(full) || resp.StatusCode != http.StatusOK {
		t.Fatalf("healthy getRepo = %d bytes / status %d, want %d bytes / 200", len(body), resp.StatusCode, len(full))
	}

	// Corrupt a RECORD block. The walk emits the head commit and the MST nodes
	// first, so the failure lands after bytes are already on the wire — the case
	// a status code can no longer describe.
	recordCID, _, ok, err := store.GetRecord(ctx, testDID, feedPostNSID, rkeys[len(rkeys)-1])
	if err != nil || !ok {
		t.Fatalf("GetRecord for the block to drop: ok=%v err=%v", ok, err)
	}
	dropBlock(t, path, testDID, recordCID)

	resp, err = ts.Client().Get(ts.URL + "/xrpc/com.atproto.sync.getRepo?did=" + testDID)
	if err != nil {
		// A torn-down response may also surface as a transport error here, which
		// is the same verdict: the consumer does not get a complete body.
		return
	}
	body, rerr = io.ReadAll(resp.Body)
	resp.Body.Close()
	if rerr == nil {
		t.Errorf("corrupt-repo getRepo returned a COMPLETE %d-byte body (status %d) — a partial CAR must not read as a whole repo", len(body), resp.StatusCode)
	}
	if len(body) >= len(full) {
		t.Errorf("corrupt-repo getRepo sent %d bytes, want fewer than the healthy %d", len(body), len(full))
	}
}

// TestGetRepoOnAnUnservableRepoIsAnInternalErrorNotRepoNotFound: requireServed
// has already resolved the DID as existing and active, so an export failure from
// there on is OUR storage failing. Answering the caller's own RepoNotFound made a
// corrupt tree indistinguishable from a mistyped DID and produced no operator
// signal at all — the export error was swallowed unlogged. The head commit is
// loaded before the first byte precisely so this case still gets a clean status.
func TestGetRepoOnAnUnservableRepoIsAnInternalErrorNotRepoNotFound(t *testing.T) {
	ctx := context.Background()
	store, path, _ := seedFileRepo(t, 3)
	ts := serve(t, store)

	_, headCommit, ok, err := store.Head(ctx, testDID)
	if err != nil || !ok {
		t.Fatalf("Head: ok=%v err=%v", ok, err)
	}
	dropBlock(t, path, testDID, headCommit)

	status, body := getJSON(t, ts, "/xrpc/com.atproto.sync.getRepo?did="+testDID)
	if status != http.StatusInternalServerError {
		t.Errorf("getRepo on a repo whose head block is gone = status %d, want 500", status)
	}
	if body["error"] == "RepoNotFound" {
		t.Error("a corrupt served repo answered RepoNotFound — indistinguishable from an unknown DID, and no signal that storage is broken")
	}
}
