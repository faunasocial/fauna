package atprotopds

// The F2 authed write surface: com.atproto.repo.{createRecord,putRecord,
// deleteRecord} (docs/goal/behavior/atproto-pds-full.md § F2 detail).
//
// The shape of one write, end to end:
//
//	XRPC call → structural validate → dag-cbor encode + CID (indigo, ONE
//	encoder) → ingest_external_write (nest classifies D2/D6, round-trips or
//	journals, answers {rkey, at_uri, fauna_post_id, reproject_record}) → drive
//	the C1 funnel at the rkey the NEST chose, with the record the nest either
//	accepted from the caller or told us to re-render → answer {uri, cid}.
//
// Three properties are load-bearing and must not be "simplified":
//
//  1. **The nest answers first, and its rkey wins.** A round-tripped post's
//     record key is derived from the Fauna post's creation instant + content id
//     (D1's deterministic TID), so the bridge cannot know it before the call.
//     Committing at a bridge-chosen rkey would put the record somewhere the
//     projection loop would never look, and the account would end up with two
//     records for one post.
//
//  2. **The funnel commit carries the Fauna post id.** That is what writes the
//     post_map row, and post_map is how the projection loop recognizes this
//     post as already projected. Without it the loop re-projects the post and
//     overwrites the caller's own record bytes with the translator's rendering
//     of them — handing the caller back a different record than it wrote.
//
//  3. **When the nest says the projection owns the record, WE render it.** For a
//     collection the projection OWNS and re-derives (the app.bsky.actor.profile
//     singleton), the caller's record is an input to a Fauna update, not the
//     record that results. Committing the caller's bytes there would have the
//     next projection pass overwrite them, so the cid answered synchronously
//     would name bytes that do not survive — and getRecord would disagree with
//     the write's own answer. On res.ReprojectRecord this side renders the
//     record through the SAME function a projection pass calls
//     (RepoWriter.RenderProjectedRecord → atprotorepo.Projector.RenderProfileRecord),
//     which makes the next pass a no-op by construction. It is rendered HERE and
//     not nest-side because two of the answers exist only here: a picture's
//     ATProto CID/MIME/size come from the blob store keyed by its Fauna CID, and
//     whether its bytes are a publishable image is sniffed from the bytes
//     themselves (F2.4 slice 3).

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotolex"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// maxWriteBodyBytes caps the XRPC request body. The record itself is checked
// against the wire's own record cap after encoding; this is the envelope bound
// so an oversized body never reaches the JSON decoder's allocator.
const maxWriteBodyBytes = wsrpc.ExternalWriteRecordMaxBytes + 8192

// RepoWriter commits records into an account's ATProto repo and reads the
// post_map index back.
//
// A seam like Authorizer / RepoSignerSource: production (cmd/fauna-atproto-
// bridge) pairs the C1 funnel with the per-account sealed signing key, so the
// unseal — and its cgo — stays out of this package and the handlers below stay
// unit-testable against a fake. A nil writer refuses every write rather than
// serving one it cannot commit.
type RepoWriter interface {
	// Commit applies ops to did's repo, signed with that account's own repo
	// key, as ONE funnel commit. CAS expectations ride as options so the
	// funnel can re-check them under its own per-DID lock.
	Commit(ctx context.Context, actorID []byte, did string, ops []atprotorepo.RepoOp, opts ...atprotorepo.BatchOption) (atprotorepo.CommitResult, error)
	// RepoHead reads the account's current commit CID, or ok=false for a repo
	// with no commit yet. The `swapCommit` pre-check's operand.
	RepoHead(ctx context.Context, did string) (commitCID string, ok bool, err error)
	// RecordCID reads the CID of the record currently at a path, or ok=false
	// when no such record exists. The `swapRecord` pre-check's operand —
	// "exists" is load-bearing on its own, since an explicit null swapRecord
	// asserts precisely that the record is absent.
	RecordCID(ctx context.Context, did, collection, rkey string) (cid string, ok bool, err error)
	// FaunaPostIDForRecord resolves a served record back to the Fauna post it
	// maps from, or ok=false when it maps to none (a journaled record).
	FaunaPostIDForRecord(ctx context.Context, did, collection, rkey string) (string, bool, error)
	// ResolveRecordRefs maps every AT-URI this record refers to (its reply
	// parent, the post it quotes) to the Fauna post it names, skipping the
	// ones that name none.
	//
	// Behind the seam because the extraction is shared Rust reached over the
	// FFI: this package is deliberately cgo-free (it is the fast inner loop),
	// so the cgo call lives with the production writer, exactly like the
	// funnel and the signing key. The bridge never interprets a record's
	// reference semantics itself — see fauna_bridge_atproto::record_refs for
	// why the quote embed in particular is not safe to hand-walk here.
	ResolveRecordRefs(ctx context.Context, recordCBOR []byte) (map[string]string, error)
	// ExtractBlobRefs walks the record for the blob CIDs it references — what
	// the `#commit` frame's `blobs` field must announce, so a consumer learns
	// which blobs to fetch without decoding the record. Behind the seam for
	// the same reason as ResolveRecordRefs: the walk is shared Rust
	// (fauna_bridge_atproto::record_refs, the generic blob-shape walk the nest
	// stamps `referenced_at` from) reached over the FFI, and a Go walker that
	// learned only the embeds it knows would silently under-announce. Pure —
	// no I/O, no error to surface: unreadable bytes announce nothing, which
	// only ever hurts the record's own author.
	ExtractBlobRefs(recordCBOR []byte) []string
	// ResolveBlobRefs maps every blob CID this record references to the Fauna
	// ContentHash (base32) those bytes are, for the refs THIS repo has already
	// published — skipping the ones it has not.
	//
	// The bridge-side half of the inbound-picture resolution
	// (atproto-pds-full.md § F2 detail): an external app that echoes back the
	// avatar ref our own projection emitted is naming bytes only this side can
	// identify, because the Fauna-CID <-> ATProto-CID index is our blob store
	// and the two address spaces are bridged only by holding the bytes. The
	// nest resolves the other half (a fresh uploadBlob) against the ledger it
	// owns, and refuses the batch when neither answers.
	//
	// A miss is a valid answer, never an error — exactly like ResolveRecordRefs:
	// the nest reads an absent key as "not one of ours" and decides from its own
	// store what that means.
	ResolveBlobRefs(ctx context.Context, did string, recordCBOR []byte) (map[string]string, error)
	// FirstEmitGated reports whether did's identity is still behind the
	// pre-firehose resolvability gate — i.e. this account has never had a
	// frame on the firehose and its DID/handle does not yet resolve
	// ecosystem-side. The projection loop owns opening it; the write path only
	// reads it, to refuse before the write has any effect.
	FirstEmitGated(ctx context.Context, did string) (bool, error)
	// RenderProjectedRecord renders the record JSON a PROJECTION PASS would
	// commit for a collection the projection owns, from the account's current
	// Fauna state — the answer to the nest's `reproject_record`.
	//
	// Behind the seam because it needs both cgo (the translator crosses the
	// FFI) and the projector's own blob resolution. That sharing is the whole
	// point: the write path must commit what the next projection pass would,
	// or the cid it answers names bytes that pass overwrites, so it calls the
	// SAME function the pass calls (atprotorepo.Projector.RenderProfileRecord)
	// rather than a rendering kept in step with it. An unknown collection is an
	// error, never a silent fall-back to the caller's bytes: the nest asserted
	// the projection owns this record, and committing the caller's draft would
	// quietly restore the very drift the assertion exists to prevent.
	RenderProjectedRecord(ctx context.Context, actorID []byte, did, collection string) (recordJSON string, err error)
	// NextRkey allocates a record key for a caller that did not supply one.
	NextRkey() string
}

