package atprotoread

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/repo"
	"github.com/bluesky-social/indigo/atproto/syntax"
	car "github.com/ipld/go-car/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

const (
	testDID         = "did:plc:read0000000000000000000000"
	feedPostNSID    = "app.bsky.feed.post"
	testPostFaunaID = "aa" // short hex placeholder for post_map
)

// testClock is the ONE TID source for every repo this package's tests build —
// commit revs and record rkeys alike. A per-helper clock, or a funnel's default
// wall-clock one, restarts from the wall clock, so where that clock is coarser
// than the gap between two setup steps (measured on Windows, 2026-09-14) its
// first TID equals the previous step's last: the second funnel refuses with
// `rev clock regressed`, or a create silently overwrites an existing rkey (a
// three-record fixture serving two). One shared clock is strictly increasing by
// construction, so no fixture depends on the wall clock ticking between steps.
var testClock = atprotorepo.NewTIDRevClock()

// seedRepo opens an in-memory store, projects one post record into testDID's
// repo, and returns the store + the record's rkey.
func seedRepo(t *testing.T) (*atprotorepo.Store, string) {
	t.Helper()
	ctx := context.Background()
	store, err := atprotorepo.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close() })
	funnel, err := atprotorepo.NewFunnel(ctx, store, testClock)
	if err != nil {
		t.Fatal(err)
	}
	// The funnel refuses to emit for an identity that has never been announced
	// (atprotorepo.ErrFirstEmitGated). These are read-surface tests, so the
	// account is past that point exactly as it is in production before any
	// record is served.
	announced(t, store, testDID)
	signer, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	recCBOR, err := atprotorepo.JSONRecordToDagCBOR(
		`{"$type":"app.bsky.feed.post","text":"hello","createdAt":"2026-01-01T00:00:00Z"}`)
	if err != nil {
		t.Fatal(err)
	}
	rkey := testClock.Next()
	if _, err := funnel.ApplyBatch(ctx, testDID, signer, []atprotorepo.RepoOp{{
		Action:      atprotorepo.ActionCreate,
		Collection:  feedPostNSID,
		Rkey:        rkey,
		RecordCBOR:  recCBOR,
		FaunaPostID: testPostFaunaID,
	}}); err != nil {
		t.Fatal(err)
	}
	return store, rkey
}

// seedRepoRecords is seedRepo with n records instead of one, so a test can tell
// a proof CAR apart from a whole-repo CAR (with a single record the two are the
// same bytes). Returns the store and the rkeys in commit order.
func seedRepoRecords(t *testing.T, n int) (*atprotorepo.Store, []string) {
	t.Helper()
	ctx := context.Background()
	store, err := atprotorepo.Open(":memory:")
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
		recCBOR, err := atprotorepo.JSONRecordToDagCBOR(fmt.Sprintf(
			`{"$type":"app.bsky.feed.post","text":"post %d","createdAt":"2026-01-01T00:00:00Z"}`, i))
		if err != nil {
			t.Fatal(err)
		}
		rkeys[i] = testClock.Next()
		if _, err := funnel.ApplyBatch(ctx, testDID, signer, []atprotorepo.RepoOp{{
			Action:      atprotorepo.ActionCreate,
			Collection:  feedPostNSID,
			Rkey:        rkeys[i],
			RecordCBOR:  recCBOR,
			FaunaPostID: fmt.Sprintf("%04x", i),
		}}); err != nil {
			t.Fatalf("commit %d: %v", i, err)
		}
	}
	return store, rkeys
}

// serve registers the read surface on a real xrpc.Server and returns an httptest
// server plus its base URL.
func serve(t *testing.T, store *atprotorepo.Store) *httptest.Server {
	t.Helper()
	srv := xrpc.NewServer(nil, nil, nil, nil, slog.New(slog.NewTextHandler(io.Discard, nil)))
	Register(srv, store, nil)
	ts := httptest.NewServer(srv)
	t.Cleanup(ts.Close)
	return ts
}

