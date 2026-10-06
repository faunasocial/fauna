// Package atprotoread is the bridge's public ATProto read surface — the
// com.atproto.sync.* / repo.* GET methods a relay and AppView use to backfill
// and browse a projected repo (atproto-pds-bridge.md § Architecture). Every
// route registers on F1's single XRPC route table (internal/xrpc, C5 — never a
// second table) as Public / ClassPublicRead: no token, anonymous, per-IP
// rate-limited by the frame.
//
// It reads only from the atprotorepo.Store the projection loop fills; it never
// mutates. The subscribeRepos firehose (a WS upgrade) and /.well-known/did.json
// (not under /xrpc) register separately in the bridge main — the firehose is
// stateful (a broadcaster), and did.json is a plain route.
//
// S3 scope notes carried here rather than silently:
//   - listRepos / listRecords are cursor-paginated (S5): both bound their page
//     with a hard-coded ceiling and return an opaque `cursor` while more
//     remains, so no anonymous caller can make one request that serves an
//     unbounded response. Cursors are keyset (last DID / last rkey), never
//     offsets, so a repo or record changing mid-walk cannot make the next page
//     skip or repeat an entry.
//   - sync.getRecord serves a minimal PROOF CAR (S5): the signed commit, the
//     MST nodes on the path down to the record's key, and the record block. It
//     served the whole repo CAR until then — correct, since a repo trivially
//     contains the proof, but an amplification on an anonymous surface whose
//     factor was the user's entire projected history. On a miss it answers
//     RecordNotFound rather than the lexicon's alternative of a CAR proving
//     non-existence; the exclusion path is walked either way, so serving it is
//     an additive change if a consumer ever needs it.
//   - sync.getRepo serves the repo's CURRENT state (S5): the head commit, the
//     MST nodes reachable from it, and the live record blocks — never the
//     superseded blocks the append-only store also holds (spec grounding on
//     Store.ExportRepo). Its `since` param is likewise accepted and ignored:
//     the lexicon makes the diff optional ("Optionally only a 'diff' since a
//     previous revision"), the current-state export is a conformant superset
//     of any diff, and a correct diff needs no superseded block — so the
//     narrowing forecloses nothing if `since` is ever honored.
//     It is also the one route a page ceiling cannot reach — the lexicon gives
//     it no limit/cursor and its answer IS the whole live repo — so it is
//     bounded by COST instead: the CAR streams (no whole-repo buffer) under a
//     per-block-refreshed response-stall deadline. See the handler.
//   - sync.listBlobs ignores the lexicon's `since` (a repo rev): blobs are keyed
//     by CID here, not by the commit that introduced them. The full set is a
//     conformant superset of any `since` answer; see the handler.
//   - repo.* `repo` params are treated as DIDs (the AppView resolves
//     handle→DID before calling); a handle reads as not-found. describeRepo
//     omits handle/didDoc — the DID document lives in the PLC directory (did:plc)
//     or the user's own domain (did:web), not this store.
package atprotoread

