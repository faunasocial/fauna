package atprotopds

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// fakeWriter records what the handler asked the funnel to commit. It is the
// half of the write path these tests assert hardest on: the nest decides
// WHERE a record lands and WHAT Fauna post it maps from, and this fake is
// where we can see whether the handler actually honoured that.
type fakeWriter struct {
	mu sync.Mutex

	ops        [][]atprotorepo.RepoOp
	commitDIDs []string
	rkeySeq    int
	postIDs    map[string]string // "collection/rkey" → fauna post id
	// refs stands in for the FFI extraction + post_map resolution: AT-URI →
	// fauna post id for targets this "repo" knows. Anything absent resolves to
	// nothing, which is how a non-Fauna target reaches the nest.
	refs map[string]string
	// resolveCalls records the records handed to ResolveRecordRefs, so a test
	// can prove the resolution ran BEFORE the nest call rather than after.
	resolveCalls [][]byte
	// blobRefs is what ExtractBlobRefs answers; blobWalkCalls records the
	// bytes it was asked to walk (the committed bytes, override included).
	blobRefs      []string
	blobWalkCalls [][]byte
	// publishedBlobs stands in for the blob store's reverse index: ATProto blob
	// CID → Fauna ContentHash, for blobs "this repo" has already published.
	// Anything absent resolves to nothing, which is how a ref the projection
	// never emitted reaches the nest unvouched-for.
	publishedBlobs   map[string]string
	blobResolveErr   error
	blobResolveCalls []blobResolveCall
	// gated stands in for the projection_state first-emit gate. The zero value
	// is OPEN, matching every test whose subject is not the gate: an account
	// whose identity already resolves.
	gated bool
	// headCID / hasHead stand in for the repo's current signed head, and
	// records for the live record set ("collection/rkey" → record CID). Both
	// are the CAS pre-check's operands; the zero value is a repo with no commit
	// and no records.
	headCID string
	hasHead bool
	records map[string]string
	// casOpts records the BatchOptions each Commit was given, so a test can
	// prove the expectations really reach the funnel — where the race-free
	// half of the CAS lives — rather than dying at the pre-check.
	casOpts [][]atprotorepo.BatchOption
	// commitErr, when set, is what Commit returns instead of succeeding. It is
	// how a test drives the funnel-side CAS loss the pre-check cannot see.
	commitErr error
	// renderedRecord stands in for the projector: what a projection pass would
	// commit for a collection it owns, which is what the nest's
	// `reproject_record` asks this side to produce. renderErr drives the
	// failure arm, and renderCalls records what was asked — so a test can prove
	// the write path asked the projection rather than rendering anything itself.
	renderedRecord string
	renderErr      error
	renderCalls    []renderCall
}

// blobResolveCall is one ResolveBlobRefs ask.
type blobResolveCall struct {
	DID    string
	Record []byte
}

// renderCall is one RenderProjectedRecord ask.
type renderCall struct {
	actorID    []byte
	did        string
	collection string
}

func newFakeWriter() *fakeWriter {
	return &fakeWriter{
		postIDs: map[string]string{},
		refs:    map[string]string{},
		records: map[string]string{},
	}
}

func (w *fakeWriter) Commit(
	_ context.Context, _ []byte, did string, ops []atprotorepo.RepoOp, opts ...atprotorepo.BatchOption,
) (atprotorepo.CommitResult, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.casOpts = append(w.casOpts, opts)
	if w.commitErr != nil {
		return atprotorepo.CommitResult{}, w.commitErr
	}
	w.ops = append(w.ops, ops)
	w.commitDIDs = append(w.commitDIDs, did)
	return atprotorepo.CommitResult{Rev: "3rev", CommitCID: "bafycommit", Ops: len(ops)}, nil
}

// errorName reads the XRPC wire error's `error` field. The NAME is the part
// third-party clients branch on — an InvalidSwap that arrived as a plain
// InvalidRequest would still be a 400, and would still be wrong.
// validationStatusOf reads the reply's validationStatus. Its own helper
// because the Lexicon-validation tests assert it on replies whose other fields
// they do not care about.
func validationStatusOf(t *testing.T, resp *http.Response) string {
	t.Helper()
	var got struct {
		ValidationStatus string `json:"validationStatus"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatalf("decode reply: %v", err)
	}
	return got.ValidationStatus
}

func errorName(t *testing.T, resp *http.Response) string {
	t.Helper()
	var body struct {
		Error string `json:"error"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
		t.Fatalf("decode xrpc error body: %v", err)
	}
	return body.Error
}

func (w *fakeWriter) RepoHead(_ context.Context, _ string) (string, bool, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.headCID, w.hasHead, nil
}

func (w *fakeWriter) RecordCID(_ context.Context, _, collection, rkey string) (string, bool, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	recordCID, ok := w.records[collection+"/"+rkey]
	return recordCID, ok, nil
}

func (w *fakeWriter) FaunaPostIDForRecord(_ context.Context, _, collection, rkey string) (string, bool, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	id, ok := w.postIDs[collection+"/"+rkey]
	return id, ok, nil
}

// ResolveRecordRefs fakes the shared-Rust extraction: the fixture record's
// reply parent / quote URI is whatever the test planted in `refs`, and only
// planted URIs resolve. The record bytes are recorded so a test can assert the
// call happened at all.
func (w *fakeWriter) ResolveRecordRefs(_ context.Context, recordCBOR []byte) (map[string]string, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.resolveCalls = append(w.resolveCalls, recordCBOR)
	if len(w.refs) == 0 {
		return nil, nil
	}
	out := make(map[string]string, len(w.refs))
	for uri, id := range w.refs {
		out[uri] = id
	}
	return out, nil
}

// ExtractBlobRefs fakes the shared-Rust blob walk: it returns whatever the
// test planted in `blobRefs` and records the exact bytes it was asked to walk,
// so a test can prove the fill runs on the COMMITTED bytes (override included)
// rather than on the caller's draft.
func (w *fakeWriter) ExtractBlobRefs(recordCBOR []byte) []string {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.blobWalkCalls = append(w.blobWalkCalls, recordCBOR)
	return w.blobRefs
}

// ResolveBlobRefs fakes the blob-store reverse index: only CIDs the test
// planted in `publishedBlobs` are bytes "this repo" already published. The
// calls are recorded so a test can prove the resolution happened at all, and
// for which collection — the scope decision is the production behaviour under
// test, not an implementation detail.
func (w *fakeWriter) ResolveBlobRefs(
	_ context.Context, did string, recordCBOR []byte,
) (map[string]string, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.blobResolveCalls = append(w.blobResolveCalls, blobResolveCall{DID: did, Record: recordCBOR})
	if w.blobResolveErr != nil {
		return nil, w.blobResolveErr
	}
	if len(w.publishedBlobs) == 0 {
		return nil, nil
	}
	out := make(map[string]string, len(w.publishedBlobs))
	for blobCID, faunaCID := range w.publishedBlobs {
		out[blobCID] = faunaCID
	}
	return out, nil
}

func (w *fakeWriter) FirstEmitGated(_ context.Context, _ string) (bool, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.gated, nil
}

func (w *fakeWriter) RenderProjectedRecord(
	_ context.Context, actorID []byte, did, collection string,
) (string, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.renderCalls = append(w.renderCalls, renderCall{
		actorID: bytes.Clone(actorID), did: did, collection: collection,
	})
	if w.renderErr != nil {
		return "", w.renderErr
	}
	return w.renderedRecord, nil
}

func (w *fakeWriter) NextRkey() string {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.rkeySeq++
	return fmt.Sprintf("3bridgerkey%d", w.rkeySeq)
}

func (w *fakeWriter) lastOps(t *testing.T) []atprotorepo.RepoOp {
	t.Helper()
	w.mu.Lock()
	defer w.mu.Unlock()
	if len(w.ops) == 0 {
		t.Fatal("nothing was committed to the repo")
	}
	return w.ops[len(w.ops)-1]
}

// commitCount is how many times the handler reached the funnel at all — the
// assertion for a path that must commit NOTHING.
func (w *fakeWriter) commitCount() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return len(w.ops)
}

// writeFixture is newFixture plus a wired RepoWriter.
func newWriteFixture(t *testing.T) (*fixture, *fakeWriter) {
	t.Helper()
	f := newFixture(t)
	w := newFakeWriter()
	f.server.EnableWrites(w)
	return f, w
}

// roundTripped is the nest answering "this became a real Fauna post": its own
// derived rkey plus the Fauna post id the bridge must map.
func roundTripped(rkey, faunaPostID string) func([]wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
	return func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		out := make([]wsrpc.ExternalWriteResult, len(ws))
		for i, w := range ws {
			uri := "at://did:fauna:x/" + w.Collection + "/" + rkey
			k, id := rkey, faunaPostID
			out[i] = wsrpc.ExternalWriteResult{Rkey: &k, AtURI: &uri, FaunaPostID: &id}
		}
		return out
	}
}