func getJSON(t *testing.T, ts *httptest.Server, path string) (int, map[string]any) {
	t.Helper()
	resp, err := ts.Client().Get(ts.URL + path)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	var m map[string]any
	if len(body) > 0 && resp.Header.Get("Content-Type") == "application/json" {
		_ = json.Unmarshal(body, &m)
	}
	return resp.StatusCode, m
}

func TestGetRepoServesCAR(t *testing.T) {
	store, _ := seedRepo(t)
	ts := serve(t, store)

	resp, err := ts.Client().Get(ts.URL + "/xrpc/com.atproto.sync.getRepo?did=" + testDID)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("getRepo status = %d, want 200", resp.StatusCode)
	}
	if ct := resp.Header.Get("Content-Type"); ct != contentTypeCAR {
		t.Errorf("getRepo content-type = %q, want %q", ct, contentTypeCAR)
	}
	body, _ := io.ReadAll(resp.Body)
	if len(body) == 0 {
		t.Error("getRepo returned an empty CAR")
	}
}

func TestGetRepoUnknownDIDIsRepoNotFound(t *testing.T) {
	store, _ := seedRepo(t)
	ts := serve(t, store)

	status, body := getJSON(t, ts, "/xrpc/com.atproto.sync.getRepo?did=did:plc:nope000000000000000000000000")
	if status != http.StatusBadRequest {
		t.Fatalf("unknown-did getRepo status = %d, want 400", status)
	}
	if body["error"] != "RepoNotFound" {
		t.Errorf("error = %v, want RepoNotFound", body["error"])
	}
}

func TestGetRepoMissingDIDIsInvalidRequest(t *testing.T) {
	store, _ := seedRepo(t)
	ts := serve(t, store)

	status, body := getJSON(t, ts, "/xrpc/com.atproto.sync.getRepo")
	if status != http.StatusBadRequest || body["error"] != "InvalidRequest" {
		t.Errorf("missing-did getRepo = (%d, %v), want (400, InvalidRequest)", status, body["error"])
	}
}

func TestGetLatestCommitAndStatus(t *testing.T) {
	store, _ := seedRepo(t)
	ts := serve(t, store)
	rev, commitCID, _, _ := store.Head(context.Background(), testDID)

	status, body := getJSON(t, ts, "/xrpc/com.atproto.sync.getLatestCommit?did="+testDID)
	if status != http.StatusOK {
		t.Fatalf("getLatestCommit status = %d", status)
	}
	if body["cid"] != commitCID || body["rev"] != rev {
		t.Errorf("getLatestCommit = %v, want cid=%s rev=%s", body, commitCID, rev)
	}

	status, body = getJSON(t, ts, "/xrpc/com.atproto.sync.getRepoStatus?did="+testDID)
	if status != http.StatusOK || body["active"] != true || body["rev"] != rev {
		t.Errorf("getRepoStatus = (%d, %v)", status, body)
	}
}

func TestListRepos(t *testing.T) {
	store, _ := seedRepo(t)
	ts := serve(t, store)

	status, body := getJSON(t, ts, "/xrpc/com.atproto.sync.listRepos")
	if status != http.StatusOK {
		t.Fatalf("listRepos status = %d", status)
	}
	repos, _ := body["repos"].([]any)
	if len(repos) != 1 {
		t.Fatalf("listRepos returned %d repos, want 1", len(repos))
	}
	first, _ := repos[0].(map[string]any)
	if first["did"] != testDID || first["active"] != true {
		t.Errorf("listRepos[0] = %v", first)
	}
}

func TestRepoGetRecord(t *testing.T) {
	store, rkey := seedRepo(t)
	ts := serve(t, store)

	status, body := getJSON(t, ts,
		"/xrpc/com.atproto.repo.getRecord?repo="+testDID+"&collection="+feedPostNSID+"&rkey="+rkey)
	if status != http.StatusOK {
		t.Fatalf("repo.getRecord status = %d, body=%v", status, body)
	}
	if body["uri"] != "at://"+testDID+"/"+feedPostNSID+"/"+rkey {
		t.Errorf("uri = %v", body["uri"])
	}
	value, _ := body["value"].(map[string]any)
	if value["text"] != "hello" || value["$type"] != feedPostNSID {
		t.Errorf("decoded record value = %v, want text=hello $type=%s", value, feedPostNSID)
	}
}