// EnableWrites wires the write surface. Without it the routes still register
// but refuse — the same fail-closed posture as a nil Authorizer.
func (s *Server) EnableWrites(w RepoWriter) { s.writer = w }

// registerWriteRoutes adds the F2 write surface to the shared route table.
//
// ClassWrite, not ClassAuthed: a write costs a nest round-trip plus a signed
// funnel commit. Deliberately NOT Proxyable — a write is served locally by
// definition (D6's served-locally bucket), and honouring an `atproto-proxy`
// header here would let a forged header push a user's post to a third party.
func (s *Server) registerWriteRoutes(x *xrpc.Server) {
	x.Register(xrpc.Route{
		NSID: "com.atproto.repo.createRecord", Method: http.MethodPost,
		Auth: xrpc.Session, Class: xrpc.ClassWrite, Handle: s.createRecord,
	})
	x.Register(xrpc.Route{
		NSID: "com.atproto.repo.putRecord", Method: http.MethodPost,
		Auth: xrpc.Session, Class: xrpc.ClassWrite, Handle: s.putRecord,
	})
	x.Register(xrpc.Route{
		NSID: "com.atproto.repo.deleteRecord", Method: http.MethodPost,
		Auth: xrpc.Session, Class: xrpc.ClassWrite, Handle: s.deleteRecord,
	})
	x.Register(xrpc.Route{
		NSID: "com.atproto.repo.applyWrites", Method: http.MethodPost,
		Auth: xrpc.Session, Class: xrpc.ClassWrite, Handle: s.applyWrites,
	})
}

type createRecordRequest struct {
	Repo       string          `json:"repo"`
	Collection string          `json:"collection"`
	Rkey       string          `json:"rkey"`
	Validate   *bool           `json:"validate"`
	Record     json.RawMessage `json:"record"`
	SwapCommit string          `json:"swapCommit"`
}

// putRecordRequest is createRecordRequest plus the record-level CAS parameter
// and a REQUIRED rkey: `putRecord` addresses a specific record, so unlike
// `createRecord` there is no server-chosen key.
type putRecordRequest struct {
	Repo       string          `json:"repo"`
	Collection string          `json:"collection"`
	Rkey       string          `json:"rkey"`
	Validate   *bool           `json:"validate"`
	Record     json.RawMessage `json:"record"`
	// RawMessage, not string: `putRecord`'s swapRecord is explicitly NULLABLE,
	// and an explicit `null` is itself a CAS assertion ("this record must not
	// already exist"). Decoding into a string would silently flatten that into
	// "no CAS requested" and serve a write the caller asked us to guard.
	SwapRecord json.RawMessage `json:"swapRecord"`
	SwapCommit string          `json:"swapCommit"`
}

type deleteRecordRequest struct {
	Repo       string `json:"repo"`
	Collection string `json:"collection"`
	Rkey       string `json:"rkey"`
	SwapRecord string `json:"swapRecord"`
	SwapCommit string `json:"swapCommit"`
}

// commitRef is the lexicon's commit reference on a write reply.
type commitRef struct {
	CID string `json:"cid"`
	Rev string `json:"rev"`
}

type createRecordResponse struct {
	URI    string     `json:"uri"`
	CID    string     `json:"cid"`
	Commit *commitRef `json:"commit,omitempty"`
	// ValidationStatus is "unknown", never "valid": this PDS does not resolve
	// Lexicon schemas, so claiming "valid" would assert a check nobody ran.
	// The lexicon defines "unknown" for exactly this.
	ValidationStatus string `json:"validationStatus"`
}

type deleteRecordResponse struct {
	Commit *commitRef `json:"commit,omitempty"`
}

func (s *Server) createRecord(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller) {
	var body createRecordRequest
	if err := decodeWriteBody(w, r, &body); err != nil {
		xrpc.WriteError(w, err)
		return
	}
	s.applyRecordWrite(w, r, caller, recordWrite{
		verb:       "createRecord",
		action:     wsrpc.ExternalWriteActionCreate,
		commitOp:   atprotorepo.ActionCreate,
		repo:       body.Repo,
		collection: body.Collection,
		rkey:       body.Rkey,
		validate:   body.Validate,
		record:     body.Record,
		cas:        casExpect{commit: body.SwapCommit},
	})
}

// putRecord — `com.atproto.repo.putRecord`, the mutable-singleton write.
//
// Same path as createRecord, one action apart: the D2/D6 classifier is a
// function of (collection, ACTION), so the verb is what tells the nest an
// update was asked for. It routes by lexicon class nest-side
// (atproto-pds-full.md § F2 detail): the profile singleton takes the C2
// sanctioned update path, a post is policy-refused as immutable (matching the
// network — Bluesky posts are not editable either), and a journal collection
// updates in place.
//
// "never putRecord" in the mirror's firm boundaries binds the POST path only —
// it is a statement about the one-way translator, not about which verbs this
// PDS serves (atproto-pds-full.md D1 § Firm-boundary restatement).
func (s *Server) putRecord(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller) {
	var body putRecordRequest
	if err := decodeWriteBody(w, r, &body); err != nil {
		xrpc.WriteError(w, err)
		return
	}
	// An rkey is not optional here: putRecord addresses a specific record, and
	// there is no server-chosen key to fall back on.
	if body.Rkey == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("rkey is required"))
		return
	}
	// A present swapRecord — INCLUDING an explicit `null`, which asserts the
	// record does not exist yet — is a CAS the caller asked for.
	swapRecord, e := parseSwapRecord(body.SwapRecord)
	if e != nil {
		xrpc.WriteError(w, e)
		return
	}
	cas := casExpect{commit: body.SwapCommit}
	if swapRecord != nil {
		cas.records = map[string]atprotorepo.RecordSwap{
			body.Collection + "/" + body.Rkey: *swapRecord,
		}
	}
	s.applyRecordWrite(w, r, caller, recordWrite{
		verb:       "putRecord",
		action:     wsrpc.ExternalWriteActionUpdate,
		commitOp:   atprotorepo.ActionUpdate,
		repo:       body.Repo,
		collection: body.Collection,
		rkey:       body.Rkey,
		validate:   body.Validate,
		record:     body.Record,
		cas:        cas,
	})
}