func refusedWith(subType, msg string) func([]wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
	return func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		out := make([]wsrpc.ExternalWriteResult, len(ws))
		for i := range ws {
			out[i] = wsrpc.ExternalWriteResult{
				Refusal: &wsrpc.ExternalWriteRefusal{SubType: subType, Message: msg},
			}
		}
		return out
	}
}

const postRecord = `{"$type":"app.bsky.feed.post","text":"hello from an external app","createdAt":"2026-07-24T10:00:00.000Z"}`

// listRecord is the journaled-collection fixture — a record with no Fauna
// concept, which D2 sends to the native-records journal.
//
// It carries `purpose` and `createdAt` because the lexicon requires them.
// Until Lexicon validation landed, these fixtures were a bare `{"name":…}`
// that no real PDS would have accepted; the catalog refused them the moment it
// was wired in, which is the first thing it did and a fair advertisement for
// what it is for. Keep this record schema-valid: a test that needs an INVALID
// one should say so at its own call site.
const listRecord = `{"$type":"app.bsky.graph.list","name":"mates",` +
	`"purpose":"app.bsky.graph.defs#curatelist","createdAt":"2026-07-24T10:00:00.000Z"}`

func createRecordBody(collection, record string) []byte {
	return []byte(`{"repo":"` + testDIDForAlice + `","collection":"` + collection + `","record":` + record + `}`)
}

// testDIDForAlice is the DID the fake nest reports for the fixture account —
// the account's real did:plc, carried through the session token (slice 4d).
// Before 4d this was `didForActor(<the 0x42 actor>)`, a placeholder the bridge
// re-derived; nothing re-derives a DID now.
const testDIDForAlice = testLoginDID

type createReply struct {
	URI              string `json:"uri"`
	CID              string `json:"cid"`
	ValidationStatus string `json:"validationStatus"`
	Commit           *struct {
		CID string `json:"cid"`
		Rev string `json:"rev"`
	} `json:"commit"`
}

// The F2 headline at handler level: a session-authed createRecord of a post
// reaches the nest, and the repo commit lands at the rkey the NEST chose,
// carrying the Fauna post id.
func TestCreateRecordCommitsAtTheNestChosenRkey(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3nestchose", "ab12")

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.feed.post", postRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("createRecord: %d", resp.StatusCode)
	}
	var got createReply
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatal(err)
	}
	if got.URI != "at://did:fauna:x/app.bsky.feed.post/3nestchose" {
		t.Fatalf("uri = %q", got.URI)
	}
	if got.CID == "" {
		t.Fatal("reply carries no record cid")
	}
	// "valid": app.bsky.feed.post is in the vendored catalog and this record
	// satisfies it. The claim is earned — see TestCreateRecordJournalsAn-
	// UnknownLexiconUnvalidated for the collection that still answers
	// "unknown", which is the honest answer where no schema was checked.
	if got.ValidationStatus != "valid" {
		t.Fatalf("validationStatus = %q", got.ValidationStatus)
	}
	if got.Commit == nil || got.Commit.Rev != "3rev" {
		t.Fatalf("commit ref = %+v", got.Commit)
	}

	// The bridge sent a candidate rkey and the record CID it computed.
	if f.nest.ingestCalls != 1 {
		t.Fatalf("ingest calls = %d", f.nest.ingestCalls)
	}
	sent := f.nest.lastIngest[0]
	if sent.Action != wsrpc.ExternalWriteActionCreate || sent.Collection != "app.bsky.feed.post" {
		t.Fatalf("sent %+v", sent)
	}
	if sent.Rkey == nil || *sent.Rkey == "" {
		t.Fatal("no candidate rkey was sent; a journaled write would have nowhere to land")
	}
	if sent.CID == nil || *sent.CID != got.CID {
		t.Fatalf("cid sent to nest %v != cid answered %q", sent.CID, got.CID)
	}
	if len(sent.Record) == 0 {
		t.Fatal("no record bytes were sent")
	}

	// THE load-bearing assertion. The commit must land at the nest's rkey (the
	// projection derives that same key from the Fauna post) and must carry the
	// Fauna post id, which is what writes post_map and stops the projection
	// loop re-projecting this post over the caller's own record bytes.
	ops := w.lastOps(t)
	if len(ops) != 1 {
		t.Fatalf("committed %d ops", len(ops))
	}
	if ops[0].Rkey != "3nestchose" {
		t.Fatalf("committed at rkey %q, not the nest's", ops[0].Rkey)
	}
	if ops[0].FaunaPostID != "ab12" {
		t.Fatalf("commit carries fauna post id %q — post_map would be missing the row", ops[0].FaunaPostID)
	}
	if ops[0].Action != atprotorepo.ActionCreate {
		t.Fatalf("action = %q", ops[0].Action)
	}
	// Into the caller's OWN repo, never whichever DID the payload named.
	if w.commitDIDs[0] != testDIDForAlice {
		t.Fatalf("committed into repo %q, not the caller's", w.commitDIDs[0])
	}
}

// A journaled record has no Fauna identity, so the commit carries no post id —
// post_map is keyed by exactly that identity and must not gain a bogus row.
func TestCreateRecordJournaledCarriesNoFaunaPostID(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		// The journal arm echoes the bridge's own rkey back.
		uri := "at://did:fauna:x/" + ws[0].Collection + "/" + *ws[0].Rkey
		return []wsrpc.ExternalWriteResult{{Rkey: ws[0].Rkey, AtURI: &uri}}
	}

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.graph.list", listRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("createRecord: %d", resp.StatusCode)
	}
	ops := w.lastOps(t)
	if ops[0].FaunaPostID != "" {
		t.Fatalf("journaled record got a fauna post id %q", ops[0].FaunaPostID)
	}
	if ops[0].Collection != "app.bsky.graph.list" {
		t.Fatalf("collection = %q", ops[0].Collection)
	}
}

// D6's sub-types must survive onto the XRPC wire: `deferred` is a "not yet"
// (MethodNotImplemented), never a permanent-policy 400 a later session could
// read as settled.
func TestRefusalSubTypesReachTheWire(t *testing.T) {
	for _, tc := range []struct {
		subType    string
		wantStatus int
		wantName   string
	}{
		{wsrpc.RefusalDeferred, http.StatusNotFound, "MethodNotImplemented"},
		{wsrpc.RefusalFaunaSurface, http.StatusBadRequest, "InvalidRequest"},
		{wsrpc.RefusalPolicy, http.StatusBadRequest, "InvalidRequest"},
	} {
		t.Run(tc.subType, func(t *testing.T) {
			f, w := newWriteFixture(t)
			_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
			f.nest.ingest = refusedWith(tc.subType, "nope: "+tc.subType)

			resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
				createRecordBody("app.bsky.feed.post", postRecord))
			if resp.StatusCode != tc.wantStatus {
				t.Fatalf("status = %d, want %d", resp.StatusCode, tc.wantStatus)
			}
			var e struct {
				Error   string `json:"error"`
				Message string `json:"message"`
			}
			if err := json.NewDecoder(resp.Body).Decode(&e); err != nil {
				t.Fatal(err)
			}
			if e.Error != tc.wantName {
				t.Fatalf("error name = %q, want %q", e.Error, tc.wantName)
			}
			// The nest's own message is what tells the user where to go
			// (e.g. "authorize it in a Fauna app") — it must not be
			// replaced by a generic one.
			if e.Message != "nope: "+tc.subType {
				t.Fatalf("message = %q", e.Message)
			}
			// A refused write must leave the repo untouched.
			w.mu.Lock()
			defer w.mu.Unlock()
			if len(w.ops) != 0 {
				t.Fatal("a refused write still committed to the repo")
			}
		})
	}
}

// A delete of a record that maps to a Fauna post carries that id, so the
// funnel drops the post_map row in the same transaction as the record.
func TestDeleteRecordCarriesTheMappedFaunaPostID(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	w.postIDs["app.bsky.feed.post/3gone"] = "cafe"
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		uri := "at://did:fauna:x/" + ws[0].Collection + "/" + *ws[0].Rkey
		return []wsrpc.ExternalWriteResult{{Rkey: ws[0].Rkey, AtURI: &uri}}
	}

	body := []byte(`{"repo":"` + testDIDForAlice + `","collection":"app.bsky.feed.post","rkey":"3gone"}`)
	resp := f.postBody(t, "com.atproto.repo.deleteRecord", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("deleteRecord: %d", resp.StatusCode)
	}
	if f.nest.lastIngest[0].Action != wsrpc.ExternalWriteActionDelete {
		t.Fatalf("action = %q", f.nest.lastIngest[0].Action)
	}
	ops := w.lastOps(t)
	if ops[0].Action != atprotorepo.ActionDelete || ops[0].Rkey != "3gone" {
		t.Fatalf("op = %+v", ops[0])
	}
	if ops[0].FaunaPostID != "cafe" {
		t.Fatalf("delete op carries fauna post id %q — post_map would keep a dangling row", ops[0].FaunaPostID)
	}
}