func TestRepoGetRecordNotFound(t *testing.T) {
	store, _ := seedRepo(t)
	ts := serve(t, store)

	status, body := getJSON(t, ts,
		"/xrpc/com.atproto.repo.getRecord?repo="+testDID+"&collection="+feedPostNSID+"&rkey=3knonexistent0")
	if status != http.StatusBadRequest || body["error"] != "RecordNotFound" {
		t.Errorf("missing record = (%d, %v), want (400, RecordNotFound)", status, body["error"])
	}
}

func TestRepoListRecords(t *testing.T) {
	store, rkey := seedRepo(t)
	ts := serve(t, store)

	status, body := getJSON(t, ts, "/xrpc/com.atproto.repo.listRecords?repo="+testDID+"&collection="+feedPostNSID)
	if status != http.StatusOK {
		t.Fatalf("listRecords status = %d", status)
	}
	records, _ := body["records"].([]any)
	if len(records) != 1 {
		t.Fatalf("listRecords returned %d, want 1", len(records))
	}
	rec, _ := records[0].(map[string]any)
	if rec["uri"] != "at://"+testDID+"/"+feedPostNSID+"/"+rkey {
		t.Errorf("listRecords[0].uri = %v", rec["uri"])
	}
}

func TestDescribeRepo(t *testing.T) {
	store, _ := seedRepo(t)
	ts := serve(t, store)

	status, body := getJSON(t, ts, "/xrpc/com.atproto.repo.describeRepo?repo="+testDID)
	if status != http.StatusOK {
		t.Fatalf("describeRepo status = %d", status)
	}
	if body["did"] != testDID {
		t.Errorf("describeRepo did = %v", body["did"])
	}
	cols, _ := body["collections"].([]any)
	if len(cols) != 1 || cols[0] != feedPostNSID {
		t.Errorf("describeRepo collections = %v, want [%s]", cols, feedPostNSID)
	}
}

// TestSyncGetRecordServesProofCAR: over the real HTTP surface, sync.getRecord
// answers a PROOF CAR — indigo's own repo loader (what a relay runs) resolves
// the requested record out of the served bytes, and those bytes carry no OTHER
// record's block. Asserting only "a non-empty CAR" would pass just as happily
// for the whole-repo payload this replaced, which is the amplification the
// slice exists to close, so the response is checked for what it must NOT
// contain as well as what it must.
func TestSyncGetRecordServesProofCAR(t *testing.T) {
	ctx := context.Background()
	store, rkeys := seedRepoRecords(t, 25)
	ts := serve(t, store)
	target := rkeys[len(rkeys)/2]

	resp, err := ts.Client().Get(ts.URL +
		"/xrpc/com.atproto.sync.getRecord?did=" + testDID + "&collection=" + feedPostNSID + "&rkey=" + target)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK || resp.Header.Get("Content-Type") != contentTypeCAR {
		t.Fatalf("sync.getRecord = (%d, %s), want (200, CAR)", resp.StatusCode, resp.Header.Get("Content-Type"))
	}
	body, _ := io.ReadAll(resp.Body)
	if len(body) == 0 {
		t.Fatal("sync.getRecord returned empty CAR")
	}

	wantCID, wantBytes, ok, err := store.GetRecord(ctx, testDID, feedPostNSID, target)
	if err != nil || !ok {
		t.Fatalf("seeded record missing: ok=%v err=%v", ok, err)
	}
	_, rp, err := repo.LoadRepoFromCAR(ctx, bytes.NewReader(body))
	if err != nil {
		t.Fatalf("indigo rejected the served proof CAR: %v", err)
	}
	gotBytes, gotCID, err := rp.GetRecordBytes(ctx, syntax.NSID(feedPostNSID), syntax.RecordKey(target))
	if err != nil {
		t.Fatalf("indigo could not resolve the record through the served proof: %v", err)
	}
	if gotCID.String() != wantCID || !bytes.Equal(gotBytes, wantBytes) {
		t.Errorf("served proof resolved CID %s, want %s (bytes equal: %v)", gotCID, wantCID, bytes.Equal(gotBytes, wantBytes))
	}

	// No foreign record block may ride along.
	served := map[string]bool{}
	br, err := car.NewBlockReader(bytes.NewReader(body))
	if err != nil {
		t.Fatalf("go-car/v2 rejected the served CAR: %v", err)
	}
	for {
		blk, err := br.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatalf("go-car/v2 could not verify a served block: %v", err)
		}
		served[blk.Cid().String()] = true
	}
	recs, err := store.ListRecords(ctx, testDID, feedPostNSID)
	if err != nil {
		t.Fatal(err)
	}
	if len(recs) != len(rkeys) {
		t.Fatalf("seeded %d records, store has %d", len(rkeys), len(recs))
	}
	for _, r := range recs {
		if r.Rkey != target && served[r.RecordCID] {
			t.Errorf("sync.getRecord leaked a foreign record block: rkey=%s cid=%s", r.Rkey, r.RecordCID)
		}
	}

	status, ebody := getJSON(t, ts,
		"/xrpc/com.atproto.sync.getRecord?did="+testDID+"&collection="+feedPostNSID+"&rkey=3knope00000")
	if status != http.StatusBadRequest || ebody["error"] != "RecordNotFound" {
		t.Errorf("missing sync.getRecord = (%d, %v)", status, ebody["error"])
	}
}

