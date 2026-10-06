package atprotopds

// The F2.4 blob ingest leg: com.atproto.repo.uploadBlob
// (docs/goal/behavior/atproto-pds-full.md § F2 detail's `uploadBlob` bullet).
//
// One upload, end to end:
//
//	XRPC call (raw bytes) → bounded read → sniff the media type FROM THE BYTES →
//	ATProto blob CID (sha256) → [1] stream the bytes to the nest's bulk-binary
//	carve-out under AudienceClass::PublicPost, getting the blake3 ContentHash →
//	[2] record_blob ties that CID to that ContentHash for this account →
//	[3] the bridge's own blob store makes the bytes servable by
//	com.atproto.sync.getBlob → answer the blob ref.
//
// Three properties are load-bearing:
//
//  1. **The media type is SNIFFED, and an unrecognized sniff REFUSES.** The
//     caller's Content-Type never reaches the sidecar: that value becomes the
//     Content-Type the nest serves on download, so trusting the caller would let
//     it choose what its bytes are later claimed to be. Where the OUTBOUND
//     projection drops an unrenderable attachment and publishes the post anyway,
//     inbound refuses — nothing has been published yet, so the app learns
//     immediately and nothing is half-done, while accepting bytes whose type we
//     invented means serving them under a guess forever. Both directions use one
//     sniffer (atprotorepo.SniffImageMIME) and differ only here.
//
//  2. **The three writes happen in this order, and the order is why every crash
//     converges.** Bytes-then-row-then-serve means no partial state ever hands
//     the caller a ref it cannot resolve: a crash after [1] leaves an orphan in
//     the Fauna media store (the GC's business, and the retry re-derives the
//     identical CID from the identical bytes); a crash after [2] returns an error
//     the caller retries idempotently. Storing in the bridge FIRST was rejected —
//     that leaves bytes com.atproto.sync.getBlob serves with no Fauna media
//     backing, so a record could reference bytes the round-trip cannot resolve.
//
//  3. **The account attribution rides on [2], not on [1].** The byte route
//     authenticates as the BRIDGE's service user and keeps its uploader field for
//     audit only — it cannot know which account uploaded. Only the session-token
//     side holds the authenticated actor, which is why the mapping is its own
//     call rather than something the byte upload infers.

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// BlobIngester lands an uploaded blob on both sides of the CID divide: the
// Fauna media path (BLAKE3-addressed, where a Fauna post's media lives) and the
// bridge's own ATProto blob store (sha256-addressed, what the network fetches).
//
// A seam like [RepoWriter]: production (cmd/fauna-atproto-bridge) pairs the
// nest's bulk-binary byte route with the WS-RPC mapping call and the blob store,
// keeping this package unit-testable against a fake. It is split into three
// methods rather than one on purpose — the ORDER of the writes is a load-bearing
// property (see this file's header), and a single method would bury it where no
// test in this package could pin it.
//
// A nil ingester refuses every upload rather than serving one it cannot persist,
// the same fail-closed posture as a nil RepoWriter or Authorizer.
type BlobIngester interface {
	// StoreInFaunaMedia streams data to the nest over the sanctioned
	// bulk-binary carve-out (POST /api/v1/blob) as AudienceClass::PublicPost —
	// the one class Fauna does not AEAD-seal, which is both what makes this leg
	// possible (the bridge holds no user key material and could seal for no
	// other class) and correct by content (a blob an app.bsky.* record
	// references is public by construction). Answers the blake3 ContentHash the
	// bytes landed under: the name a Fauna post's media carries for them.
	StoreInFaunaMedia(ctx context.Context, mime string, data []byte) (mediaRef []byte, err error)

	// RecordBlobMapping ties cid to mediaRef for actorID in nest state
	// (fauna.bridges.atproto.record_blob) and answers mediaRef spelled as a
	// canonical Fauna content CID. Idempotent: cid is the sha256 of the bytes, so
	// a retry carries the identical pair.
	//
	// The faunaCID comes back from the NEST rather than being derived here. It is
	// the provenance key the projection loop's "already published these bytes?"
	// lookup matches on, so a Go-side spelling that drifted from
	// ContentHash::to_base32 by one character would make every image re-fetch and
	// re-hash forever with nothing failing.
	RecordBlobMapping(ctx context.Context, actorID []byte, cid string, mediaRef []byte) (faunaCID string, err error)

	// ServeBlobBytes makes data fetchable by com.atproto.sync.getBlob under cid
	// for did, recording faunaCID as its provenance so a later projection of the
	// same bytes neither re-fetches nor re-hashes them.
	ServeBlobBytes(ctx context.Context, did, cid, faunaCID, mime string, data []byte) error
}

// EnableBlobUploads wires the blob ingest surface. Without it the route still
// registers but refuses — the same fail-closed posture as a nil RepoWriter.
func (s *Server) EnableBlobUploads(b BlobIngester) { s.blobs = b }