import (
	"errors"
	"log/slog"
	"net/http"
	"net/url"
	"strconv"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// Register wires every read route onto srv.
func Register(srv *xrpc.Server, store *atprotorepo.Store, logger *slog.Logger) {
	if logger == nil {
		logger = slog.Default()
	}
	h := &handlers{store: store, logger: logger}
	get := func(nsid string, fn xrpc.Handler) {
		srv.Register(xrpc.Route{
			NSID:   nsid,
			Method: http.MethodGet,
			Auth:   xrpc.Public,
			Class:  xrpc.ClassPublicRead,
			Handle: fn,
		})
	}
	get("com.atproto.sync.getRepo", h.getRepo)
	get("com.atproto.sync.getLatestCommit", h.getLatestCommit)
	get("com.atproto.sync.getRecord", h.syncGetRecord)
	get("com.atproto.sync.listRepos", h.listRepos)
	get("com.atproto.sync.getRepoStatus", h.getRepoStatus)
	get("com.atproto.sync.getBlob", h.getBlob)
	get("com.atproto.sync.listBlobs", h.listBlobs)
	get("com.atproto.repo.getRecord", h.repoGetRecord)
	get("com.atproto.repo.listRecords", h.listRecords)
	get("com.atproto.repo.describeRepo", h.describeRepo)
}

type handlers struct {
	store  *atprotorepo.Store
	logger *slog.Logger
}

// contentTypeCAR is the CARv1 media type sync.getRepo / getRecord serve.
const contentTypeCAR = "application/vnd.ipld.car"

// Page ceilings for the two list endpoints. Both are UNAUTHENTICATED and
// anonymous, so the ceiling — not the caller's `limit` — is what bounds the
// work one request can ask for; without it a repo with a large projected
// history turns every listRecords call into an unbounded response. Hard-coded
// constants, never configuration (§ Product invariants), the same rule the
// firehose subscriber caps follow. The defaults match what the lexicons
// declare, and a server answering with FEWER records than asked is always
// conformant — the cursor is what carries the rest.
const (
	listReposDefaultLimit   = 500
	listReposMaxLimit       = 1000
	listRecordsDefaultLimit = 50
	listRecordsMaxLimit     = 100
	listBlobsDefaultLimit   = 500
	listBlobsMaxLimit       = 1000
)

// parseLimit reads the lexicons' `limit` query param, clamped into [1, max]
// with def when absent. A malformed or out-of-range value is CLAMPED rather
// than refused: the ceiling is the protection, and rejecting the request would
// only hand an anonymous caller a way to make the endpoint error rather than
// serve. (`limit=0` therefore reads as "unspecified", never as "unbounded".)
func parseLimit(q url.Values, def, max int) int {
	raw := q.Get("limit")
	if raw == "" {
		return def
	}
	n, err := strconv.Atoi(raw)
	if err != nil || n < 1 {
		return def
	}
	if n > max {
		return max
	}
	return n
}

// repoNotFound / recordNotFound / repoDeactivated are the ATProto-named errors
// (HTTP 400 per the lexicons, matching the S0 probe). RepoDeactivated is the
// Sync lexicon's own error name for a getRepo against a deactivated account;
// the content reads return it uniformly so a relay learns the account is
// hosted-but-inactive rather than gone (atproto-pds-bridge.md § Disable &
// revocation, layer 2 — "the repo is no longer served").
func repoNotFound(did string) *xrpc.Error {
	return &xrpc.Error{Status: http.StatusBadRequest, Name: "RepoNotFound", Message: "repo not found: " + did}
}

func recordNotFound() *xrpc.Error {
	return &xrpc.Error{Status: http.StatusBadRequest, Name: "RecordNotFound", Message: "record not found"}
}

func blobNotFound() *xrpc.Error {
	return &xrpc.Error{Status: http.StatusBadRequest, Name: "BlobNotFound", Message: "blob not found"}
}

func repoDeactivated(did string) *xrpc.Error {
	return &xrpc.Error{Status: http.StatusBadRequest, Name: "RepoDeactivated", Message: "repo is deactivated: " + did}
}

// requireServed resolves did's served status and writes the right error when it
// must not be served: RepoNotFound (no repo yet) or RepoDeactivated (layer-2
// step-down). It is the single guard every CONTENT read runs before serving —
// deactivation is checked BEFORE record existence so a deactivated repo never
// leaks which records it holds. Returns ok=false once an error has been written.
func (h *handlers) requireServed(w http.ResponseWriter, r *http.Request, did string) (rev string, ok bool) {
	rev, active, exists, err := h.store.RepoStatus(r.Context(), did)
	if err != nil {
		h.logger.Error("repo status", "did", did, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return "", false
	}
	if !exists {
		xrpc.WriteError(w, repoNotFound(did))
		return "", false
	}
	if !active {
		xrpc.WriteError(w, repoDeactivated(did))
		return "", false
	}
	return rev, true
}

func writeCAR(w http.ResponseWriter, car []byte) {
	w.Header().Set("Content-Type", contentTypeCAR)
	_, _ = w.Write(car)
}

// getRepo serves the repo's current state as a CARv1, STREAMED.
//
// This is the one route whose payload size is the user's whole live repo, and
// the lexicon gives it no `limit`/`cursor` to bound with — so the bound cannot be
// the response, only its cost. Two things do that here, and neither narrows what
// a caller receives: the CAR streams block by block (peak resident memory is one
// block, not the repo materialised twice), and the response-stall deadline is
// re-armed as each block goes out, so a slow-reading peer is cut loose in
// seconds while a fast one is never cut off however large the repo.
//
// (`since` stays accepted-and-ignored, and is deliberately NOT the answer to
// this: an attacker simply omits it. A parameter only a cooperative caller sets
// can narrow a response, never bound an anonymous surface.)
func (h *handlers) getRepo(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	did := r.URL.Query().Get("did")
	if did == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("did is required"))
		return
	}
	if _, ok := h.requireServed(w, r, did); !ok {
		return
	}
	// requireServed already resolved the repo as existing and active, so from
	// here an export failure is OUR storage failing, never the caller's DID
	// being unknown — answering RepoNotFound would make a corrupt tree
	// indistinguishable from a typo'd DID and leave the admin no signal at
	// all. Same treatment as sync.getRecord's proof export above.
	//
	// Setting the CAR type up front is safe on the error paths: xrpc.WriteError
	// Sets application/json, which overwrites it.
	w.Header().Set("Content-Type", contentTypeCAR)
	pw := xrpc.NewProgressWriter(w)
	err := h.store.ExportRepoTo(r.Context(), did, pw)
	if err == nil {
		return
	}
	h.logger.Error("sync.getRepo: repo export failed", "did", did, "bytes_sent", pw.Written(), "err", err)
	if pw.Written() == 0 {
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	// Bytes are already on the wire, so the status is spent. Tearing the
	// response down is the only way a partial CAR does not read as a whole one:
	// ending normally would close a well-formed chunked body and hand the
	// consumer a repo silently missing records. ErrAbortHandler is Go's own
	// "abandon this response" signal — the server closes the connection without
	// logging a stack, and the consumer's CAR read fails, which is the truth.
	panic(http.ErrAbortHandler)
}

func (h *handlers) getLatestCommit(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	did := r.URL.Query().Get("did")
	if did == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("did is required"))
		return
	}
	if _, ok := h.requireServed(w, r, did); !ok {
		return
	}
	rev, commitCID, ok, err := h.store.Head(r.Context(), did)
	if err != nil {
		h.logger.Error("getLatestCommit: store error", "did", did, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	if !ok {
		xrpc.WriteError(w, repoNotFound(did))
		return
	}
	xrpc.WriteJSON(w, map[string]any{"cid": commitCID, "rev": rev})
}

func (h *handlers) syncGetRecord(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	q := r.URL.Query()
	did, collection, rkey := q.Get("did"), q.Get("collection"), q.Get("rkey")
	if did == "" || collection == "" || rkey == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("did, collection and rkey are required"))
		return
	}
	if _, ok := h.requireServed(w, r, did); !ok {
		return
	}
	// A proof CAR: the signed commit, the MST path down to this record, and the
	// record block — not the whole repo (see Store.ExportRecordProof). The
	// record's existence is decided by the walk itself rather than by a prior
	// lookup, so a miss cannot be reported from a different read than the one
	// that built the answer.
	car, err := h.store.ExportRecordProof(r.Context(), did, collection, rkey)
	if errors.Is(err, atprotorepo.ErrRecordNotFound) {
		xrpc.WriteError(w, recordNotFound())
		return
	}
	if err != nil {
		h.logger.Error("sync.getRecord: proof export", "did", did, "collection", collection, "rkey", rkey, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	writeCAR(w, car)
}

func (h *handlers) listRepos(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	q := r.URL.Query()
	limit := parseLimit(q, listReposDefaultLimit, listReposMaxLimit)
	repos, err := h.store.ListReposPage(r.Context(), q.Get("cursor"), limit)
	if err != nil {
		h.logger.Error("listRepos: store error", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	out := make([]any, 0, len(repos))
	for _, rp := range repos {
		entry := map[string]any{"did": rp.DID, "head": rp.CommitCID, "rev": rp.Rev, "active": rp.Active}
		// A deactivated repo stays listed, marked inactive with a status reason —
		// the Sync v1.1 listRepos shape (active + status) exists so a relay learns
		// the account is known-but-inactive rather than assuming it vanished.
		if !rp.Active {
			entry["status"] = atprotorepo.AccountStatusDeactivated
		}
		out = append(out, entry)
	}
	body := map[string]any{"repos": out}
	// A full page means more may remain; a short page is the end of the walk, and
	// omitting the cursor there is what tells a relay to stop rather than poll a
	// page it already has.
	if len(repos) == limit {
		body["cursor"] = repos[len(repos)-1].DID
	}
	xrpc.WriteJSON(w, body)
}

func (h *handlers) getRepoStatus(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	did := r.URL.Query().Get("did")
	if did == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("did is required"))
		return
	}
	rev, active, exists, err := h.store.RepoStatus(r.Context(), did)
	if err != nil {
		h.logger.Error("getRepoStatus: store error", "did", did, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	if !exists {
		xrpc.WriteError(w, repoNotFound(did))
		return
	}
	// getRepoStatus REPORTS the status rather than refusing — that is its whole
	// purpose. A deactivated repo answers active:false + status:"deactivated"
	// (Sync v1.1); an active one omits status.
	resp := map[string]any{"did": did, "active": active, "rev": rev}
	if !active {
		resp["status"] = atprotorepo.AccountStatusDeactivated
	}
	xrpc.WriteJSON(w, resp)
}

// getBlob serves the bytes behind a blob ref a projected record published —
// the endpoint an AppView calls to render an image embed.
//
// Served as application/octet-stream rather than the blob's declared MIME. The
// MIME rides in the record's blob ref, which is what a consumer reads it from,
// and reflecting an attacker-influenceable type on an unauthenticated,
// navigable URL is the navigable-XSS shape the nest's own blob route already
// refuses to take (`blob_routes.rs`); `nosniff` keeps a relabelled
// octet-stream from being sniffed back into markup.
func (h *handlers) getBlob(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	q := r.URL.Query()
	did, blobCID := q.Get("did"), q.Get("cid")
	if did == "" || blobCID == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("did and cid are required"))
		return
	}
	if _, ok := h.requireServed(w, r, did); !ok {
		return
	}
	blob, ok, err := h.store.GetBlob(r.Context(), did, blobCID)
	if err != nil {
		h.logger.Error("sync.getBlob: store error", "did", did, "cid", blobCID, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	if !ok {
		xrpc.WriteError(w, blobNotFound())
		return
	}
	w.Header().Set("Content-Type", "application/octet-stream")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	// The CID names the bytes, so the strongest possible validator is free.
	w.Header().Set("ETag", `"`+blobCID+`"`)
	_, _ = w.Write(blob.Bytes)
}

// listBlobs enumerates the blob CIDs a repo serves — the walk an account
// migration or a completeness check uses.
//
// The lexicon's `since` (a repo rev) is accepted and ignored: this store keys
// blobs by CID, not by the commit that introduced them, so it cannot answer
// "blobs added since rev X" without a per-blob rev column no caller has asked
// for. Serving the FULL set is a conformant superset of any `since` answer —
// the caller gets everything it would have got plus blobs it already has —
// whereas honouring it wrongly would silently withhold blobs.
func (h *handlers) listBlobs(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	q := r.URL.Query()
	did := q.Get("did")
	if did == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("did is required"))
		return
	}
	if _, ok := h.requireServed(w, r, did); !ok {
		return
	}
	limit := parseLimit(q, listBlobsDefaultLimit, listBlobsMaxLimit)
	cids, next, err := h.store.ListBlobCIDs(r.Context(), did, q.Get("cursor"), limit)
	if err != nil {
		h.logger.Error("sync.listBlobs: store error", "did", did, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	// `cids` is a required array in the lexicon — an empty repo answers [],
	// never null, which a strict consumer would reject.
	if cids == nil {
		cids = []string{}
	}
	body := map[string]any{"cids": cids}
	if next != "" {
		body["cursor"] = next
	}
	xrpc.WriteJSON(w, body)
}

func (h *handlers) repoGetRecord(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	q := r.URL.Query()
	did, collection, rkey := q.Get("repo"), q.Get("collection"), q.Get("rkey")
	if did == "" || collection == "" || rkey == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("repo, collection and rkey are required"))
		return
	}
	if _, ok := h.requireServed(w, r, did); !ok {
		return
	}
	recordCID, recordCBOR, ok, err := h.store.GetRecord(r.Context(), did, collection, rkey)
	if err != nil {
		h.logger.Error("repo.getRecord: store error", "did", did, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	if !ok {
		xrpc.WriteError(w, recordNotFound())
		return
	}
	value, err := atprotorepo.DagCBORRecordToJSONValue(recordCBOR)
	if err != nil {
		h.logger.Error("repo.getRecord: decode record", "did", did, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	xrpc.WriteJSON(w, map[string]any{
		"uri":   "at://" + did + "/" + collection + "/" + rkey,
		"cid":   recordCID,
		"value": value,
	})
}

func (h *handlers) listRecords(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	q := r.URL.Query()
	did, collection := q.Get("repo"), q.Get("collection")
	if did == "" || collection == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("repo and collection are required"))
		return
	}
	if _, ok := h.requireServed(w, r, did); !ok {
		return
	}
	limit := parseLimit(q, listRecordsDefaultLimit, listRecordsMaxLimit)
	records, err := h.store.ListRecordsPage(r.Context(), did, collection, q.Get("cursor"), limit)
	if err != nil {
		h.logger.Error("repo.listRecords: store error", "did", did, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	out := make([]any, 0, len(records))
	for _, rec := range records {
		value, derr := atprotorepo.DagCBORRecordToJSONValue(rec.RecordCBOR)
		if derr != nil {
			h.logger.Error("repo.listRecords: decode record", "did", did, "rkey", rec.Rkey, "err", derr)
			xrpc.WriteError(w, xrpc.InternalError())
			return
		}
		out = append(out, map[string]any{
			"uri":   "at://" + did + "/" + collection + "/" + rec.Rkey,
			"cid":   rec.RecordCID,
			"value": value,
		})
	}
	body := map[string]any{"records": out}
	if len(records) == limit {
		body["cursor"] = records[len(records)-1].Rkey
	}
	xrpc.WriteJSON(w, body)
}

func (h *handlers) describeRepo(w http.ResponseWriter, r *http.Request, _ *xrpc.Caller) {
	did := r.URL.Query().Get("repo")
	if did == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("repo is required"))
		return
	}
	if _, ok := h.requireServed(w, r, did); !ok {
		return
	}
	collections, err := h.store.Collections(r.Context(), did)
	if err != nil {
		h.logger.Error("describeRepo: collections", "did", did, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	if collections == nil {
		collections = []string{}
	}
	// handle/didDoc are omitted (not in this store — the DID document lives in
	// the PLC directory / the user's own domain); handleIsCorrect is therefore
	// false. A future slice threads the roster's handle through here.
	xrpc.WriteJSON(w, map[string]any{
		"did":             did,
		"collections":     collections,
		"handleIsCorrect": false,
	})
}