// TestDeactivatedRepoIsUnservedButListed is the S4-D read-surface proof: after a
// layer-2 step-down (repo_heads.active=0) every CONTENT read refuses with
// RepoDeactivated — including for an EXISTING record, so a deactivated repo never
// leaks what it holds — while getRepoStatus REPORTS active:false + status and
// listRepos still lists it marked inactive (the Sync v1.1 shape). Re-entry
// restores serving of the same repo with no data change.
func TestDeactivatedRepoIsUnservedButListed(t *testing.T) {
	store, rkey := seedRepo(t)
	ts := serve(t, store)
	ctx := context.Background()

	if err := store.SetRepoActive(ctx, testDID, false); err != nil {
		t.Fatal(err)
	}

	// Every content read refuses with RepoDeactivated — deactivation is checked
	// before record existence, so an existing record leaks nothing.
	contentReads := map[string]string{
		"sync.getRepo":         "/xrpc/com.atproto.sync.getRepo?did=" + testDID,
		"sync.getLatestCommit": "/xrpc/com.atproto.sync.getLatestCommit?did=" + testDID,
		"sync.getRecord":       "/xrpc/com.atproto.sync.getRecord?did=" + testDID + "&collection=" + feedPostNSID + "&rkey=" + rkey,
		"repo.getRecord":       "/xrpc/com.atproto.repo.getRecord?repo=" + testDID + "&collection=" + feedPostNSID + "&rkey=" + rkey,
		"repo.listRecords":     "/xrpc/com.atproto.repo.listRecords?repo=" + testDID + "&collection=" + feedPostNSID,
		"repo.describeRepo":    "/xrpc/com.atproto.repo.describeRepo?repo=" + testDID,
		// The blob reads take the same guard. getBlob names a CID this repo does
		// not hold ON PURPOSE: refusing RepoDeactivated rather than BlobNotFound
		// is what proves deactivation is checked BEFORE existence, so a
		// deactivated repo cannot be probed for which media it holds.
		"sync.getBlob":   "/xrpc/com.atproto.sync.getBlob?did=" + testDID + "&cid=bafkreiabsent",
		"sync.listBlobs": "/xrpc/com.atproto.sync.listBlobs?did=" + testDID,
	}
	for name, path := range contentReads {
		status, body := getJSON(t, ts, path)
		if status != http.StatusBadRequest || body["error"] != "RepoDeactivated" {
			t.Errorf("%s on a deactivated repo = (%d, %v), want (400, RepoDeactivated)", name, status, body["error"])
		}
	}

	// getRepoStatus reports the status rather than refusing.
	status, body := getJSON(t, ts, "/xrpc/com.atproto.sync.getRepoStatus?did="+testDID)
	if status != http.StatusOK {
		t.Fatalf("getRepoStatus on a deactivated repo status = %d, want 200", status)
	}
	if body["active"] != false || body["status"] != atprotorepo.AccountStatusDeactivated {
		t.Errorf("getRepoStatus = %v, want active:false status:%q", body, atprotorepo.AccountStatusDeactivated)
	}

	// listRepos still lists it, marked inactive with a status reason.
	status, body = getJSON(t, ts, "/xrpc/com.atproto.sync.listRepos")
	if status != http.StatusOK {
		t.Fatalf("listRepos status = %d", status)
	}
	repos, _ := body["repos"].([]any)
	if len(repos) != 1 {
		t.Fatalf("listRepos returned %d repos, want the deactivated one still listed", len(repos))
	}
	first, _ := repos[0].(map[string]any)
	if first["did"] != testDID || first["active"] != false || first["status"] != atprotorepo.AccountStatusDeactivated {
		t.Errorf("listRepos[0] = %v, want did=%s active:false status:%q", first, testDID, atprotorepo.AccountStatusDeactivated)
	}

	// Re-entry restores serving: the SAME repo serves again with no data change.
	if err := store.SetRepoActive(ctx, testDID, true); err != nil {
		t.Fatal(err)
	}
	resp, err := ts.Client().Get(ts.URL + "/xrpc/com.atproto.sync.getRepo?did=" + testDID)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("getRepo after re-entry status = %d, want 200", resp.StatusCode)
	}
}