// A create carries the record's resolved references to the nest. Without them
// the nest cannot build a Fauna `Reference` — the AT-URI -> Fauna post id
// direction exists only in post_map — and would journal every reply.
func TestCreateRecordCarriesResolvedTargetsToTheNest(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	const parentURI = "at://did:plc:parent/app.bsky.feed.post/3parent"
	w.refs[parentURI] = "beef"
	f.nest.ingest = roundTripped("3nestrkey", "cafe")

	reply := `{"$type":"app.bsky.feed.post","text":"a reply","createdAt":"2026-07-24T10:00:00.000Z",` +
		`"reply":{"root":{"uri":"` + parentURI + `","cid":"bafyparent"},` +
		`"parent":{"uri":"` + parentURI + `","cid":"bafyparent"}}}`
	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.feed.post", reply))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("createRecord: %d", resp.StatusCode)
	}

	if len(w.resolveCalls) != 1 {
		t.Fatalf("the record must be put through reference resolution exactly once, got %d", len(w.resolveCalls))
	}
	got := f.nest.lastIngest[0].ResolvedTargets
	if got[parentURI] != "beef" {
		t.Fatalf("resolved targets reaching the nest = %v, want the parent mapped to its Fauna post", got)
	}
}

// A record whose references resolve to nothing sends no targets — and that is
// a normal write, not an error: the nest journals it, which is the ratified
// answer for a reply whose parent is not a Fauna post.
func TestCreateRecordWithUnresolvableRefsSendsNoTargets(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	_ = w // nothing planted in w.refs: the target is not a Fauna post
	f.nest.ingest = roundTripped("3nestrkey", "cafe")

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.feed.post", postRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("createRecord: %d", resp.StatusCode)
	}
	if len(f.nest.lastIngest[0].ResolvedTargets) != 0 {
		t.Fatalf("unresolvable refs must send no targets, got %v", f.nest.lastIngest[0].ResolvedTargets)
	}
}

// **Ordering is the load-bearing property here.** The post_map lookup used to
// run only after the nest answered, purely to feed the funnel. The nest needs
// it too — tombstoning the Fauna post a record maps to requires the id, and
// nothing resolves rkey -> Fauna post id nest-side — so the lookup must reach
// the ingest call. A regression that moved it back after the nest call would
// leave this map empty and every post delete would journal instead of deleting.
func TestDeleteRecordResolvesTheRecordBeforeTheNestCall(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	w.postIDs["app.bsky.feed.post/3gone"] = "cafe"
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		uri := "at://did:fauna:x/" + ws[0].Collection + "/" + *ws[0].Rkey
		return []wsrpc.ExternalWriteResult{{Rkey: ws[0].Rkey, AtURI: &uri}}
	}

	body := []byte(`{"repo":"` + testDIDForAlice + `","collection":"app.bsky.feed.post","rkey":"3gone"}`)
	if resp := f.postBody(t, "com.atproto.repo.deleteRecord", sess.AccessJwt, body); resp.StatusCode != http.StatusOK {
		t.Fatalf("deleteRecord: %d", resp.StatusCode)
	}

	want := atprotorepo.ATURI(testDIDForAlice, "app.bsky.feed.post", "3gone")
	got := f.nest.lastIngest[0].ResolvedTargets
	if got[want] != "cafe" {
		t.Fatalf("the nest must be told which Fauna post this record maps to; targets = %v, want %q -> cafe", got, want)
	}
}

// A delete of a record mapping to no Fauna post sends no targets, so the nest
// tombstones it in the journal rather than looking for a post to delete.
func TestDeleteRecordOfAnUnmappedRecordSendsNoTargets(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		uri := "at://did:fauna:x/" + ws[0].Collection + "/" + *ws[0].Rkey
		return []wsrpc.ExternalWriteResult{{Rkey: ws[0].Rkey, AtURI: &uri}}
	}

	body := []byte(`{"repo":"` + testDIDForAlice + `","collection":"app.bsky.graph.follow","rkey":"3journaled"}`)
	if resp := f.postBody(t, "com.atproto.repo.deleteRecord", sess.AccessJwt, body); resp.StatusCode != http.StatusOK {
		t.Fatalf("deleteRecord: %d", resp.StatusCode)
	}
	if len(f.nest.lastIngest[0].ResolvedTargets) != 0 {
		t.Fatalf("an unmapped record must send no targets, got %v", f.nest.lastIngest[0].ResolvedTargets)
	}
}

// A session authenticates ONE account. Naming another repo must be refused
// outright, never quietly redirected to whatever repo the token owns.
func TestWritesRefuseAnotherRepo(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3x", "aa")

	body := []byte(`{"repo":"did:plc:someoneelse","collection":"app.bsky.feed.post","record":` + postRecord + `}`)
	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("foreign repo accepted: %d", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a foreign-repo write reached the nest")
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	if len(w.ops) != 0 {
		t.Fatal("a foreign-repo write reached the repo")
	}
}

// A CAS that has already lost is refused BEFORE the nest is asked to apply
// anything — the load-bearing half of "checked bridge-side, nothing ingested"
// (atproto-pds-full.md § F2 detail, the CAS bullet). Refusing after the nest
// call would leave a real Fauna post behind for a write the caller was told
// failed.
func TestAStaleSwapCommitRefusesBeforeTheNestCall(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3x", "aa")
	w.headCID, w.hasHead = "bafycurrent", true

	body := `{"repo":"` + testDIDForAlice + `","collection":"app.bsky.feed.post",` +
		`"swapCommit":"bafystale","record":` + postRecord + `}`
	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt, []byte(body))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("stale swapCommit: %d, want 400 InvalidSwap", resp.StatusCode)
	}
	if name := errorName(t, resp); name != "InvalidSwap" {
		t.Fatalf("error name %q, want InvalidSwap — third-party clients match on it", name)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a doomed CAS reached the nest: a refused write must ingest nothing")
	}
	if len(w.ops) != 0 {
		t.Fatal("a doomed CAS reached the repo")
	}

	// Control: the SAME write naming the current head is served, so the
	// refusal above is the comparison and not the parameter's mere presence.
	ok := `{"repo":"` + testDIDForAlice + `","collection":"app.bsky.feed.post",` +
		`"swapCommit":"bafycurrent","record":` + postRecord + `}`
	if s := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt, []byte(ok)).StatusCode; s != http.StatusOK {
		t.Fatalf("matching swapCommit: %d, want 200", s)
	}
}

// The pre-check is the good error message; it is NOT the guarantee. It cannot
// be atomic with the commit that follows, so the expectations must also reach
// the funnel, which repeats the comparison under its per-DID lock against the
// very head the commit chains onto. Two-site enforcement, the first-emit
// gate's shape — and a test that only asserted the pre-check would let the
// race-free half be deleted silently.
func TestCASExpectationsReachTheFunnel(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("self", "")
	w.headCID, w.hasHead = "bafycurrent", true
	w.records["app.bsky.actor.profile/self"] = "bafyrecord"

	body := []byte(`{"repo":"` + testDIDForAlice + `","collection":"app.bsky.actor.profile",` +
		`"rkey":"self","swapCommit":"bafycurrent","swapRecord":"bafyrecord","record":` + profileRecord + `}`)
	if s := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt, body).StatusCode; s != http.StatusOK {
		t.Fatalf("putRecord with a matching CAS: %d", s)
	}

	w.mu.Lock()
	defer w.mu.Unlock()
	if len(w.casOpts) != 1 {
		t.Fatalf("commits: %d, want 1", len(w.casOpts))
	}
	// Replay the options the handler passed onto a fresh funnel option set:
	// the values themselves are the funnel's business, but a commit that
	// carried NO options would mean the guarantee never reached it. A third,
	// unconditional SkipUnchangedRecords rides every write-path commit (the
	// reconciler-race cosmetic fix, atproto-pds-full.md § F2 detail's no-CAS
	// residual) — it only ever short-circuits a byte-identical race, so it
	// carries alongside the CAS options rather than replacing them.
	if len(w.casOpts[0]) != 3 {
		t.Fatalf("the commit carried %d options, want 3 (swapCommit + swapRecord + SkipUnchangedRecords)",
			len(w.casOpts[0]))
	}
}

// The funnel is the only site that can see a CAS lost to a concurrent writer
// between the pre-check and the commit. When it does, the caller gets the
// lexicon's InvalidSwap — theirs to retry after re-reading — never an opaque
// internal error.
func TestAFunnelSideCASLossSurfacesAsInvalidSwap(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3x", "aa")
	w.headCID, w.hasHead = "bafycurrent", true
	// The pre-check passes; the funnel then refuses, exactly as a racing
	// sibling commit would make it.
	w.commitErr = fmt.Errorf("%w: swapCommit bafycurrent, but the repo head is bafyraced",
		atprotorepo.ErrSwapMismatch)

	body := `{"repo":"` + testDIDForAlice + `","collection":"app.bsky.feed.post",` +
		`"swapCommit":"bafycurrent","record":` + postRecord + `}`
	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt, []byte(body))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("funnel-side CAS loss: %d, want 400", resp.StatusCode)
	}
	if name := errorName(t, resp); name != "InvalidSwap" {
		t.Fatalf("error name %q, want InvalidSwap (not an internal error)", name)
	}
}