// registerBlobRoutes adds com.atproto.repo.uploadBlob.
//
// ClassBlob, not ClassWrite: a record write's cost is a funnel commit over a few
// KB while a blob body runs to the ceiling, so the two belong in different
// buckets (xrpc.ClassBlob's own doc carries the full reasoning). Deliberately NOT
// Proxyable — uploadBlob is in D6's served-locally bucket, and honouring an
// `atproto-proxy` header here would let a forged header hand a user's image to a
// third party.
func (s *Server) registerBlobRoutes(x *xrpc.Server) {
	x.Register(xrpc.Route{
		NSID: "com.atproto.repo.uploadBlob", Method: http.MethodPost,
		Auth: xrpc.Session, Class: xrpc.ClassBlob, Handle: s.uploadBlob,
	})
}

// blobRef is the lexicon's `blob` type — what a record's image/video embed and a
// profile's avatar/banner carry.
type blobRef struct {
	Type     string      `json:"$type"`
	Ref      blobRefLink `json:"ref"`
	MimeType string      `json:"mimeType"`
	Size     int64       `json:"size"`
}

// blobRefLink is the IPLD link form a blob ref's `ref` takes in JSON.
type blobRefLink struct {
	Link string `json:"$link"`
}

type uploadBlobResponse struct {
	Blob blobRef `json:"blob"`
}

// errBlobTooLarge marks the oversize case so the bounded read and the handler
// agree on one refusal.
var errBlobTooLarge = errors.New("blob exceeds the upload ceiling")

func (s *Server) uploadBlob(w http.ResponseWriter, r *http.Request, caller *xrpc.Caller) {
	if s.blobs == nil {
		xrpc.WriteError(w, xrpc.MethodNotImplemented())
		return
	}

	data, err := readBoundedBlobBody(r.Body)
	if err != nil {
		if errors.Is(err, errBlobTooLarge) {
			// InvalidRequest rather than an invented name: the ceiling is ours,
			// not the lexicon's. Should F5's live interop show third-party
			// clients branching on a `BlobTooLarge` name, that is a one-line
			// addition beside xrpc.InvalidSwap.
			xrpc.WriteError(w, xrpc.InvalidRequest(fmt.Sprintf(
				"blob exceeds the %d-byte upload ceiling", atprotorepo.MaxUploadBlobBytes)))
			return
		}
		xrpc.WriteError(w, xrpc.InvalidRequest("could not read the blob body"))
		return
	}
	if len(data) == 0 {
		xrpc.WriteError(w, xrpc.InvalidRequest("blob body is empty"))
		return
	}

	// The bytes are the only source of truth for what this is (property 1). The
	// caller's declared Content-Type is deliberately not consulted.
	mime, ok := atprotorepo.SniffImageMIME(data)
	if !ok {
		xrpc.WriteError(w, xrpc.InvalidRequest(
			"these bytes are not an image this PDS can publish; upload a PNG, JPEG, GIF or WebP"))
		return
	}

	blobCID, err := atprotorepo.BlobCIDForBytes(data)
	if err != nil {
		s.logger.Error("atproto uploadBlob: blob cid", "did", caller.DID, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}
	cid := blobCID.String()

	ctx := r.Context()

	// [1] The bytes into the Fauna media path.
	mediaRef, err := s.blobs.StoreInFaunaMedia(ctx, mime, data)
	if err != nil {
		s.logger.Error("atproto uploadBlob: store in fauna media",
			"did", caller.DID, "cid", cid, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	// [2] The account attribution (property 3), which also answers how the nest
	// spells those bytes as a Fauna content CID.
	faunaCID, err := s.blobs.RecordBlobMapping(ctx, caller.ActorID, cid, mediaRef)
	if err != nil {
		s.logger.Error("atproto uploadBlob: record blob mapping",
			"did", caller.DID, "cid", cid, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	// [3] Servable by com.atproto.sync.getBlob BEFORE the answer (property 2):
	// the caller will reference this ref in a record moments from now.
	if err := s.blobs.ServeBlobBytes(ctx, caller.DID, cid, faunaCID, mime, data); err != nil {
		s.logger.Error("atproto uploadBlob: serve blob bytes",
			"did", caller.DID, "cid", cid, "err", err)
		xrpc.WriteError(w, xrpc.InternalError())
		return
	}

	xrpc.WriteJSON(w, uploadBlobResponse{Blob: blobRef{
		Type:     "blob",
		Ref:      blobRefLink{Link: cid},
		MimeType: mime,
		Size:     int64(len(data)),
	}})
}

// readBoundedBlobBody reads at most the ceiling, plus one byte to recognise an
// oversize body without buffering it. Reading the extra byte rather than trusting
// Content-Length is what makes the bound hold for a caller that mis-reports its
// length or sends none.
func readBoundedBlobBody(body io.Reader) ([]byte, error) {
	data, err := io.ReadAll(io.LimitReader(body, atprotorepo.MaxUploadBlobBytes+1))
	if err != nil {
		return nil, err
	}
	if len(data) > atprotorepo.MaxUploadBlobBytes {
		return nil, errBlobTooLarge
	}
	return data, nil
}