// ── S5: cursor pagination on the two list endpoints ──────────────────────────

// seedRecords adds n extra posts to testDID's repo and returns every rkey in
// the collection, sorted — the order listRecords must walk.
func seedRecords(t *testing.T, store *atprotorepo.Store, n int) []string {
	t.Helper()
	ctx := context.Background()
	funnel, err := atprotorepo.NewFunnel(ctx, store, testClock)
	if err != nil {
		t.Fatal(err)
	}
	announced(t, store, testDID)
	signer, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	for i := 0; i < n; i++ {
		recCBOR, err := atprotorepo.JSONRecordToDagCBOR(
			fmt.Sprintf(`{"$type":"app.bsky.feed.post","text":"post %d","createdAt":"2026-01-02T00:00:00Z"}`, i))
		if err != nil {
			t.Fatal(err)
		}
		if _, err := funnel.ApplyBatch(ctx, testDID, signer, []atprotorepo.RepoOp{{
			Action: atprotorepo.ActionCreate, Collection: feedPostNSID,
			Rkey: testClock.Next(), RecordCBOR: recCBOR,
			FaunaPostID: fmt.Sprintf("seed%02d", i),
		}}); err != nil {
			t.Fatal(err)
		}
	}
	all, err := store.ListRecords(ctx, testDID, feedPostNSID)
	if err != nil {
		t.Fatal(err)
	}
	rkeys := make([]string, 0, len(all))
	for _, r := range all {
		rkeys = append(rkeys, r.Rkey)
	}
	return rkeys
}

// TestListRecordsPaginatesAndTerminates: the unauthenticated listRecords must
// bound one response and hand back a cursor while more remains — walking the
// cursor visits every record exactly once, in order, and the final short page
// omits the cursor so a relay knows to stop.
func TestListRecordsPaginatesAndTerminates(t *testing.T) {
	store, _ := seedRepo(t)
	want := seedRecords(t, store, 4) // 5 records total (seedRepo wrote one)
	ts := serve(t, store)

	var got []string
	cursor := ""
	for pages := 0; ; pages++ {
		if pages > 10 {
			t.Fatal("cursor walk did not terminate")
		}
		path := "/xrpc/com.atproto.repo.listRecords?repo=" + testDID + "&collection=" + feedPostNSID + "&limit=2"
		if cursor != "" {
			path += "&cursor=" + cursor
		}
		code, body := getJSON(t, ts, path)
		if code != 200 {
			t.Fatalf("listRecords page %d = %d: %v", pages, code, body)
		}
		records, _ := body["records"].([]any)
		if len(records) > 2 {
			t.Fatalf("page %d returned %d records, over the requested limit of 2", pages, len(records))
		}
		for _, rec := range records {
			uri, _ := rec.(map[string]any)["uri"].(string)
			got = append(got, uri[strings.LastIndex(uri, "/")+1:])
		}
		next, hasCursor := body["cursor"].(string)
		if !hasCursor {
			if len(records) == 2 {
				t.Errorf("page %d was full but carried no cursor — the walk would stop early", pages)
			}
			break
		}
		cursor = next
	}

	if len(got) != len(want) {
		t.Fatalf("cursor walk visited %d records, want %d (%v vs %v)", len(got), len(want), got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("cursor walk order diverged at %d: got %q want %q", i, got[i], want[i])
		}
	}
}