// `validate: true` is a caller DEMANDING schema validation, and it is now
// SERVED for any collection this PDS holds a record schema for.
func TestCreateRecordServesExplicitValidateRequest(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3x", "aa")

	body := `{"repo":"` + testDIDForAlice + `","collection":"app.bsky.feed.post","validate":true,"record":` + postRecord + `}`
	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt, []byte(body))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("validate:true refused: %d", resp.StatusCode)
	}
	if got := validationStatusOf(t, resp); got != "valid" {
		t.Fatalf("validationStatus = %q, want valid", got)
	}
}

// The other half of the same demand: a collection this PDS holds NO record
// schema for cannot satisfy `validate: true`, and answering 200 would be the
// silent downgrade the demand exists to prevent. It is InvalidRequest, not
// MethodNotImplemented — the method is served and works; it is this record's
// lexicon that is unknown here.
func TestCreateRecordRefusesValidateTrueForAnUnknownLexicon(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3x", "aa")

	body := `{"repo":"` + testDIDForAlice + `","collection":"com.example.custom.thing","validate":true,` +
		`"record":{"$type":"com.example.custom.thing","a":"b"}}`
	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt, []byte(body))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("validate:true on an unknown lexicon: %d, want 400", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a refused validate:true write reached the nest")
	}
}

// The same unknown lexicon WITHOUT the demand is journaled, not refused: D2's
// table rules that unknown/third-party NSIDs get no schema gate. This is the
// pin that stops a later session hardening the refusal above into a policy
// against third-party lexicons.
func TestCreateRecordJournalsAnUnknownLexiconUnvalidated(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		uri := "at://did:fauna:x/" + ws[0].Collection + "/" + *ws[0].Rkey
		return []wsrpc.ExternalWriteResult{{Rkey: ws[0].Rkey, AtURI: &uri}}
	}

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("com.example.custom.thing", `{"$type":"com.example.custom.thing","a":"b"}`))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("unknown-lexicon write refused: %d", resp.StatusCode)
	}
	if got := validationStatusOf(t, resp); got != "unknown" {
		t.Fatalf("validationStatus = %q, want unknown", got)
	}
}

// A record whose lexicon IS known and which does not satisfy it is refused
// before the nest is called — the pre-flight ordering every other write-path
// refusal follows.
func TestCreateRecordRefusesARecordThatFailsItsKnownSchema(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3x", "aa")

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.feed.post", `{"$type":"app.bsky.feed.post","text":"no createdAt"}`))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("invalid post accepted: %d", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a schema-invalid write reached the nest")
	}
}

// `validate: false` is the caller's explicit opt-out, and it must still work
// for a record that would otherwise be refused — otherwise the escape hatch
// the behavior change relies on does not exist.
func TestCreateRecordHonoursValidateFalse(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3x", "aa")

	body := `{"repo":"` + testDIDForAlice + `","collection":"app.bsky.feed.post","validate":false,` +
		`"record":{"$type":"app.bsky.feed.post","text":"no createdAt"}}`
	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt, []byte(body))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("validate:false refused: %d", resp.StatusCode)
	}
	if got := validationStatusOf(t, resp); got != "unknown" {
		t.Fatalf("validationStatus = %q, want unknown for an unvalidated write", got)
	}
}

// A record filed under one collection while declaring another would serve
// correctly here and be read as the DECLARED type everywhere downstream.
func TestCreateRecordRefusesTypeCollectionMismatch(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3x", "aa")

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.graph.list", postRecord))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("$type/collection mismatch accepted: %d", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a mismatched write reached the nest")
	}
}

func TestWritesRequireAuth(t *testing.T) {
	f, _ := newWriteFixture(t)
	if s := f.postBody(t, "com.atproto.repo.createRecord", "",
		createRecordBody("app.bsky.feed.post", postRecord)).StatusCode; s != http.StatusUnauthorized {
		t.Fatalf("unauthenticated createRecord: %d", s)
	}
	if s := f.postBody(t, "com.atproto.repo.deleteRecord", "",
		[]byte(`{"repo":"x","collection":"c","rkey":"r"}`)).StatusCode; s != http.StatusUnauthorized {
		t.Fatalf("unauthenticated deleteRecord: %d", s)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("an unauthenticated write reached the nest")
	}
}

// The routes register even without a writer wired (one route table, always the
// same shape) — but they must refuse rather than pretend, and must not spend a
// nest round-trip first.
func TestWritesRefuseWithNoWriterWired(t *testing.T) {
	f := newFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.feed.post", postRecord))
	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("write with no writer: %d", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a write reached the nest with no writer to commit it")
	}
}

// ── slice 4d: the two binaries must name the SAME repo ──────────

// THE 4d pin. Every assertion here is RELATIVE — it compares what the nest said
// against what the bridge did — because that is the only way this bug is
// visible. Before 4d both sides were internally consistent and green while
// disagreeing with each other: the nest answered `at://did:plc:…/…` while the
// bridge committed into `did:fauna:<hex>`, so the account quietly maintained
// two repos. A test that pins each side to its own fixture literal (as the
// tests above once did, with `at://did:fauna:x/…`) cannot see that, and did
// not for two slices.
//
// The invariant, stated once: **the repo the bridge commits into is the repo
// the AT-URI the nest answered names.** Everything downstream — AppView
// resolution, post_map's DID key, the projection loop's idempotency — is a
// consequence of it.
func TestTheCommittedRepoIsTheRepoTheNestAnswered(t *testing.T) {
	f, w := newWriteFixture(t)
	// A real did:plc, deliberately unlike anything derivable from the actor id:
	// if the bridge can only produce this string by carrying the nest's, the
	// re-derivation path is provably gone.
	f.nest.loginDID = "did:plc:7iza6de2dwap2sbkpav7c6c6"
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	// The session the app was handed names that DID — not a placeholder.
	if sess.Did != f.nest.loginDID {
		t.Fatalf("createSession answered did %q, but the nest's identity is %q", sess.Did, f.nest.loginDID)
	}

	// The nest answers the AT-URI under its own DID, exactly as the real one
	// does (it resolves the DID from the actor id and never takes one from the
	// bridge).
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		out := make([]wsrpc.ExternalWriteResult, len(ws))
		for i, wr := range ws {
			k := "3nestchose"
			uri := "at://" + f.nest.loginDID + "/" + wr.Collection + "/" + k
			id := "ab12"
			out[i] = wsrpc.ExternalWriteResult{Rkey: &k, AtURI: &uri, FaunaPostID: &id}
		}
		return out
	}

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.feed.post", postRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("createRecord: %d", resp.StatusCode)
	}
	var got createReply
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatal(err)
	}

	// The load-bearing comparison: authority(answered uri) == committed repo.
	wantRepo := aturiAuthority(t, got.URI)
	if len(w.commitDIDs) != 1 {
		t.Fatalf("commit count = %d", len(w.commitDIDs))
	}
	if w.commitDIDs[0] != wantRepo {
		t.Fatalf("the bridge committed into repo %q but the nest answered a uri in repo %q — "+
			"the account is maintaining two repos", w.commitDIDs[0], wantRepo)
	}
	// And no placeholder survives anywhere on the path.
	if strings.HasPrefix(w.commitDIDs[0], "did:fauna:") {
		t.Fatalf("committed into a placeholder repo %q", w.commitDIDs[0])
	}
}

// The delete arm keys its post_map lookup by the caller's DID too, so it has
// the same failure mode — and it is worse there, because an unresolved lookup
// silently degrades a post delete into a journal tombstone rather than erroring.
func TestDeleteRecordResolvesPostMapUnderTheNestsRepo(t *testing.T) {
	f, w := newWriteFixture(t)
	f.nest.loginDID = "did:plc:7iza6de2dwap2sbkpav7c6c6"
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	w.postIDs["app.bsky.feed.post/3nestchose"] = "ab12"
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		return []wsrpc.ExternalWriteResult{{Rkey: ws[0].Rkey}}
	}

	resp := f.postBody(t, "com.atproto.repo.deleteRecord", sess.AccessJwt,
		[]byte(`{"repo":"`+f.nest.loginDID+`","collection":"app.bsky.feed.post","rkey":"3nestchose"}`))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("deleteRecord: %d", resp.StatusCode)
	}
	// resolved_targets carries `at://<did>/…`, and that DID must be the real
	// one: the nest looks the post up by that URI.
	sent := f.nest.lastIngest[0]
	if len(sent.ResolvedTargets) != 1 {
		t.Fatalf("resolved targets = %v", sent.ResolvedTargets)
	}
	for uri := range sent.ResolvedTargets {
		if got := aturiAuthority(t, uri); got != f.nest.loginDID {
			t.Fatalf("delete resolved target %q is keyed by %q, not the account's real did %q",
				uri, got, f.nest.loginDID)
		}
	}
	if w.commitDIDs[0] != f.nest.loginDID {
		t.Fatalf("delete committed into %q, not %q", w.commitDIDs[0], f.nest.loginDID)
	}
}

