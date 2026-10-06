package main

// The production atprotopds.RepoWriter: pair the C1 commit funnel with the
// per-account repo signing key so the F2 write handlers can commit without
// knowing how a key is obtained.
//
// Same placement rationale as sealedRepoSigners / ffiAuthorizer: the unseal is
// cgo and the key custody is main's business, so internal/atprotopds stays
// cgo-free behind the seam.
//
// The signer comes from the SAME RepoSignerSource the projection loop and the
// service-auth mint use, so an external write's commit is signed by exactly the
// key that signs a projected commit — one key class, no custody widening (C7).

import (
	"context"
	"fmt"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

type funnelRepoWriter struct {
	funnel  *atprotorepo.Funnel
	store   *atprotorepo.Store
	signers atprotopds.RepoSignerSource
	// projector renders the records the projection OWNS, for the nest's
	// `reproject_record` answer — the SAME projector the projection loop drives,
	// so a write path render and a pass render are one function, not two.
	projector *atprotorepo.Projector
	// nest reads the Fauna state that render is derived from. Nil in the unit
	// tests that only exercise commit/signing.
	nest wsrpc.Caller
	// rkeys allocates record keys for callers that supply none. The funnel's
	// own TID clock is for repo *revisions*; a second clock here keeps a
	// caller-facing rkey from consuming a rev the funnel expects to allocate.
	rkeys atprotorepo.RevClock
}

func newFunnelRepoWriter(
	funnel *atprotorepo.Funnel,
	store *atprotorepo.Store,
	signers atprotopds.RepoSignerSource,
	projector *atprotorepo.Projector,
	nest wsrpc.Caller,
) *funnelRepoWriter {
	return &funnelRepoWriter{
		funnel:    funnel,
		store:     store,
		signers:   signers,
		projector: projector,
		nest:      nest,
		rkeys:     atprotorepo.NewTIDRevClock(),
	}
}

// RenderProjectedRecord answers the nest's `reproject_record` with the record a
// projection pass would commit for a collection the projection owns.
//
// It reads the account's current Fauna state and hands it to the projector's own
// renderer — so the write path's record and the next pass's record are produced
// by one function rather than kept in step, which is the property F2.4 slice 3
// exists to make structural (the picture blob refs and the publishability
// verdict both live on this side; see atprotorepo.Projector.RenderProfileRecord).
//
// An unknown collection is an error rather than a fall-back to the caller's
// bytes: the nest asserted that the projection owns this record, so silently
// committing the caller's draft would restore exactly the drift the assertion
// prevents — and would do it invisibly.
func (w *funnelRepoWriter) RenderProjectedRecord(
	ctx context.Context, actorID []byte, did, collection string,
) (string, error) {
	if collection != atprotorepo.NsidProfile {
		return "", fmt.Errorf("the projection owns no record in %q", collection)
	}
	if w.projector == nil || w.nest == nil {
		return "", fmt.Errorf("no projector wired to render %q", collection)
	}
	profileBytes, err := wsrpc.FetchProfile(ctx, w.nest, actorID)
	if err != nil {
		return "", fmt.Errorf("fetch profile: %w", err)
	}
	if len(profileBytes) == 0 {
		// The arm that produced this write just stored a profile, so an empty
		// read is a real inconsistency, not the "no profile yet" case
		// ProjectProfile tolerates. Rendering `{}` would commit an empty record
		// over the account's real one.
		return "", fmt.Errorf("the nest holds no profile for the account that just updated one")
	}
	recordJSON, _, err := w.projector.RenderProfileRecord(ctx, did, profileBytes)
	if err != nil {
		return "", err
	}
	return recordJSON, nil
}

func (w *funnelRepoWriter) Commit(
	ctx context.Context, actorID []byte, did string, ops []atprotorepo.RepoOp,
	opts ...atprotorepo.BatchOption,
) (atprotorepo.CommitResult, error) {
	signer, err := w.signers.RepoSigner(ctx, actorID)
	if err != nil {
		return atprotorepo.CommitResult{}, fmt.Errorf("resolve repo signer: %w", err)
	}
	// RepoSignerSource hands back the structural signing interface; the funnel
	// wants indigo's concrete key (it signs a commit block, not a digest).
	// Production always supplies that concrete type — a source that does not is
	// a wiring bug, refused here rather than papered over.
	key, ok := signer.(atprotorepo.Signer)
	if !ok {
		return atprotorepo.CommitResult{}, fmt.Errorf("repo signer is not an indigo private key")
	}
	return w.funnel.ApplyBatch(ctx, did, key, ops, opts...)
}

func (w *funnelRepoWriter) FaunaPostIDForRecord(
	ctx context.Context, did, collection, rkey string,
) (string, bool, error) {
	return w.store.FaunaPostIDForRecord(ctx, did, collection, rkey)
}

// RepoHead reads the account's current commit CID for the `swapCommit`
// pre-check. The rev is the funnel's business, not the caller's CAS operand.
func (w *funnelRepoWriter) RepoHead(ctx context.Context, did string) (string, bool, error) {
	_, commitCID, ok, err := w.store.Head(ctx, did)
	return commitCID, ok, err
}

// RecordCID reads the CID of the record currently at a path for the
// `swapRecord` pre-check. The bytes are irrelevant here — only the CID the
// caller pinned against, and whether the record exists at all.
func (w *funnelRepoWriter) RecordCID(
	ctx context.Context, did, collection, rkey string,
) (string, bool, error) {
	recordCID, _, ok, err := w.store.GetRecord(ctx, did, collection, rkey)
	return recordCID, ok, err
}

// ResolveRecordRefs extracts the record's referenced AT-URIs over the FFI and
// resolves each against post_map.
//
// The extraction is shared Rust on purpose (fauna_bridge_atproto::record_refs):
// a quote reaches a record through two embed shapes, one nested inside the
// other, and a Go walker that learned only the simpler one would silently stop
// resolving quote-with-media posts — the nest would journal writes that should
// have round-tripped, with nothing failing loudly. Cross-repo resolution is the
// ordinary case here, since replying to another bridged actor is ordinary.
//
// URIs that resolve to no Fauna post are omitted rather than mapped to "": the
// nest reads an absent key as "not a Fauna post" and journals, so an empty
// result is a valid, meaningful answer and never an error.
func (w *funnelRepoWriter) ResolveRecordRefs(
	ctx context.Context, recordCBOR []byte,
) (map[string]string, error) {
	uris := faunaFfi.AtprotoExtractRecordRefs(recordCBOR)
	if len(uris) == 0 {
		return nil, nil
	}
	resolved := make(map[string]string, len(uris))
	for _, uri := range uris {
		faunaPostID, ok, err := w.store.FaunaPostIDForATURI(ctx, uri)
		if err != nil {
			return nil, fmt.Errorf("resolve %q against post_map: %w", uri, err)
		}
		if ok {
			resolved[uri] = faunaPostID
		}
	}
	return resolved, nil
}

// ExtractBlobRefs walks the record for its blob refs over the FFI — the same
// shared-Rust walk (fauna_bridge_atproto::record_refs) the nest stamps
// `referenced_at` from, so what the `#commit` frame announces and what the
// nest guards against GC can never disagree about which blobs a record names.
func (w *funnelRepoWriter) ExtractBlobRefs(recordCBOR []byte) []string {
	return faunaFfi.AtprotoExtractBlobRefs(recordCBOR)
}

// ResolveBlobRefs walks the record's blob refs over the same FFI walk and asks
// the blob store which Fauna content each already-published one is.
//
// Deliberately the SAME walk ExtractBlobRefs and the nest's `referenced_at`
// stamp use: what we resolve for the nest, what the `#commit` frame announces,
// and what the nest guards against GC are then three readings of one answer
// rather than three walkers kept in step.
//
// Refs this repo has never published are omitted rather than mapped to "": the
// nest reads an absent key as "not ours to vouch for" and falls back to its own
// upload ledger, so an empty result is a valid answer and never an error.
func (w *funnelRepoWriter) ResolveBlobRefs(
	ctx context.Context, did string, recordCBOR []byte,
) (map[string]string, error) {
	cids := faunaFfi.AtprotoExtractBlobRefs(recordCBOR)
	if len(cids) == 0 {
		return nil, nil
	}
	resolved := make(map[string]string, len(cids))
	for _, blobCID := range cids {
		faunaCID, ok, err := w.store.FaunaCIDForBlob(ctx, did, blobCID)
		if err != nil {
			return nil, fmt.Errorf("resolve blob %q against the blob store: %w", blobCID, err)
		}
		if ok {
			resolved[blobCID] = faunaCID
		}
	}
	return resolved, nil
}

func (w *funnelRepoWriter) FirstEmitGated(ctx context.Context, did string) (bool, error) {
	return w.store.FirstEmitGated(ctx, did)
}

func (w *funnelRepoWriter) NextRkey() string { return w.rkeys.Next() }