// TestListRecordsCeilingBoundsAnAnonymousCaller is the hardening assertion: the
// ceiling, not the caller, decides how much work one anonymous request buys.
func TestListRecordsCeilingBoundsAnAnonymousCaller(t *testing.T) {
	store, _ := seedRepo(t)
	seedRecords(t, store, listRecordsMaxLimit+20)
	ts := serve(t, store)

	code, body := getJSON(t, ts,
		"/xrpc/com.atproto.repo.listRecords?repo="+testDID+"&collection="+feedPostNSID+"&limit=100000")
	if code != 200 {
		t.Fatalf("listRecords = %d: %v", code, body)
	}
	records, _ := body["records"].([]any)
	if len(records) != listRecordsMaxLimit {
		t.Errorf("limit=100000 served %d records, want the ceiling %d", len(records), listRecordsMaxLimit)
	}
	if _, ok := body["cursor"].(string); !ok {
		t.Error("a ceiling-clamped page carried no cursor — the rest would be unreachable")
	}
}

// TestListLimitFallbacks: absent/garbage/zero/negative all fall back to the
// default rather than erroring — refusing would only hand an anonymous caller a
// way to make the endpoint fail, and the ceiling is the actual protection.
func TestListLimitFallbacks(t *testing.T) {
	store, _ := seedRepo(t)
	seedRecords(t, store, 2) // 3 records, under any default
	ts := serve(t, store)

	for _, q := range []string{"", "&limit=", "&limit=abc", "&limit=0", "&limit=-5"} {
		code, body := getJSON(t, ts,
			"/xrpc/com.atproto.repo.listRecords?repo="+testDID+"&collection="+feedPostNSID+q)
		if code != 200 {
			t.Errorf("listRecords%q = %d: %v", q, code, body)
			continue
		}
		records, _ := body["records"].([]any)
		if len(records) != 3 {
			t.Errorf("listRecords%q served %d records, want all 3 under the default", q, len(records))
		}
		if _, ok := body["cursor"].(string); ok {
			t.Errorf("listRecords%q carried a cursor on a short page", q)
		}
	}
}