// aturiAuthority pulls the DID out of an `at://<authority>/<collection>/<rkey>`.
func aturiAuthority(t *testing.T, uri string) string {
	t.Helper()
	rest, ok := strings.CutPrefix(uri, "at://")
	if !ok {
		t.Fatalf("not an at-uri: %q", uri)
	}
	authority, _, _ := strings.Cut(rest, "/")
	if authority == "" {
		t.Fatalf("at-uri has no authority: %q", uri)
	}
	return authority
}

// Slice 4e. An account whose identity has never been announced — and does not
// yet resolve ecosystem-side — must not get its FIRST firehose event from an
// external write. The projection loop has always enforced this for its own
// commits; the write path went straight through the funnel, so an app writing
// during the window could put the account on the network unresolvable, which is
// the first-impression trap of atproto-pds-full.md § Ecosystem reality.
//
// The refusal is asserted BEFORE the nest ingest, not merely as a status code:
// refusing after it would leave a real Fauna post that all 7 apps show while the
// network never sees it. Each arm carries an open-gate control on the same
// route, so a refusal that fired for any other reason fails the control too.
func TestCreateRecordRefusesWhileTheIdentityIsFirstEmitGated(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3nestchose", "ab12")
	w.gated = true

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.feed.post", postRecord))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("a gated account's write must be refused, got %d", resp.StatusCode)
	}
	// Nothing happened anywhere: no Fauna post, no repo commit.
	if f.nest.ingestCalls != 0 {
		t.Fatalf("the nest was asked to ingest %d writes while gated — a refusal "+
			"after the ingest leaves a Fauna post the network will never see",
			f.nest.ingestCalls)
	}
	if len(w.ops) != 0 {
		t.Fatalf("committed %d batches into a gated repo", len(w.ops))
	}

	// Control: the SAME request on the SAME route succeeds once the gate opens,
	// so the refusal above is provably the gate and not the fixture.
	w.gated = false
	resp = f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.feed.post", postRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("ungated control write failed: %d", resp.StatusCode)
	}
	if f.nest.ingestCalls != 1 {
		t.Fatalf("ungated control did not reach the nest: %d calls", f.nest.ingestCalls)
	}
}

func TestDeleteRecordRefusesWhileTheIdentityIsFirstEmitGated(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	w.postIDs["app.bsky.feed.post/3nestchose"] = "ab12"
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		return []wsrpc.ExternalWriteResult{{Rkey: ws[0].Rkey}}
	}
	body := []byte(`{"repo":"` + f.nest.loginDID + `","collection":"app.bsky.feed.post","rkey":"3nestchose"}`)
	w.gated = true

	resp := f.postBody(t, "com.atproto.repo.deleteRecord", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("a gated account's delete must be refused, got %d", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatalf("the nest was asked to tombstone %d writes while gated — the "+
			"Fauna post would be gone with the repo never having existed",
			f.nest.ingestCalls)
	}

	w.gated = false
	resp = f.postBody(t, "com.atproto.repo.deleteRecord", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("ungated control delete failed: %d", resp.StatusCode)
	}
	if f.nest.ingestCalls != 1 {
		t.Fatalf("ungated control did not reach the nest: %d calls", f.nest.ingestCalls)
	}
}

// ── putRecord (F2.3) ───────────────────────────────────────────────────────

const profileRecord = `{"$type":"app.bsky.actor.profile","displayName":"Alice","description":"bio from bsky.app"}`

func putRecordBody(collection, rkey, record string) []byte {
	return []byte(`{"repo":"` + testDIDForAlice + `","collection":"` + collection +
		`","rkey":"` + rkey + `","record":` + record + `}`)
}

// The classifier is a function of (collection, ACTION), so the verb is the only
// thing that tells the nest an update was asked for. A putRecord that declared
// `create` would be classified as a profile CREATE and — worse — a post
// putRecord would round-trip as a new post instead of being refused immutable.
func TestPutRecordDeclaresTheUpdateAction(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("self", "")

	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt,
		putRecordBody("app.bsky.actor.profile", "self", profileRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("putRecord: %d", resp.StatusCode)
	}
	sent := f.nest.lastIngest[0]
	if sent.Action != wsrpc.ExternalWriteActionUpdate {
		t.Fatalf("putRecord declared action %q, want %q",
			sent.Action, wsrpc.ExternalWriteActionUpdate)
	}
	if sent.Collection != "app.bsky.actor.profile" {
		t.Fatalf("collection = %q", sent.Collection)
	}
}

// The inbound-picture resolution's bridge half (atproto-pds-full.md § F2
// detail): a profile write's blob refs are resolved against OUR blob store and
// sent as `resolved_media`, so the nest can tell an echoed avatar — bytes it
// published but cannot name — from a ref nobody serves.
//
// It must run BEFORE the nest call, like every other refusal input: the nest
// refuses a picture that resolves to nothing, and a refusal that arrives after
// the ingest is a profile changed for a record the network never gets.
func TestAProfileWriteSendsTheEchoedPicturesResolvedMedia(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("self", "")
	const echoedBlob = "bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4"
	const faunaCID = "bafyfaunaavatarcontenthash"
	w.publishedBlobs = map[string]string{echoedBlob: faunaCID}

	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt,
		putRecordBody("app.bsky.actor.profile", "self", profileRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("putRecord: %d", resp.StatusCode)
	}

	sent := f.nest.lastIngest[0]
	if got := sent.ResolvedMedia[echoedBlob]; got != faunaCID {
		t.Fatalf("resolved_media[%s] = %q, want %q — the nest cannot resolve an "+
			"echoed picture itself; the blob store is the only Fauna-CID index",
			echoedBlob, got, faunaCID)
	}
	if len(w.blobResolveCalls) != 1 {
		t.Fatalf("ResolveBlobRefs calls = %d, want 1", len(w.blobResolveCalls))
	}
	if got := w.blobResolveCalls[0].DID; got != testDIDForAlice {
		t.Fatalf("resolved against did %q, want %q — blob refs are repo-scoped",
			got, testDIDForAlice)
	}
}

// The scope decision, pinned as behaviour rather than left to a code comment: a
// POST write sends no `resolved_media`. F2.4 slice 2 ratified that an external
// app uploads its post images, and the nest refuses a post ref with no
// atproto_blobs row.
//
// This is the PRODUCING side of a two-sided rule, and defence in depth rather
// than the rule itself: the nest independently refuses to consult a vouch on a
// non-profile row (`resolves_echoed_media`), so a future bridge that started
// sending them would be declined rather than believed.
func TestAPostWriteSendsNoResolvedMedia(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3kexamplepostrkey", "abc123")
	w.publishedBlobs = map[string]string{
		"bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4": "bafyfauna",
	}

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		[]byte(`{"repo":"`+testDIDForAlice+`","collection":"app.bsky.feed.post",`+
			`"record":{"$type":"app.bsky.feed.post","text":"hi",`+
			`"createdAt":"2026-07-30T10:00:00.000Z"}}`))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("createRecord: %d", resp.StatusCode)
	}

	if got := f.nest.lastIngest[0].ResolvedMedia; len(got) != 0 {
		t.Fatalf("a post write sent resolved_media %v — post images have no echo "+
			"case; they must be uploaded (F2.4 slice 2)", got)
	}
	if len(w.blobResolveCalls) != 0 {
		t.Fatalf("a post write asked the blob store %d times, want 0",
			len(w.blobResolveCalls))
	}
}

// A blob-store read failure must fail the WRITE, never proceed with a partial
// map: an unresolved echo reaching the nest reads as "no such picture" and
// refuses the batch — turning a transient DB error into a user-visible refusal
// whose message blames the caller for a ref it correctly supplied.
func TestAProfileWriteFailsWhenTheBlobStoreCannotAnswer(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("self", "")
	w.blobResolveErr = errors.New("blob store unavailable")

	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt,
		putRecordBody("app.bsky.actor.profile", "self", profileRecord))
	if resp.StatusCode != http.StatusInternalServerError {
		t.Fatalf("putRecord with an unreadable blob store: %d, want 500", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatalf("the nest was called %d times despite an unresolved picture map",
			f.nest.ingestCalls)
	}
}