// recordWrite is one record-carrying write, verb-independent. createRecord and
// putRecord differ only in the action they declare and the funnel op they
// commit — everything between (validation, encoding, the first-emit gate,
// reference resolution, the nest round trip, honouring the nest's answer) is
// one path on purpose, so a check added for one verb can never be missing on
// the other.
type recordWrite struct {
	verb       string
	action     string
	commitOp   string
	repo       string
	collection string
	rkey       string
	validate   *bool
	record     json.RawMessage
	cas        casExpect
}

func (s *Server) applyRecordWrite(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller, rw recordWrite) {
	if e := s.checkWritePreconditions(caller, rw.repo); e != nil {
		xrpc.WriteError(w, e)
		return
	}
	if e := validateCollection(rw.collection); e != nil {
		xrpc.WriteError(w, e)
		return
	}
	if e := validateRkey(rw.rkey); e != nil {
		xrpc.WriteError(w, e)
		return
	}

	recordCBOR, e := encodeRecord(rw.record, rw.collection)
	if e != nil {
		xrpc.WriteError(w, e)
		return
	}

	// Schema validation is part of the PRE-FLIGHT, before the nest call, for
	// the same reason the first-emit gate and the CAS pre-check are: a refusal
	// raised after `ingest_external_write` would leave a real Fauna post behind
	// a write the caller was told failed (§ F2 detail, all-or-nothing).
	validationStatus, e := lexiconValidate(rw.collection, recordCBOR, rw.validate)
	if e != nil {
		xrpc.WriteError(w, e)
		return
	}

	// The candidate rkey. The nest overrides it for a round-tripped post (whose
	// key is derived from the Fauna post) and for the profile singleton (always
	// `self`), and uses it verbatim for a journaled record — which is why one is
	// always sent.
	rkey := rw.rkey
	if rkey == "" {
		rkey = s.writer.NextRkey()
	}

	ctx, cancel := timeoutCtx(r, rpcTimeout)
	defer cancel()

	if e := s.refuseWhileFirstEmitGated(ctx, caller.DID); e != nil {
		xrpc.WriteError(w, e)
		return
	}

	// The CAS pre-check, likewise BEFORE the nest call: a write whose
	// precondition already fails must ingest nothing.
	if e := s.preCheckSwap(ctx, caller.DID, rw.cas); e != nil {
		xrpc.WriteError(w, e)
		return
	}

	// Resolve the record's references BEFORE the nest call: a reply/quote
	// names its target by AT-URI, and only post_map maps that to a Fauna post
	// id. An unresolvable target is not an error — the nest journals the
	// write, which is the ratified answer for a target that is not a Fauna
	// post (most of the network).
	resolved, err := s.writer.ResolveRecordRefs(ctx, recordCBOR)
	if err != nil {
		s.logger.Warn(rw.verb+": resolve record refs", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	// And the record's PICTURE refs, likewise before the nest call: an echoed
	// avatar names bytes only our blob store can identify as Fauna content, and
	// a ref neither side resolves must refuse before anything is applied.
	resolvedMedia, err := s.resolveEchoedMedia(ctx, caller.DID, rw.collection, recordCBOR)
	if err != nil {
		s.logger.Warn(rw.verb+": resolve echoed media", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	cid, err := atprotorepo.RecordCID(recordCBOR)
	if err != nil {
		s.logger.Warn(rw.verb+": cid", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	results, err := wsrpc.IngestExternalWrite(ctx, s.nest, caller.ActorID, []wsrpc.ExternalWrite{{
		Collection:      rw.collection,
		Action:          rw.action,
		Rkey:            &rkey,
		Record:          recordCBOR,
		CID:             &cid,
		ResolvedTargets: resolved,
		ResolvedMedia:   resolvedMedia,
	}})
	if err != nil {
		s.logger.Warn(rw.verb+": nest ingest failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	res := results[0]
	if e := refusalError(res.Refusal); e != nil {
		xrpc.WriteError(w, e)
		return
	}
	if res.Rkey == nil || res.AtURI == nil {
		s.logger.Warn(rw.verb + ": nest applied a write without an rkey")
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	// The nest may replace the record itself, not just its key: for a
	// collection the projection OWNS and re-derives (the profile singleton),
	// the caller's record is an input to a Fauna update rather than the record
	// that results. Committing the caller's bytes there would have the next
	// projection pass overwrite them, and the cid answered below would name
	// bytes that do not survive. The nest asserts the property and WE render
	// it, through the same function a projection pass calls — see the package
	// doc's property 3 for why that rendering cannot live nest-side.
	if res.ReprojectRecord {
		recordCBOR, cid, err = s.reprojectedRecord(ctx, caller.ActorID, caller.DID, rw.collection)
		if err != nil {
			s.logger.Warn(rw.verb+": render the projected record", "err", err)
			xrpc.WriteError(w, xrpc.InternalError())
			return
		}
	}

	// The CAS keeps the path the CALLER named, even where the nest chose a
	// different rkey to commit at: `swapRecord` is a precondition about the
	// record the caller pinned, not about the write's target, so re-keying it
	// onto the committed path would quietly assert something else.
	//
	// SkipUnchangedRecords: a projection pass can land inside the ingest→commit
	// window and commit at this SAME deterministic rkey first (the reconciler
	// race, atproto-pds-full.md § F2 detail's no-CAS residual). When that
	// happens this commit still runs and still wins — the caller's bytes always
	// replace whatever the pass wrote — but for a projection-owned record
	// (`res.ReprojectRecord`, F2.4 slice 3) the two renderings are
	// byte-identical, so without this option the redundant commit would still
	// broadcast a second, no-op `#commit`. This is a pure win: the option only
	// ever short-circuits when the bytes already match, so a real content
	// change (any ordinary post) is committed exactly as before.
	commitOpts := append(rw.cas.batchOptions(), atprotorepo.SkipUnchangedRecords())
	commit, err := s.writer.Commit(ctx, caller.ActorID, caller.DID, []atprotorepo.RepoOp{{
		Action:     rw.commitOp,
		Collection: rw.collection,
		Rkey:       *res.Rkey,
		RecordCBOR: recordCBOR,
		// Walked from the COMMITTED bytes — after the override replacement
		// above — so the frame announces the blobs of the record that lands,
		// never the caller's superseded draft of it.
		BlobCIDs:    s.writer.ExtractBlobRefs(recordCBOR),
		FaunaPostID: derefString(res.FaunaPostID),
	}}, commitOpts...)
	if err != nil {
		// The Fauna-side effect already happened; the repo commit did not. The
		// projection loop is the reconciler for exactly this case — it will
		// project the post on its next pass, since post_map has no row for it.
		// A lost CAS race reaches the caller as InvalidSwap rather than an
		// internal error: it is theirs to retry after re-reading.
		s.logger.Error(rw.verb+": repo commit failed after nest ingest",
			"did", caller.DID, "at_uri", *res.AtURI, "err", err)
		xrpc.WriteError(w, commitError(err))
		return
	}

	xrpc.WriteJSON(w, createRecordResponse{
		URI: *res.AtURI, CID: cid,
		Commit:           commitRefOf(commit),
		ValidationStatus: validationStatus,
	})
}

func (s *Server) deleteRecord(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller) {
	var body deleteRecordRequest
	if err := decodeWriteBody(w, r, &body); err != nil {
		xrpc.WriteError(w, err)
		return
	}
	if e := s.checkWritePreconditions(caller, body.Repo); e != nil {
		xrpc.WriteError(w, e)
		return
	}
	if e := validateCollection(body.Collection); e != nil {
		xrpc.WriteError(w, e)
		return
	}
	if body.Rkey == "" {
		xrpc.WriteError(w, xrpc.InvalidRequest("rkey is required"))
		return
	}
	if e := validateRkey(body.Rkey); e != nil {
		xrpc.WriteError(w, e)
		return
	}

	ctx, cancel := timeoutCtx(r, rpcTimeout)
	defer cancel()

	// Same gate as createRecord, and deliberately not carved out for deletes:
	// the rule is "this PDS serves no repo writes for an account whose identity
	// has not been announced", one rule for both verbs. A delete during the
	// window would also tombstone the Fauna post nest-side while the bridge
	// no-ops on an empty repo — a split the single rule never creates.
	if e := s.refuseWhileFirstEmitGated(ctx, caller.DID); e != nil {
		xrpc.WriteError(w, e)
		return
	}

	// deleteRecord's swapRecord is a plain (non-nullable) CID string: there is
	// no "must not exist" assertion to make about a record you are deleting.
	cas := casExpect{commit: body.SwapCommit}
	if body.SwapRecord != "" {
		cas.records = map[string]atprotorepo.RecordSwap{
			body.Collection + "/" + body.Rkey: {CID: body.SwapRecord},
		}
	}
	if e := s.preCheckSwap(ctx, caller.DID, cas); e != nil {
		xrpc.WriteError(w, e)
		return
	}
	rkey := body.Rkey

	// The post_map lookup happens BEFORE the nest call, not after it. The
	// funnel needs the id either way (to drop the post_map row in the same txn
	// as the record), but the NEST needs it too: tombstoning the Fauna post
	// this record maps to requires the id, and nothing resolves rkey -> Fauna
	// post id nest-side. A record mapping to nothing resolves to "", which the
	// nest reads as a journaled record and tombstones in the journal instead.
	faunaPostID, mapped, err := s.writer.FaunaPostIDForRecord(ctx, caller.DID, body.Collection, body.Rkey)
	if err != nil {
		s.logger.Warn("deleteRecord: post_map lookup", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	var resolved map[string]string
	if mapped {
		resolved = map[string]string{
			atprotorepo.ATURI(caller.DID, body.Collection, body.Rkey): faunaPostID,
		}
	}

	results, err := wsrpc.IngestExternalWrite(ctx, s.nest, caller.ActorID, []wsrpc.ExternalWrite{{
		Collection:      body.Collection,
		Action:          wsrpc.ExternalWriteActionDelete,
		Rkey:            &rkey,
		ResolvedTargets: resolved,
		// No ResolvedMedia: a delete carries no record, so it references no
		// pictures — there is nothing to resolve and nothing to vouch for.
	}})
	if err != nil {
		s.logger.Warn("deleteRecord: nest ingest failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	if e := refusalError(results[0].Refusal); e != nil {
		xrpc.WriteError(w, e)
		return
	}

	commit, err := s.writer.Commit(ctx, caller.ActorID, caller.DID, []atprotorepo.RepoOp{{
		Action:      atprotorepo.ActionDelete,
		Collection:  body.Collection,
		Rkey:        body.Rkey,
		FaunaPostID: faunaPostID,
	}}, cas.batchOptions()...)
	if err != nil {
		s.logger.Error("deleteRecord: repo commit failed after nest ingest",
			"did", caller.DID, "collection", body.Collection, "rkey", body.Rkey, "err", err)
		xrpc.WriteError(w, commitError(err))
		return
	}
	xrpc.WriteJSON(w, deleteRecordResponse{Commit: commitRefOf(commit)})
}

// ── applyWrites: the batch verb ──────────────────────────────────

// applyWritesRequest is `com.atproto.repo.applyWrites`. Its `writes` are a
// lexicon union discriminated by `$type`, decoded raw so an unknown member is
// refused by name rather than silently read as one of the three known ones.
type applyWritesRequest struct {
	Repo       string            `json:"repo"`
	Validate   *bool             `json:"validate"`
	Writes     []json.RawMessage `json:"writes"`
	SwapCommit string            `json:"swapCommit"`
}

// applyWritesMember is the shape shared by the union's three members. `value`
// is absent on #delete and required on the other two.
type applyWritesMember struct {
	Type       string          `json:"$type"`
	Collection string          `json:"collection"`
	Rkey       string          `json:"rkey"`
	Value      json.RawMessage `json:"value"`
}

const (
	applyWritesCreate = "com.atproto.repo.applyWrites#create"
	applyWritesUpdate = "com.atproto.repo.applyWrites#update"
	applyWritesDelete = "com.atproto.repo.applyWrites#delete"
)

// maxApplyWritesLen bounds one batch. The nest re-checks its own cap; this is
// the envelope bound, so a pathological batch never reaches the per-write
// encode loop.
const maxApplyWritesLen = 200

type applyWritesResult struct {
	Type string `json:"$type"`
	// URI/CID/ValidationStatus are absent on a #deleteResult, which the
	// lexicon defines as an empty object.
	URI              string `json:"uri,omitempty"`
	CID              string `json:"cid,omitempty"`
	ValidationStatus string `json:"validationStatus,omitempty"`
}

type applyWritesResponse struct {
	Commit  *commitRef          `json:"commit,omitempty"`
	Results []applyWritesResult `json:"results"`
}

// applyWrites — `com.atproto.repo.applyWrites`, the batch verb.
//
// It is ONE nest round-trip and ONE funnel commit (atproto-pds-full.md § F2
// detail, "`applyWrites` = one funnel commit"), built on the same path the
// single-record verbs use rather than forking it: the same preconditions, the
// same first-emit gate, the same CAS, the same reference resolution, the same
// honouring of the nest's rkey and record override.
//
// **The batch is all-or-nothing**, which is the property this verb exists to
// provide and the reason the nest applies no row when any row refuses (§ F2
// detail's all-or-nothing bullet). The lexicon's reply is a results array or
// an error — there is no shape for "3 of your 5 writes happened" — so a
// refusal anywhere becomes a whole-call error that NAMES the offending row,
// and nothing was applied to name it about.
func (s *Server) applyWrites(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller) {
	var body applyWritesRequest
	if err := decodeWriteBody(w, r, &body); err != nil {
		xrpc.WriteError(w, err)
		return
	}
	if e := s.checkWritePreconditions(caller, body.Repo); e != nil {
		xrpc.WriteError(w, e)
		return
	}
	if len(body.Writes) == 0 {
		xrpc.WriteError(w, xrpc.InvalidRequest("writes must not be empty"))
		return
	}
	if len(body.Writes) > maxApplyWritesLen {
		xrpc.WriteError(w, xrpc.InvalidRequest(fmt.Sprintf(
			"a batch is limited to %d writes", maxApplyWritesLen)))
		return
	}
	ctx, cancel := timeoutCtx(r, rpcTimeout)
	defer cancel()

	if e := s.refuseWhileFirstEmitGated(ctx, caller.DID); e != nil {
		xrpc.WriteError(w, e)
		return
	}

	cas := casExpect{commit: body.SwapCommit}
	if e := s.preCheckSwap(ctx, caller.DID, cas); e != nil {
		xrpc.WriteError(w, e)
		return
	}

	writes := make([]wsrpc.ExternalWrite, 0, len(body.Writes))
	members := make([]applyWritesMember, 0, len(body.Writes))
	// One status per member, positional. Schema validation runs HERE, in the
	// pre-flight loop, and not after the ingest: a batch is all-or-nothing, so
	// a refusal raised once rows had been applied could not be honoured (§ F2
	// detail — every refusal a member can raise is hoisted up front).
	statuses := make([]string, 0, len(body.Writes))
	for i, raw := range body.Writes {
		m, e := decodeApplyWritesMember(raw, i)
		if e != nil {
			xrpc.WriteError(w, e)
			return
		}
		ew, e := s.externalWriteForMember(ctx, caller.DID, m, i)
		if e != nil {
			xrpc.WriteError(w, e)
			return
		}
		// A #delete carries no record, and a #deleteResult carries no
		// validationStatus — there is nothing to validate or to report.
		status := ""
		if m.Type != applyWritesDelete {
			status, e = lexiconValidate(m.Collection, ew.Record, body.Validate)
			if e != nil {
				e.Message = fmt.Sprintf("write %d (%s): %s", i, m.Collection, e.Message)
				xrpc.WriteError(w, e)
				return
			}
		}
		members = append(members, m)
		writes = append(writes, ew)
		statuses = append(statuses, status)
	}

	results, err := wsrpc.IngestExternalWrite(ctx, s.nest, caller.ActorID, writes)
	if err != nil {
		s.logger.Warn("applyWrites: nest ingest failed", "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	// A refusal anywhere fails the whole call, naming the row. The nest applied
	// nothing — that is the all-or-nothing rule — so this error describes the
	// true state rather than papering over a partial application.
	for i, res := range results {
		if e := refusalError(res.Refusal); e != nil {
			e.Message = fmt.Sprintf("write %d (%s): %s", i, members[i].Collection, e.Message)
			xrpc.WriteError(w, e)
			return
		}
	}

	ops := make([]atprotorepo.RepoOp, 0, len(results))
	out := make([]applyWritesResult, 0, len(results))
	for i, res := range results {
		m := members[i]
		if res.Rkey == nil || res.AtURI == nil {
			s.logger.Warn("applyWrites: nest applied a write without an rkey", "index", i)
			xrpc.WriteError(w, xrpc.InternalError())
			return
		}
		op := atprotorepo.RepoOp{
			Collection:  m.Collection,
			Rkey:        *res.Rkey,
			FaunaPostID: derefString(res.FaunaPostID),
		}
		switch m.Type {
		case applyWritesCreate:
			op.Action = atprotorepo.ActionCreate
		case applyWritesUpdate:
			op.Action = atprotorepo.ActionUpdate
		default:
			op.Action = atprotorepo.ActionDelete
		}

		if op.Action != atprotorepo.ActionDelete {
			recordCBOR, cid, e := s.recordBytesForCommit(ctx, caller, m, res)
			if e != nil {
				xrpc.WriteError(w, e)
				return
			}
			op.RecordCBOR = recordCBOR
			// From the committed bytes, override included — same rule as the
			// single-record path.
			op.BlobCIDs = s.writer.ExtractBlobRefs(recordCBOR)
			out = append(out, applyWritesResult{
				Type:             resultTypeFor(m.Type),
				URI:              *res.AtURI,
				CID:              cid,
				ValidationStatus: statuses[i],
			})
		} else {
			out = append(out, applyWritesResult{Type: resultTypeFor(m.Type)})
		}
		ops = append(ops, op)
	}

	commit, err := s.writer.Commit(ctx, caller.ActorID, caller.DID, ops, cas.batchOptions()...)
	if err != nil {
		s.logger.Error("applyWrites: repo commit failed after nest ingest",
			"did", caller.DID, "writes", len(ops), "err", err)
		xrpc.WriteError(w, commitError(err))
		return
	}
	xrpc.WriteJSON(w, applyWritesResponse{Commit: commitRefOf(commit), Results: out})
}

// decodeApplyWritesMember reads one union member, refusing an unknown `$type`
// by name. Guessing which of the three a stranger meant would apply a write
// the caller did not ask for.
func decodeApplyWritesMember(raw json.RawMessage, i int) (applyWritesMember, *xrpc.Error) {
	var m applyWritesMember
	if err := json.Unmarshal(raw, &m); err != nil {
		return m, xrpc.InvalidRequest(fmt.Sprintf("write %d is not an object", i))
	}
	switch m.Type {
	case applyWritesCreate, applyWritesUpdate, applyWritesDelete:
	default:
		return m, xrpc.InvalidRequest(fmt.Sprintf(
			"write %d has unknown $type %q", i, m.Type))
	}
	if e := validateCollection(m.Collection); e != nil {
		e.Message = fmt.Sprintf("write %d: %s", i, e.Message)
		return m, e
	}
	// #update and #delete address an existing record, so neither has a
	// server-chosen key to fall back on.
	if m.Rkey == "" && m.Type != applyWritesCreate {
		return m, xrpc.InvalidRequest(fmt.Sprintf("write %d: rkey is required", i))
	}
	if e := validateRkey(m.Rkey); e != nil {
		e.Message = fmt.Sprintf("write %d: %s", i, e.Message)
		return m, e
	}
	return m, nil
}

// resolveEchoedMedia answers the nest's `resolved_media` for one record: which
// of the blob refs it carries are already-published bytes of THIS repo.
//
// ONE owner, two callers (the single-record path and each applyWrites member),
// so the two cannot send the nest different answers about the same record.
//
// **Scoped to the profile collection, deliberately.** A picture is the only ref
// class with an echo case: the projection publishes the account's avatar/banner,
// so an external app editing a bio hands those very refs back, and nothing but
// this store can say what Fauna content they are. A post's images have no echo
// case — F2.4 slice 2 ratified that an external app uploads them, and the nest
// refuses a post ref it has no `atproto_blobs` row for.
//
// ⚠ Widening this is a product ruling (it would make a post able to reference
// already-published media), not a tidy-up, and it takes TWO changes, not one:
// the nest independently decides which rows may consult a vouch at all
// (`resolves_echoed_media` in bridge_atproto_handlers.rs) and declines to look
// for anything but a profile row. That is deliberate — a pre-flight trusting
// the producer's discretion is the finding's shape — so widening here alone
// changes nothing, and the nest-side pins would still refuse.
func (s *Server) resolveEchoedMedia(
	ctx context.Context, did, collection string, recordCBOR []byte,
) (map[string]string, error) {
	if collection != atprotorepo.NsidProfile {
		return nil, nil
	}
	return s.writer.ResolveBlobRefs(ctx, did, recordCBOR)
}

// externalWriteForMember turns one decoded member into the wire row the nest
// classifies — the same encode + reference-resolution the single-record path
// does, per member.
func (s *Server) externalWriteForMember(
	ctx context.Context, did string, m applyWritesMember, i int,
) (wsrpc.ExternalWrite, *xrpc.Error) {
	rkey := m.Rkey
	if m.Type == applyWritesDelete {
		// Same ordering as deleteRecord: the post_map lookup precedes the nest
		// call, because the nest cannot tombstone a Fauna post it cannot name.
		faunaPostID, mapped, err := s.writer.FaunaPostIDForRecord(ctx, did, m.Collection, m.Rkey)
		if err != nil {
			s.logger.Warn("applyWrites: post_map lookup", "index", i, "err", err)
			return wsrpc.ExternalWrite{}, xrpc.InternalError()
		}
		var resolved map[string]string
		if mapped {
			resolved = map[string]string{
				atprotorepo.ATURI(did, m.Collection, m.Rkey): faunaPostID,
			}
		}
		return wsrpc.ExternalWrite{
			Collection:      m.Collection,
			Action:          wsrpc.ExternalWriteActionDelete,
			Rkey:            &rkey,
			ResolvedTargets: resolved,
		}, nil
	}

	recordCBOR, e := encodeRecord(m.Value, m.Collection)
	if e != nil {
		e.Message = fmt.Sprintf("write %d: %s", i, e.Message)
		return wsrpc.ExternalWrite{}, e
	}
	if rkey == "" {
		rkey = s.writer.NextRkey()
	}
	resolved, err := s.writer.ResolveRecordRefs(ctx, recordCBOR)
	if err != nil {
		s.logger.Warn("applyWrites: resolve record refs", "index", i, "err", err)
		return wsrpc.ExternalWrite{}, xrpc.InternalError()
	}
	resolvedMedia, err := s.resolveEchoedMedia(ctx, did, m.Collection, recordCBOR)
	if err != nil {
		s.logger.Warn("applyWrites: resolve echoed media", "index", i, "err", err)
		return wsrpc.ExternalWrite{}, xrpc.InternalError()
	}
	cid, err := atprotorepo.RecordCID(recordCBOR)
	if err != nil {
		s.logger.Warn("applyWrites: cid", "index", i, "err", err)
		return wsrpc.ExternalWrite{}, xrpc.InternalError()
	}
	action := wsrpc.ExternalWriteActionCreate
	if m.Type == applyWritesUpdate {
		action = wsrpc.ExternalWriteActionUpdate
	}
	return wsrpc.ExternalWrite{
		Collection:      m.Collection,
		Action:          action,
		Rkey:            &rkey,
		Record:          recordCBOR,
		CID:             &cid,
		ResolvedTargets: resolved,
		ResolvedMedia:   resolvedMedia,
	}, nil
}

// reprojectedRecord renders, encodes and CIDs the record a projection pass would
// commit for a projection-owned collection — the nest's `reproject_record`
// answer. ONE owner, so the single-record path and applyWrites cannot commit
// different bytes for the same write.
//
// The encode runs through the SAME indigo encoder every other record takes: no
// second encoder, so the cid is computed the one way it ever is.
func (s *Server) reprojectedRecord(
	ctx context.Context, actorID []byte, did, collection string,
) (recordCBOR []byte, cid string, err error) {
	recordJSON, err := s.writer.RenderProjectedRecord(ctx, actorID, did, collection)
	if err != nil {
		return nil, "", fmt.Errorf("render %s: %w", collection, err)
	}
	recordCBOR, err = atprotorepo.JSONRecordToDagCBOR(recordJSON)
	if err != nil {
		return nil, "", fmt.Errorf("encode the projected %s record: %w", collection, err)
	}
	cid, err = atprotorepo.RecordCID(recordCBOR)
	if err != nil {
		return nil, "", fmt.Errorf("cid of the projected %s record: %w", collection, err)
	}
	return recordCBOR, cid, nil
}

// recordBytesForCommit re-derives what the repo must carry for one member,
// honouring the nest's reproject signal exactly as the single-record path does.
func (s *Server) recordBytesForCommit(
	ctx context.Context, caller *xrpc.Caller, m applyWritesMember, res wsrpc.ExternalWriteResult,
) ([]byte, string, *xrpc.Error) {
	if res.ReprojectRecord {
		recordCBOR, cid, err := s.reprojectedRecord(ctx, caller.ActorID, caller.DID, m.Collection)
		if err != nil {
			s.logger.Warn("applyWrites: render the projected record", "err", err)
			return nil, "", xrpc.InternalError()
		}
		return recordCBOR, cid, nil
	}
	recordCBOR, err := atprotorepo.JSONRecordToDagCBOR(string(m.Value))
	if err != nil {
		s.logger.Warn("applyWrites: encode record for commit", "err", err)
		return nil, "", xrpc.InternalError()
	}
	cid, err := atprotorepo.RecordCID(recordCBOR)
	if err != nil {
		s.logger.Warn("applyWrites: cid for commit", "err", err)
		return nil, "", xrpc.InternalError()
	}
	return recordCBOR, cid, nil
}

func resultTypeFor(memberType string) string {
	switch memberType {
	case applyWritesCreate:
		return "com.atproto.repo.applyWrites#createResult"
	case applyWritesUpdate:
		return "com.atproto.repo.applyWrites#updateResult"
	default:
		return "com.atproto.repo.applyWrites#deleteResult"
	}
}

// ── shared plumbing ─────────────────────────────────────────────

func decodeWriteBody(w http.ResponseWriter, r *http.Request, into any) *xrpc.Error {
	dec := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxWriteBodyBytes))
	if err := dec.Decode(into); err != nil {
		return xrpc.InvalidRequest("request body too large or malformed")
	}
	return nil
}

// checkWritePreconditions holds the checks every write shares, single-record
// and batch alike: the writer is wired, and the caller owns the repo it names.
//
// The COLLECTION is deliberately not checked here — `applyWrites` carries one
// per member rather than one per call, so it is validated per write by
// [validateCollection] instead of being faked into this signature.
func (s *Server) checkWritePreconditions(caller *xrpc.Caller, repo string) *xrpc.Error {
	if s.writer == nil {
		e := xrpc.MethodNotImplemented()
		e.Message = "this PDS is not serving repo writes"
		return e
	}
	// A session authenticates ONE account, so `repo` may only name that
	// account. Anything else is a caller error, never a silent redirect of the
	// write to whatever repo the token happens to own.
	if repo == "" {
		return xrpc.InvalidRequest("repo is required")
	}
	if repo != caller.DID && !strings.EqualFold(repo, caller.Handle) {
		return xrpc.InvalidRequest("repo does not match the authenticated account")
	}
	return nil
}

func validateCollection(collection string) *xrpc.Error {
	if collection == "" || strings.ContainsAny(collection, "/ \t\n") {
		return xrpc.InvalidRequest("collection must be an NSID")
	}
	return nil
}

// casExpect is the compare-and-swap one write asked for: a repo-head pin
// (`swapCommit`) and/or a record pin (`swapRecord`), either of which may be
// absent.
type casExpect struct {
	commit string
	// records is keyed by `collection/rkey`, matching the funnel's own paths.
	// nil when no record CAS was asked for.
	records map[string]atprotorepo.RecordSwap
}

func (c casExpect) requested() bool { return c.commit != "" || len(c.records) > 0 }

// batchOptions turns the expectations into the funnel options that re-check
// them under the per-DID lock — the CAS guarantee, of which the pre-check
// below is only the good error message.
func (c casExpect) batchOptions() []atprotorepo.BatchOption {
	var opts []atprotorepo.BatchOption
	if c.commit != "" {
		opts = append(opts, atprotorepo.ExpectCommit(c.commit))
	}
	if len(c.records) > 0 {
		opts = append(opts, atprotorepo.ExpectRecords(c.records))
	}
	return opts
}

// parseSwapRecord decodes `putRecord`'s NULLABLE swapRecord.
//
// Three states, and flattening any two of them serves a write the caller asked
// us to guard: absent (no record CAS), an explicit `null` (the record must NOT
// already exist), and a CID string (the record must currently be that CID).
// This is why the field is decoded from json.RawMessage rather than a string —
// a string decode turns `null` into "" and silently drops the assertion.
func parseSwapRecord(raw json.RawMessage) (*atprotorepo.RecordSwap, *xrpc.Error) {
	if len(raw) == 0 {
		return nil, nil
	}
	if strings.TrimSpace(string(raw)) == "null" {
		return &atprotorepo.RecordSwap{MustNotExist: true}, nil
	}
	var recordCID string
	if err := json.Unmarshal(raw, &recordCID); err != nil {
		return nil, xrpc.InvalidRequest("swapRecord must be a record CID string or null")
	}
	if recordCID == "" {
		return nil, xrpc.InvalidRequest("swapRecord must not be an empty string; use null to assert the record does not exist")
	}
	return &atprotorepo.RecordSwap{CID: recordCID}, nil
}

// preCheckSwap refuses a doomed CAS BEFORE the nest call, which is the whole
// point of checking bridge-side: a write whose precondition already fails must
// not create a Fauna post that the repo then refuses to carry
// (atproto-pds-full.md § F2 detail, the CAS bullet — "nothing ingested").
//
// It is deliberately NOT the guarantee. This read cannot be atomic with the
// commit that follows, so a concurrent writer for the same account can still
// invalidate it in between; the funnel repeats the identical comparison under
// its per-DID lock. Two-site enforcement, the first-emit gate's shape: this is
// the good error message, [atprotorepo.ErrSwapMismatch] is the truth.
func (s *Server) preCheckSwap(ctx context.Context, did string, cas casExpect) *xrpc.Error {
	if !cas.requested() {
		return nil
	}
	if cas.commit != "" {
		headCID, hasHead, err := s.writer.RepoHead(ctx, did)
		if err != nil {
			s.logger.Warn("write: read repo head for swapCommit", "did", did, "err", err)
			return xrpc.InternalError()
		}
		if err := atprotorepo.CheckCommitSwap(cas.commit, headCID, hasHead); err != nil {
			return xrpc.InvalidSwap(err.Error())
		}
	}
	for path, want := range cas.records {
		collection, rkey, ok := strings.Cut(path, "/")
		if !ok {
			return xrpc.InternalError()
		}
		gotCID, exists, err := s.writer.RecordCID(ctx, did, collection, rkey)
		if err != nil {
			s.logger.Warn("write: read record for swapRecord", "did", did, "path", path, "err", err)
			return xrpc.InternalError()
		}
		if err := atprotorepo.CheckRecordSwap(want, path, gotCID, exists); err != nil {
			return xrpc.InvalidSwap(err.Error())
		}
	}
	return nil
}

// commitError maps a funnel commit failure onto the wire. A CAS that lost a
// race with a concurrent writer is the caller's to retry after re-reading, not
// an internal error — the funnel is the site that can see it at all.
func commitError(err error) *xrpc.Error {
	if errors.Is(err, atprotorepo.ErrSwapMismatch) {
		return xrpc.InvalidSwap(err.Error())
	}
	return xrpc.InternalError()
}

// refuseWhileFirstEmitGated refuses a write while this account's identity has
// never been announced on the firehose and does not yet resolve ecosystem-side.
//
// WHY REFUSE, rather than answer {uri, cid} and let the projection loop commit
// once the gate opens (atproto-pds-full.md § F2 detail carries the ruling):
// deferring would answer a CID for bytes that never land — the projection loop
// commits its own TRANSLATED record, not the caller's — and would 404 the URI it
// just handed out, breaking the read-your-writes the synchronous answer exists
// to give. Refusing is also the same answer the write path already gives the
// same person for the sibling condition (no D10 delegation → fauna_surface):
// a setup step only the account owner can finish, named in the message, from
// their Fauna app.
//
// Called BEFORE the nest ingest on every write path, which is the load-bearing
// half: a refusal after it would leave a real Fauna post that all 7 apps show
// while the network never sees it. The funnel's own [atprotorepo.ErrFirstEmitGated]
// chokepoint is the guarantee; this is the good error message.
func (s *Server) refuseWhileFirstEmitGated(ctx context.Context, did string) *xrpc.Error {
	gated, err := s.writer.FirstEmitGated(ctx, did)
	if err != nil {
		s.logger.Warn("write: read first-emit gate", "did", did, "err", err)
		return xrpc.InternalError()
	}
	if !gated {
		return nil
	}
	return &xrpc.Error{
		Status: http.StatusBadRequest,
		Name:   "InvalidRequest",
		Message: "this account's atproto identity is not resolvable on the network yet, " +
			"so this PDS will not publish its first record: the account owner finishes " +
			"publishing the handle's DNS record from their Fauna app, after which writes " +
			"proceed normally",
	}
}

// validateRkey rejects a record key that would corrupt an AT-URI or escape its
// collection. Empty is allowed — createRecord lets the server choose.
func validateRkey(rkey string) *xrpc.Error {
	if rkey == "" {
		return nil
	}
	if len(rkey) > 512 || strings.ContainsAny(rkey, "/ \t\n") || rkey == "." || rkey == ".." {
		return xrpc.InvalidRequest("rkey is not a valid record key")
	}
	return nil
}

// encodeRecord turns the caller's JSON record into canonical dag-cbor through
// the same indigo encoder the projection loop uses, after the structural checks
// that do not need a Lexicon catalog.
//
// `$type` must match the collection when present: a record filed under one
// collection while declaring another would serve correctly here and be read as
// the declared type everywhere downstream.
func encodeRecord(record json.RawMessage, collection string) ([]byte, *xrpc.Error) {
	if len(record) == 0 {
		return nil, xrpc.InvalidRequest("record is required")
	}
	var probe map[string]json.RawMessage
	if err := json.Unmarshal(record, &probe); err != nil {
		return nil, xrpc.InvalidRequest("record must be a JSON object")
	}
	if raw, ok := probe["$type"]; ok {
		var declared string
		if err := json.Unmarshal(raw, &declared); err != nil {
			return nil, xrpc.InvalidRequest("record $type must be a string")
		}
		if declared != collection {
			return nil, xrpc.InvalidRequest(fmt.Sprintf(
				"record $type %q does not match collection %q", declared, collection))
		}
	}
	recordCBOR, err := atprotorepo.JSONRecordToDagCBOR(string(record))
	if err != nil {
		return nil, xrpc.InvalidRequest("record is not encodable as dag-cbor: " + err.Error())
	}
	if len(recordCBOR) > wsrpc.ExternalWriteRecordMaxBytes {
		return nil, xrpc.InvalidRequest("record exceeds the maximum record size")
	}
	return recordCBOR, nil
}

// lexiconValidate applies D2's "lexicon-validated where the lexicon is known"
// rule to one encoded record and answers the `validationStatus` its reply
// carries. ONE owner, two callers (the single-record path and each applyWrites
// member), so the two verbs cannot drift into different verdicts about the
// same record — the same discipline as CheckCommitSwap's two callers.
//
// The three arms, and why each is what it is
// (docs/goal/behavior/atproto-pds-full.md § F2 detail, *Lexicon-schema
// validation*):
//
//   - `validate: false` — the caller's explicit opt-out. No check runs and the
//     status is "unknown", which is the honest answer for a validation nobody
//     performed.
//   - The collection has no record schema here. D2's table rules that
//     unknown/third-party NSIDs get NO schema gate, so the write proceeds and
//     answers "unknown". But a caller that explicitly DEMANDED validation is
//     refused rather than handed that 200: answering success to a demand for a
//     check nobody ran is the exact failure the deferred arm used to prevent.
//     `InvalidRequest`, not `MethodNotImplemented` — the method IS served now,
//     and succeeds for any collection in the catalog; it is this record's
//     lexicon that is unknown here, which is a property of the request.
//   - The collection is known. The record is validated, and only here may the
//     reply say "valid".
//
// Refusing an invalid record is a BEHAVIOR CHANGE for a known collection: a
// malformed post that wrote yesterday is refused today. That is the intended
// reading of D2's table — such a record would fail at every consumer that
// reads it anyway — and `validate: false` remains the escape hatch.
func lexiconValidate(collection string, recordCBOR []byte, demand *bool) (string, *xrpc.Error) {
	if demand != nil && !*demand {
		return "unknown", nil
	}
	cat, err := atprotolex.Shared()
	if err != nil {
		return "", xrpc.InternalError()
	}
	if !cat.KnownRecord(collection) {
		if demand != nil && *demand {
			e := xrpc.InvalidRequest(fmt.Sprintf(
				"this PDS validates records only against the Lexicon schemas it holds, "+
					"and %q is not one of them; omit `validate` to write it unvalidated",
				collection))
			return "", e
		}
		return "unknown", nil
	}
	value, err := atprotorepo.DagCBORRecordToJSONValue(recordCBOR)
	if err != nil {
		// We encoded these bytes moments ago, so this is ours, not the
		// caller's.
		return "", xrpc.InternalError()
	}
	if err := cat.Validate(collection, value); err != nil {
		return "", xrpc.InvalidRequest(fmt.Sprintf(
			"record does not satisfy the Lexicon schema for %s: %v", collection, err))
	}
	return "valid", nil
}

// refusalError maps a nest refusal onto the XRPC wire, preserving D6's
// sub-type in the error NAME — the same discipline as xrpc.DenyFromModule:
// `deferred` becomes MethodNotImplemented so no client (and no later session)
// can read a "not yet" as permanent policy, while policy and fauna-surface
// refusals are caller errors that will not succeed on retry.
func refusalError(r *wsrpc.ExternalWriteRefusal) *xrpc.Error {
	if r == nil {
		return nil
	}
	switch r.SubType {
	case wsrpc.RefusalDeferred:
		e := xrpc.MethodNotImplemented()
		e.Message = r.Message
		return e
	case wsrpc.RefusalPolicy, wsrpc.RefusalFaunaSurface:
		return &xrpc.Error{
			Status: http.StatusBadRequest, Name: "InvalidRequest", Message: r.Message,
		}
	default:
		// An unrecognized sub-type is the nest telling us something we cannot
		// interpret; refusing beats guessing which of the three it meant.
		return &xrpc.Error{
			Status: http.StatusBadRequest, Name: "InvalidRequest", Message: r.Message,
		}
	}
}

func commitRefOf(c atprotorepo.CommitResult) *commitRef {
	if c.NoChange {
		return nil
	}
	return &commitRef{CID: c.CommitCID, Rev: c.Rev}
}

func derefString(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}