// TestListReposPaginates: the same contract on sync.listRepos, whose keyset is
// the DID. The INTERNAL complete listing must stay complete — the firehose
// #sync degrade and the boot self-check both depend on seeing every repo — so
// that is asserted alongside.
func TestListReposPaginates(t *testing.T) {
	store, _ := seedRepo(t)
	ctx := context.Background()
	funnel, err := atprotorepo.NewFunnel(ctx, store, testClock)
	if err != nil {
		t.Fatal(err)
	}
	signer, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	recCBOR, err := atprotorepo.JSONRecordToDagCBOR(
		`{"$type":"app.bsky.feed.post","text":"other","createdAt":"2026-01-01T00:00:00Z"}`)
	if err != nil {
		t.Fatal(err)
	}
	for _, did := range []string{"did:web:bob.example", "did:web:carol.example"} {
		announced(t, store, did)
		if _, err := funnel.ApplyBatch(ctx, did, signer, []atprotorepo.RepoOp{{
			Action: atprotorepo.ActionCreate, Collection: feedPostNSID,
			Rkey: testClock.Next(), RecordCBOR: recCBOR,
		}}); err != nil {
			t.Fatal(err)
		}
	}
	ts := serve(t, store)

	var walked []string
	cursor := ""
	for pages := 0; ; pages++ {
		if pages > 10 {
			t.Fatal("listRepos cursor walk did not terminate")
		}
		path := "/xrpc/com.atproto.sync.listRepos?limit=1"
		if cursor != "" {
			path += "&cursor=" + cursor
		}
		code, body := getJSON(t, ts, path)
		if code != 200 {
			t.Fatalf("listRepos page %d = %d: %v", pages, code, body)
		}
		repos, _ := body["repos"].([]any)
		if len(repos) > 1 {
			t.Fatalf("page %d returned %d repos, over the requested limit of 1", pages, len(repos))
		}
		for _, rp := range repos {
			did, _ := rp.(map[string]any)["did"].(string)
			walked = append(walked, did)
		}
		next, hasCursor := body["cursor"].(string)
		if !hasCursor {
			break
		}
		cursor = next
	}

	all, err := store.ListRepos(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(all) != 3 {
		t.Fatalf("internal ListRepos returned %d repos, want all 3 — the #sync degrade and boot self-check need every one", len(all))
	}
	if len(walked) != 3 {
		t.Fatalf("paged walk visited %d repos, want 3: %v", len(walked), walked)
	}
	for i, rp := range all {
		if walked[i] != rp.DID {
			t.Errorf("paged walk diverged at %d: got %q want %q", i, walked[i], rp.DID)
		}
	}
}

// seedBlob stores one blob in testDID's repo and returns its ATProto CID.
func seedBlob(t *testing.T, store *atprotorepo.Store, data []byte, faunaCID string) string {
	t.Helper()
	c, err := atprotorepo.BlobCIDForBytes(data)
	if err != nil {
		t.Fatal(err)
	}
	if err := store.PutBlob(context.Background(), testDID, c.String(), faunaCID, "image/png", data); err != nil {
		t.Fatal(err)
	}
	return c.String()
}

// TestGetBlobServesBytes: the endpoint an AppView calls to render an image
// embed hands back exactly the bytes the record's blob ref names — the whole
// point of storing them, and the reason a ref is never dangling.
func TestGetBlobServesBytes(t *testing.T) {
	store, _ := seedRepo(t)
	data := []byte("png-bytes")
	blobCID := seedBlob(t, store, data, "fauna-1")
	ts := serve(t, store)

	resp, err := ts.Client().Get(ts.URL + "/xrpc/com.atproto.sync.getBlob?did=" + testDID + "&cid=" + blobCID)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("getBlob status = %d, want 200", resp.StatusCode)
	}
	got, _ := io.ReadAll(resp.Body)
	if string(got) != string(data) {
		t.Fatalf("getBlob served %q, want %q", got, data)
	}
	// Unauthenticated + navigable, so the served type must stay inert and
	// unsniffable — the same rule the nest's own blob route follows.
	if ct := resp.Header.Get("Content-Type"); ct != "application/octet-stream" {
		t.Errorf("Content-Type = %q, want application/octet-stream", ct)
	}
	if nosniff := resp.Header.Get("X-Content-Type-Options"); nosniff != "nosniff" {
		t.Errorf("X-Content-Type-Options = %q, want nosniff", nosniff)
	}
	if etag := resp.Header.Get("ETag"); etag != `"`+blobCID+`"` {
		t.Errorf("ETag = %q, want the blob CID", etag)
	}
}

// TestGetBlobUnknownAndMissingParams: an absent blob is BlobNotFound, not a 500
// and not someone else's bytes.
func TestGetBlobUnknownAndMissingParams(t *testing.T) {
	store, _ := seedRepo(t)
	seedBlob(t, store, []byte("png-bytes"), "fauna-1")
	ts := serve(t, store)

	for name, path := range map[string]string{
		"unknown cid": "/xrpc/com.atproto.sync.getBlob?did=" + testDID + "&cid=bafkreiabsent",
		"no cid":      "/xrpc/com.atproto.sync.getBlob?did=" + testDID,
		"no did":      "/xrpc/com.atproto.sync.getBlob?cid=bafkreiabsent",
		"other repo":  "/xrpc/com.atproto.sync.getBlob?did=did:plc:someone000000000000000000&cid=bafkreiabsent",
	} {
		status, body := getJSON(t, ts, path)
		if status != http.StatusBadRequest {
			t.Errorf("getBlob(%s) status = %d, want 400 (%v)", name, status, body)
		}
	}
}