// THE load-bearing putRecord assertion. For a collection the projection owns,
// the nest says so (`reproject_record`) and the bridge must commit the record a
// PROJECTION PASS would produce — not the caller's bytes. Committing the
// caller's bytes would have the next projection pass overwrite them, so the cid
// answered here would name bytes that do not survive.
func TestPutRecordCommitsTheReprojectedRecordAndAnswersItsCID(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")

	// What a projection pass renders: the projection's own rendering of the
	// merged Fauna profile. It differs from the caller's record in CONTENT, not
	// merely key order — dag-cbor canonicalizes key order, so a reordered
	// fixture would encode byte-identically and the assertion below would pass
	// vacuously. Here the projection truncated the display name to its
	// 64-grapheme cap and carries an avatar blob ref the caller's record could
	// not name (its ATProto CID is bridge-side state) — both real differences.
	const projected = `{"$type":"app.bsky.actor.profile","displayName":"Alice (truncated by the projection)","description":"bio from bsky.app","avatar":{"$type":"blob","ref":{"$link":"bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4"},"mimeType":"image/png","size":24}}`
	w.renderedRecord = projected
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		out := make([]wsrpc.ExternalWriteResult, len(ws))
		for i := range ws {
			rkey := "self"
			uri := "at://" + testDIDForAlice + "/app.bsky.actor.profile/self"
			out[i] = wsrpc.ExternalWriteResult{Rkey: &rkey, AtURI: &uri, ReprojectRecord: true}
		}
		return out
	}

	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt,
		putRecordBody("app.bsky.actor.profile", "self", profileRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("putRecord: %d", resp.StatusCode)
	}
	var got createReply
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatal(err)
	}

	// The bytes the caller sent are what reached the NEST (it needs the input),
	// but they are NOT what reached the repo.
	sentCBOR := f.nest.lastIngest[0].Record
	ops := w.lastOps(t)
	if len(ops) != 1 {
		t.Fatalf("committed %d ops", len(ops))
	}
	if ops[0].Action != atprotorepo.ActionUpdate {
		t.Fatalf("committed op action = %q, want update", ops[0].Action)
	}
	if ops[0].Rkey != "self" {
		t.Fatalf("committed at rkey %q, want the singleton key self", ops[0].Rkey)
	}
	wantCBOR, err := atprotorepo.JSONRecordToDagCBOR(projected)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(ops[0].RecordCBOR, wantCBOR) {
		t.Fatal("the repo carries bytes that are not the projection's own rendering")
	}
	if bytes.Equal(ops[0].RecordCBOR, sentCBOR) {
		t.Fatal("the repo carries the CALLER's bytes; the next projection pass would overwrite them")
	}

	// And it asked the PROJECTION for them, for this account's own profile —
	// the ask is what makes the rendering one function rather than two.
	if len(w.renderCalls) != 1 {
		t.Fatalf("RenderProjectedRecord asked %d times, want exactly 1", len(w.renderCalls))
	}
	if got, want := w.renderCalls[0].collection, "app.bsky.actor.profile"; got != want {
		t.Errorf("rendered collection %q, want %q", got, want)
	}
	if got := w.renderCalls[0].did; got != testDIDForAlice {
		t.Errorf("rendered for did %q, want %q", got, testDIDForAlice)
	}

	// And the answered cid must name what actually landed — read-your-writes.
	wantCID, err := atprotorepo.RecordCID(wantCBOR)
	if err != nil {
		t.Fatal(err)
	}
	if got.CID != wantCID {
		t.Fatalf("answered cid %q does not name the committed record (%q)", got.CID, wantCID)
	}
}

// A render failure must fail the WRITE, never fall back to committing the
// caller's bytes. The nest asserted the projection owns this record; quietly
// committing the caller's draft would restore the exact drift the assertion
// prevents, and would answer a cid the next pass overwrites — invisibly, since
// nothing in the reply would say the record was not the projected one.
func TestPutRecordRefusesRatherThanCommitTheCallersBytesWhenRenderFails(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	w.renderErr = errors.New("blob store unavailable")
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		out := make([]wsrpc.ExternalWriteResult, len(ws))
		for i := range ws {
			rkey := "self"
			uri := "at://" + testDIDForAlice + "/app.bsky.actor.profile/self"
			out[i] = wsrpc.ExternalWriteResult{Rkey: &rkey, AtURI: &uri, ReprojectRecord: true}
		}
		return out
	}

	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt,
		putRecordBody("app.bsky.actor.profile", "self", profileRecord))
	if resp.StatusCode == http.StatusOK {
		t.Fatalf("putRecord answered 200 despite an unrenderable projected record")
	}
	if got := w.commitCount(); got != 0 {
		t.Errorf("committed %d times; a failed render must commit nothing", got)
	}
}

// The `#commit` frame's `blobs` field is filled from the COMMITTED bytes —
// override included — through the shared-Rust walk behind the seam (F2.4
// slice 2). Walking the caller's draft instead would announce the blobs of a
// record that never lands and miss the committed record's own; committing
// with no walk at all would announce an empty set for a media record, the
// "small lie about the commit" the funnel's own docs call out.
func TestARecordWritesBlobRefsAreWalkedFromTheCommittedBytes(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	w.blobRefs = []string{"bafkreiblobone", "bafkreiblobtwo"}

	const projected = `{"$type":"app.bsky.actor.profile","displayName":"Alice (projected)"}`
	w.renderedRecord = projected
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		out := make([]wsrpc.ExternalWriteResult, len(ws))
		for i := range ws {
			rkey := "self"
			uri := "at://" + testDIDForAlice + "/app.bsky.actor.profile/self"
			out[i] = wsrpc.ExternalWriteResult{Rkey: &rkey, AtURI: &uri, ReprojectRecord: true}
		}
		return out
	}

	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt,
		putRecordBody("app.bsky.actor.profile", "self", profileRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("putRecord: %d", resp.StatusCode)
	}

	ops := w.lastOps(t)
	if len(ops) != 1 {
		t.Fatalf("committed %d ops", len(ops))
	}
	if len(ops[0].BlobCIDs) != 2 || ops[0].BlobCIDs[0] != "bafkreiblobone" ||
		ops[0].BlobCIDs[1] != "bafkreiblobtwo" {
		t.Fatalf("committed op announces %v, want the walked refs", ops[0].BlobCIDs)
	}

	wantCBOR, err := atprotorepo.JSONRecordToDagCBOR(projected)
	if err != nil {
		t.Fatal(err)
	}
	w.mu.Lock()
	walked := w.blobWalkCalls[len(w.blobWalkCalls)-1]
	w.mu.Unlock()
	if !bytes.Equal(walked, wantCBOR) {
		t.Fatal("the blob walk ran on bytes that are not the committed record " +
			"(the override) — the frame would announce the caller's draft")
	}
}

// The applyWrites twin: each non-delete member's committed op announces the
// walked refs — the batch verb builds ops on its own path, so the fill there
// is a separate site that must not drift.
func TestApplyWritesFillsBlobRefsPerCommittedMember(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	w.blobRefs = []string{"bafkreiblobone"}
	f.nest.ingest = roundTripped("3nestchose", "ab12")

	body := applyWritesBody("",
		`{"$type":"com.atproto.repo.applyWrites#create","collection":"app.bsky.graph.list","value":`+listRecord+`}`,
		`{"$type":"com.atproto.repo.applyWrites#delete","collection":"app.bsky.feed.post","rkey":"3gone"}`,
	)
	resp := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("applyWrites: %d", resp.StatusCode)
	}

	ops := w.lastOps(t)
	if len(ops) != 2 {
		t.Fatalf("committed %d ops", len(ops))
	}
	if len(ops[0].BlobCIDs) != 1 || ops[0].BlobCIDs[0] != "bafkreiblobone" {
		t.Fatalf("create member announces %v, want the walked refs", ops[0].BlobCIDs)
	}
	if len(ops[1].BlobCIDs) != 0 {
		t.Fatalf("a delete carries no record and must announce no blobs, got %v",
			ops[1].BlobCIDs)
	}
}