// TestListBlobsPaginates: same keyset-cursor contract as listRepos/listRecords —
// a full page carries a cursor, the walk covers every blob exactly once, and the
// caller's limit is clamped by the ceiling rather than obeyed unboundedly.
func TestListBlobsPaginates(t *testing.T) {
	store, _ := seedRepo(t)
	want := map[string]bool{}
	for i := 0; i < 5; i++ {
		want[seedBlob(t, store, []byte(fmt.Sprintf("blob-%d", i)), fmt.Sprintf("f-%d", i))] = true
	}
	ts := serve(t, store)

	seen := map[string]bool{}
	cursor := ""
	for pages := 0; ; pages++ {
		if pages > 10 {
			t.Fatal("cursor walk did not terminate")
		}
		path := "/xrpc/com.atproto.sync.listBlobs?did=" + testDID + "&limit=2"
		if cursor != "" {
			path += "&cursor=" + cursor
		}
		status, body := getJSON(t, ts, path)
		if status != http.StatusOK {
			t.Fatalf("listBlobs = %d: %v", status, body)
		}
		cids, _ := body["cids"].([]any)
		if len(cids) > 2 {
			t.Fatalf("page served %d cids past the requested limit", len(cids))
		}
		for _, c := range cids {
			s, _ := c.(string)
			if seen[s] {
				t.Fatalf("cursor walk repeated %s", s)
			}
			seen[s] = true
		}
		next, ok := body["cursor"].(string)
		if !ok {
			break
		}
		cursor = next
	}
	if len(seen) != len(want) {
		t.Fatalf("walked %d blobs, stored %d", len(seen), len(want))
	}
	for c := range want {
		if !seen[c] {
			t.Errorf("blob %s never appeared in the walk", c)
		}
	}
}

// TestListBlobsEmptyRepoServesEmptyArray: `cids` is a required array in the
// lexicon, so a repo with no media answers [] — never null, which a strict
// consumer rejects.
func TestListBlobsEmptyRepoServesEmptyArray(t *testing.T) {
	store, _ := seedRepo(t)
	ts := serve(t, store)

	status, body := getJSON(t, ts, "/xrpc/com.atproto.sync.listBlobs?did="+testDID)
	if status != http.StatusOK {
		t.Fatalf("listBlobs = %d: %v", status, body)
	}
	cids, ok := body["cids"].([]any)
	if !ok {
		t.Fatalf("cids = %#v, want an empty array", body["cids"])
	}
	if len(cids) != 0 {
		t.Fatalf("cids = %v, want empty", cids)
	}
	if _, hasCursor := body["cursor"]; hasCursor {
		t.Error("an empty listing must not carry a cursor")
	}
}

// TestListBlobsIgnoresSince: `since` is accepted and ignored — the full set is a
// conformant superset of any since-answer, whereas honouring it wrongly would
// silently withhold blobs a migrating consumer needs.
func TestListBlobsIgnoresSince(t *testing.T) {
	store, _ := seedRepo(t)
	blobCID := seedBlob(t, store, []byte("png-bytes"), "fauna-1")
	ts := serve(t, store)

	status, body := getJSON(t, ts,
		"/xrpc/com.atproto.sync.listBlobs?did="+testDID+"&since=9999999999999")
	if status != http.StatusOK {
		t.Fatalf("listBlobs = %d: %v", status, body)
	}
	cids, _ := body["cids"].([]any)
	if len(cids) != 1 || cids[0] != blobCID {
		t.Fatalf("cids = %v, want the full set [%s]", cids, blobCID)
	}
}

// announced opens did's first-emit gate — the projection loop's blessing that
// the identity resolves ecosystem-side, which every firehose producer requires
// (atprotorepo.ErrFirstEmitGated). A fresh store gates every DID, so a fixture
// seeding a repo declares its account announced, as production does once per
// identity before its first frame.
func announced(t *testing.T, store *atprotorepo.Store, did string) {
	t.Helper()
	if err := store.SetFirstEmitGated(context.Background(), did, false); err != nil {
		t.Fatalf("open first-emit gate for %s: %v", did, err)
	}
}