// The control arm for the test above: with NO override the caller's own bytes
// are what lands, so the override path is a genuine branch rather than the only
// behavior.
func TestARecordWriteWithoutAnOverrideCommitsTheCallersOwnBytes(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("3nestchose", "ab12")

	resp := f.postBody(t, "com.atproto.repo.createRecord", sess.AccessJwt,
		createRecordBody("app.bsky.feed.post", postRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("createRecord: %d", resp.StatusCode)
	}
	if !bytes.Equal(w.lastOps(t)[0].RecordCBOR, f.nest.lastIngest[0].Record) {
		t.Fatal("without an override the repo must carry exactly the caller's bytes")
	}
}

// An explicit `"swapRecord": null` is itself a CAS assertion — "this record
// must not already exist" — and the lexicon makes the field nullable precisely
// to express it. Decoding into a plain string would flatten it into "no CAS
// requested" and serve a write the caller asked us to guard.
//
// The three states are pinned together on purpose: absent, null, and a CID are
// three different questions, and a decode that collapses any two of them
// passes the other two tests while silently dropping a real assertion.
func TestPutRecordHonoursTheNullableSwapRecord(t *testing.T) {
	profileBody := func(swap string) []byte {
		return []byte(`{"repo":"` + testDIDForAlice + `","collection":"app.bsky.actor.profile",` +
			`"rkey":"self"` + swap + `,"record":` + profileRecord + `}`)
	}

	// (1) An explicit null against a repo that ALREADY has the record: the
	//     assertion is false, so the write is refused and nothing is ingested.
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("self", "")
	w.records["app.bsky.actor.profile/self"] = "bafyexisting"

	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt, profileBody(`,"swapRecord":null`))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("null swapRecord over an existing record: %d, want 400 InvalidSwap", resp.StatusCode)
	}
	if name := errorName(t, resp); name != "InvalidSwap" {
		t.Fatalf("error name %q, want InvalidSwap", name)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a failed CAS must refuse BEFORE the nest is asked to apply anything")
	}

	// (2) The same explicit null against a repo where the record is ABSENT:
	//     the assertion holds, so the write is served. This is the arm a
	//     string decode would also pass — it is only meaningful beside (1).
	f2, _ := newWriteFixture(t)
	_, sess2 := f2.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f2.nest.ingest = roundTripped("self", "")
	if s := f2.postBody(t, "com.atproto.repo.putRecord", sess2.AccessJwt,
		profileBody(`,"swapRecord":null`)).StatusCode; s != http.StatusOK {
		t.Fatalf("null swapRecord with no such record: %d, want 200", s)
	}

	// (3) Control — the field ABSENT is not a CAS at all, so the same write
	//     over an existing record is served. Without this arm, a decode that
	//     treated "absent" as "must not exist" would still pass (1) and (2).
	f3, w3 := newWriteFixture(t)
	_, sess3 := f3.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f3.nest.ingest = roundTripped("self", "")
	w3.records["app.bsky.actor.profile/self"] = "bafyexisting"
	if s := f3.postBody(t, "com.atproto.repo.putRecord", sess3.AccessJwt,
		putRecordBody("app.bsky.actor.profile", "self", profileRecord)).StatusCode; s != http.StatusOK {
		t.Fatalf("putRecord with no swapRecord at all: %d, want 200", s)
	}
}

// A swapRecord naming a CID that is not what the record currently holds is the
// ordinary lost-CAS case.
func TestAStaleSwapRecordRefuses(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("self", "")
	w.records["app.bsky.actor.profile/self"] = "bafycurrent"

	body := []byte(`{"repo":"` + testDIDForAlice + `","collection":"app.bsky.actor.profile",` +
		`"rkey":"self","swapRecord":"bafystale","record":` + profileRecord + `}`)
	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("stale swapRecord: %d, want 400", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a failed CAS reached the nest")
	}

	// deleteRecord takes the same parameter and the same comparison — its
	// swapRecord is non-nullable (there is no "must not exist" assertion to
	// make about a record you are deleting), but a stale one refuses alike.
	del := []byte(`{"repo":"` + testDIDForAlice + `","collection":"app.bsky.actor.profile",` +
		`"rkey":"self","swapRecord":"bafystale"}`)
	if s := f.postBody(t, "com.atproto.repo.deleteRecord", sess.AccessJwt, del).StatusCode; s != http.StatusBadRequest {
		t.Fatalf("stale swapRecord on deleteRecord: %d, want 400", s)
	}
}

func TestPutRecordRequiresAnRkey(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("self", "")

	body := []byte(`{"repo":"` + testDIDForAlice +
		`","collection":"app.bsky.actor.profile","record":` + profileRecord + `}`)
	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("rkey-less putRecord: %d, want 400", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a putRecord with no rkey must not reach the nest")
	}
}

// putRecord runs the same first-emit gate as createRecord. The gate is a
// property of "this PDS serves no repo write for an unannounced account", one
// rule for every verb — a verb-shaped hole in it would put an account's first
// #commit on the network unresolvable.
func TestPutRecordRefusesWhileTheIdentityIsFirstEmitGated(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = roundTripped("self", "")
	w.gated = true

	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt,
		putRecordBody("app.bsky.actor.profile", "self", profileRecord))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("gated putRecord: %d, want 400", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("the gate must refuse BEFORE the nest ingest, or a Fauna-side profile edit survives a network that never sees it")
	}

	// Open-gate control.
	w.gated = false
	resp = f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt,
		putRecordBody("app.bsky.actor.profile", "self", profileRecord))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("ungated control putRecord: %d", resp.StatusCode)
	}
}

// A refusal the nest decides — a post putRecord is policy-refused as immutable
// — must survive onto the wire with its sub-type in the error NAME, so no
// client reads "posts are immutable" as a "not yet".
func TestPutRecordSurfacesANestPolicyRefusal(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = refusedWith("policy", "posts are immutable; delete the post and create a new one")

	resp := f.postBody(t, "com.atproto.repo.putRecord", sess.AccessJwt,
		putRecordBody("app.bsky.feed.post", "3somepost", postRecord))
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("post putRecord: %d, want 400 InvalidRequest", resp.StatusCode)
	}
	if len(w.ops) != 0 {
		t.Fatal("a refused write must commit nothing to the repo")
	}
}

// ── applyWrites ──────────────────────────────────────────────────

// perWriteRkeys answers each write in a batch with its own derived rkey, so a
// test can tell the rows apart in the commit the bridge builds.
func perWriteRkeys(prefix string) func([]wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
	return func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		out := make([]wsrpc.ExternalWriteResult, len(ws))
		for i, w := range ws {
			k := fmt.Sprintf("%s%d", prefix, i)
			uri := "at://did:fauna:x/" + w.Collection + "/" + k
			id := fmt.Sprintf("post%d", i)
			out[i] = wsrpc.ExternalWriteResult{Rkey: &k, AtURI: &uri, FaunaPostID: &id}
		}
		return out
	}
}

func applyWritesBody(swapCommit string, writes ...string) []byte {
	swap := ""
	if swapCommit != "" {
		swap = `,"swapCommit":"` + swapCommit + `"`
	}
	return []byte(`{"repo":"` + testDIDForAlice + `"` + swap +
		`,"writes":[` + strings.Join(writes, ",") + `]}`)
}

// applyWrites is ONE nest round-trip and ONE funnel commit — the property the
// verb exists to provide (atproto-pds-full.md § F2 detail, "`applyWrites` =
// one funnel commit"). A per-write loop would still answer 200 while emitting
// N commits and N firehose frames for what the caller asked to be one.
func TestApplyWritesIsOneNestCallAndOneCommit(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = perWriteRkeys("3batch")

	body := applyWritesBody("",
		`{"$type":"com.atproto.repo.applyWrites#create","collection":"app.bsky.feed.post","value":`+postRecord+`}`,
		`{"$type":"com.atproto.repo.applyWrites#create","collection":"app.bsky.graph.list","value":`+listRecord+`}`,
		`{"$type":"com.atproto.repo.applyWrites#delete","collection":"app.bsky.feed.post","rkey":"3gone"}`,
	)
	resp := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("applyWrites: %d", resp.StatusCode)
	}
	var reply struct {
		Commit  *struct{ CID, Rev string } `json:"commit"`
		Results []struct {
			Type             string `json:"$type"`
			URI              string `json:"uri"`
			CID              string `json:"cid"`
			ValidationStatus string `json:"validationStatus"`
		} `json:"results"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&reply); err != nil {
		t.Fatalf("decode: %v", err)
	}

	if f.nest.ingestCalls != 1 {
		t.Fatalf("nest calls = %d, want exactly 1 for the whole batch", f.nest.ingestCalls)
	}
	if len(f.nest.lastIngest) != 3 {
		t.Fatalf("the one nest call carried %d writes, want 3", len(f.nest.lastIngest))
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	if len(w.ops) != 1 {
		t.Fatalf("commits = %d, want exactly 1 for the whole batch", len(w.ops))
	}
	if len(w.ops[0]) != 3 {
		t.Fatalf("the one commit carried %d ops, want 3", len(w.ops[0]))
	}

	// Results are positional, typed per member, and a #deleteResult carries no
	// uri/cid — the lexicon defines it as an empty object.
	if len(reply.Results) != 3 {
		t.Fatalf("results = %d, want 3", len(reply.Results))
	}
	for i, want := range []string{
		"com.atproto.repo.applyWrites#createResult",
		"com.atproto.repo.applyWrites#createResult",
		"com.atproto.repo.applyWrites#deleteResult",
	} {
		if reply.Results[i].Type != want {
			t.Fatalf("result %d $type = %q, want %q", i, reply.Results[i].Type, want)
		}
	}
	if reply.Results[0].URI == "" || reply.Results[0].CID == "" {
		t.Fatal("a #createResult must carry the uri and cid the write landed at")
	}
	if reply.Results[2].URI != "" || reply.Results[2].CID != "" {
		t.Fatalf("a #deleteResult must be empty, got %+v", reply.Results[2])
	}
	if reply.Commit == nil || reply.Commit.Rev == "" {
		t.Fatal("the batch must answer the commit it produced")
	}
}

// A refusal anywhere in the batch fails the WHOLE call and names the offending
// row. The nest applied nothing (§ F2 detail's all-or-nothing bullet), so the
// error describes the true state — the lexicon has no shape for a partial
// success, and answering 200 with some rows missing would be a lie.
func TestApplyWritesFailsTheWholeCallAndNamesTheRefusedRow(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	// The nest refuses row 1 only; rows 0 and 2 come back unapplied.
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		out := make([]wsrpc.ExternalWriteResult, len(ws))
		for i := range ws {
			if i == 1 {
				out[i] = wsrpc.ExternalWriteResult{Refusal: &wsrpc.ExternalWriteRefusal{
					SubType: "policy",
					Message: "posts are immutable; delete the post and create a new one",
				}}
				continue
			}
			out[i] = wsrpc.ExternalWriteResult{Refusal: &wsrpc.ExternalWriteRefusal{
				SubType: "policy",
				Message: "not applied: another write in this batch was refused",
			}}
		}
		return out
	}

	body := applyWritesBody("",
		`{"$type":"com.atproto.repo.applyWrites#create","collection":"app.bsky.graph.list","value":`+listRecord+`}`,
		`{"$type":"com.atproto.repo.applyWrites#update","collection":"app.bsky.feed.post","rkey":"3x","value":`+postRecord+`}`,
		// Schema-valid on purpose: this row exists to be refused BY THE NEST,
		// and a bare fixture would now be refused by the pre-flight's Lexicon
		// check first — masking the very ordering the test asserts.
		`{"$type":"com.atproto.repo.applyWrites#create","collection":"app.bsky.feed.threadgate","value":`+
			`{"$type":"app.bsky.feed.threadgate","post":"at://`+testDIDForAlice+`/app.bsky.feed.post/3x",`+
			`"createdAt":"2026-07-24T10:00:00.000Z"}}`,
	)
	resp := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("a refused row: %d, want 400", resp.StatusCode)
	}
	var body2 struct {
		Error   string `json:"error"`
		Message string `json:"message"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&body2); err != nil {
		t.Fatalf("decode: %v", err)
	}
	// It must name WHICH row, or the caller cannot act on it.
	if !strings.Contains(body2.Message, "write 0") {
		t.Fatalf("the error must name the first refused row, got %q", body2.Message)
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	if len(w.ops) != 0 {
		t.Fatal("a refused batch must reach the repo in no part")
	}
}

// An unknown union member is refused BY NAME rather than guessed at. Reading a
// stranger as one of the three known members would apply a write the caller
// never asked for.
func TestApplyWritesRefusesAnUnknownMemberType(t *testing.T) {
	f, _ := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	f.nest.ingest = perWriteRkeys("3batch")

	body := applyWritesBody("",
		`{"$type":"com.atproto.repo.applyWrites#upsert","collection":"app.bsky.feed.post","value":`+postRecord+`}`,
	)
	resp := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt, body)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("unknown member $type: %d, want 400", resp.StatusCode)
	}
	if f.nest.ingestCalls != 0 {
		t.Fatal("a malformed batch must not reach the nest")
	}
}

// applyWrites runs the same gates the single-record verbs do — they are
// properties of "this PDS serves no repo write for an account in this state",
// not of a particular verb. A verb-shaped hole in either is the whole point of
// building the batch on the shared path.
func TestApplyWritesRunsTheSameGatesAsTheSingleRecordVerbs(t *testing.T) {
	post := `{"$type":"com.atproto.repo.applyWrites#create","collection":"app.bsky.feed.post","value":` + postRecord + `}`

	t.Run("first-emit gate", func(t *testing.T) {
		f, w := newWriteFixture(t)
		_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
		f.nest.ingest = perWriteRkeys("3batch")
		w.gated = true

		resp := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt, applyWritesBody("", post))
		if resp.StatusCode != http.StatusBadRequest {
			t.Fatalf("first-emit gated batch: %d, want 400", resp.StatusCode)
		}
		if f.nest.ingestCalls != 0 {
			t.Fatal("a gated batch must refuse BEFORE the nest call")
		}
	})

	t.Run("stale swapCommit", func(t *testing.T) {
		f, w := newWriteFixture(t)
		_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
		f.nest.ingest = perWriteRkeys("3batch")
		w.headCID, w.hasHead = "bafycurrent", true

		resp := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt,
			applyWritesBody("bafystale", post))
		if resp.StatusCode != http.StatusBadRequest {
			t.Fatalf("stale swapCommit on a batch: %d, want 400", resp.StatusCode)
		}
		if name := errorName(t, resp); name != "InvalidSwap" {
			t.Fatalf("error name %q, want InvalidSwap", name)
		}
		if f.nest.ingestCalls != 0 {
			t.Fatal("a doomed CAS must refuse before the nest call")
		}
	})

	t.Run("foreign repo", func(t *testing.T) {
		f, _ := newWriteFixture(t)
		_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
		f.nest.ingest = perWriteRkeys("3batch")

		body := []byte(`{"repo":"did:plc:someoneelse","writes":[` + post + `]}`)
		resp := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt, body)
		if resp.StatusCode != http.StatusBadRequest {
			t.Fatalf("foreign repo batch: %d, want 400", resp.StatusCode)
		}
		if f.nest.ingestCalls != 0 {
			t.Fatal("a foreign-repo batch reached the nest")
		}
	})

	t.Run("explicit validate:true is served", func(t *testing.T) {
		f, _ := newWriteFixture(t)
		_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
		f.nest.ingest = perWriteRkeys("3batch")

		body := []byte(`{"repo":"` + testDIDForAlice + `","validate":true,"writes":[` + post + `]}`)
		resp := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt, body)
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("validate:true on a batch: %d, want 200", resp.StatusCode)
		}
	})

	// The batch is ALL OR NOTHING, so one schema-invalid member must fail the
	// whole call BEFORE the nest is asked to apply anything — the same
	// hoisting every other member-level refusal gets.
	t.Run("one invalid member fails the batch before the nest call", func(t *testing.T) {
		f, _ := newWriteFixture(t)
		_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
		f.nest.ingest = perWriteRkeys("3batch")

		bad := `{"$type":"com.atproto.repo.applyWrites#create","collection":"app.bsky.feed.post",` +
			`"value":{"$type":"app.bsky.feed.post","text":"no createdAt"}}`
		body := []byte(`{"repo":"` + testDIDForAlice + `","writes":[` + post + `,` + bad + `]}`)
		resp := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt, body)
		if resp.StatusCode != http.StatusBadRequest {
			t.Fatalf("batch with an invalid member: %d, want 400", resp.StatusCode)
		}
		if f.nest.ingestCalls != 0 {
			t.Fatal("a batch with an invalid member reached the nest")
		}
	})
}

// A batch honours the reproject signal exactly as putRecord does: for a
// collection the projection OWNS, the repo must carry the PROJECTION's
// rendering, not the caller's bytes.
func TestApplyWritesHonoursTheReprojectSignal(t *testing.T) {
	f, w := newWriteFixture(t)
	_, sess := f.createSession(t, "alice", "3ssn-cuqp-4u7r-farx")
	projected := `{"$type":"app.bsky.actor.profile","displayName":"the projection's rendering"}`
	w.renderedRecord = projected
	f.nest.ingest = func(ws []wsrpc.ExternalWrite) []wsrpc.ExternalWriteResult {
		out := make([]wsrpc.ExternalWriteResult, len(ws))
		for i, wr := range ws {
			k := "self"
			uri := "at://did:fauna:x/" + wr.Collection + "/self"
			out[i] = wsrpc.ExternalWriteResult{Rkey: &k, AtURI: &uri, ReprojectRecord: true}
		}
		return out
	}

	body := applyWritesBody("",
		`{"$type":"com.atproto.repo.applyWrites#update","collection":"app.bsky.actor.profile","rkey":"self","value":`+
			`{"$type":"app.bsky.actor.profile","displayName":"the caller's bytes"}}`)
	if s := f.postBody(t, "com.atproto.repo.applyWrites", sess.AccessJwt, body).StatusCode; s != http.StatusOK {
		t.Fatalf("applyWrites: %d", s)
	}

	wantCBOR, err := atprotorepo.JSONRecordToDagCBOR(projected)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(w.lastOps(t)[0].RecordCBOR, wantCBOR) {
		t.Fatal("the repo must carry the projection's rendering, not the caller's record")
	}
	// The batch member's own collection is what gets rendered — not a constant
	// and not the first member's: a mixed batch must render each owned row from
	// its own collection.
	if len(w.renderCalls) != 1 || w.renderCalls[0].collection != "app.bsky.actor.profile" {
		t.Fatalf("render calls = %+v, want one for app.bsky.actor.profile", w.renderCalls)
	}
}
