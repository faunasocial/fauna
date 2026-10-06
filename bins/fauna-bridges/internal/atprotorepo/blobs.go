package atprotorepo

import (
	"context"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
)

// The projection's media half: resolving a Fauna post's image attachments into
// blobs this PDS actually serves, before the commit that references them.
//
// The split follows atproto-pds-bridge.md § Where logic lives. Shared Rust says
// WHICH bytes matter and what they are (MediaItem, from the FFI extraction);
// Go does the I/O — fetch, re-hash, store — and hands ResolvedImages back for
// the record. The bridge still never decodes a Fauna post.

// MaxBlobBytes is the per-blob ceiling the projection will publish. A larger
// attachment is DROPPED from the record rather than published, and rather than
// blocking the post.
//
// A hard-coded constant, never configuration (§ Product invariants) — nobody
// would ever want to choose it, and the value only has to be large enough that
// an ordinary photo passes and small enough that one post cannot make the
// bridge's sqlite grow without bound. It bounds sqlite growth and fetch time
// only: blob bytes never ride the firehose (a #commit carries the record CAR,
// not the blob), so this is not a wire-size limit.
const MaxBlobBytes = 10 * 1024 * 1024

// MaxUploadBlobBytes is the ceiling on an INBOUND blob
// (com.atproto.repo.uploadBlob, F2.4 slice 1). Also a hard-coded constant, and
// deliberately below [MaxBlobBytes] rather than equal to it.
//
// The reason is a real coupling, not caution. Those bytes travel to the nest as
// one part of a multipart body on POST /api/v1/blob, and the nest caps the WHOLE
// body — blob part, dag-cbor sidecar part, and MIME framing together — at 10 MiB
// (bins/fauna-nest/src/blob_routes.rs, BLOB_BODY_LIMIT). A blob at exactly
// MaxBlobBytes therefore cannot fit beside its own sidecar, and would 413 after
// the caller had already streamed every byte. The 64 KiB reserved here is orders
// of magnitude more than the sidecar plus framing needs, which keeps the
// arithmetic obvious rather than tight.
//
// This side is the good error message, not the guarantee: the nest's 413 remains
// the authority, the same two-site shape the CAS and first-emit gates use.
const MaxUploadBlobBytes = MaxBlobBytes - 64*1024

// MediaItem is one image attachment the shared-Rust extraction found, before
// any bytes have been fetched. It mirrors the FFI AtprotoMediaItem; the
// projection loop converts.
type MediaItem struct {
	// BlobCID is the FAUNA content CID, base32 — the path segment of
	// GET /api/v1/blob/{cid_b32} on the nest.
	BlobCID string
	// MIME is the type the Fauna record DECLARED, or "" when it declared none.
	//
	// A post attachment always declares one. A PROFILE PICTURE never does:
	// Profile.avatar/banner are bare ContentHash fields, so the profile records
	// which bytes and nothing else — and the nest's CID-addressed byte route
	// answers application/octet-stream, so the wire carries no type either.
	// Empty therefore means "undeclared, determine it from the bytes"
	// ([SniffImageMIME]), never "unknown, publish it anyway": the blob ref this
	// feeds requires a mimeType, and asserting a wrong one is a lie about bytes
	// every consumer will try to render.
	MIME string
	// SizeBytes is what the post DECLARED. Advisory: what the fetch actually
	// returned is what gets stored and published.
	SizeBytes int64
	Width     *uint32
	Height    *uint32
	Alt       string
}

// ResolvedImage is one attachment that has been fetched, re-hashed and stored —
// safe to reference from a record. It mirrors the FFI AtprotoResolvedImage.
type ResolvedImage struct {
	// BlobCID is the ATPROTO blob CID (CIDv1/raw/sha2-256), NOT the Fauna one.
	BlobCID   string
	MIME      string
	SizeBytes int64
	Width     *uint32
	Height    *uint32
	Alt       string
}

// BlobSource fetches a Fauna blob's bytes by its Fauna content CID (base32).
// Implemented in the bridge main over the nest's CID-addressed byte route, and
// faked in tests. found=false means the nest does not hold those bytes.
type BlobSource interface {
	FetchBlob(ctx context.Context, faunaCID string) (data []byte, found bool, err error)
}

// ResolveMedia turns a post's media descriptors into publishable images,
// storing each blob under did BEFORE returning — so by the time the caller
// commits the record that references them, com.atproto.sync.getBlob can already
// serve every ref. The inverse order would announce a dangling ref, which
// atproto-pds-bridge.md § Projection & backfill forbids ("never a ref to a
// record no repo serves").
//
// An attachment that cannot be published is DROPPED and the rest proceed — the
// post publishes with fewer images, never not at all. That is the same reading
// § Projection & backfill already ratified for an unbridged reply parent:
// withholding content the user consented to publish is the worse failure, and
// unlike a missing ref it cascades. Three drop causes, each logged:
//
//   - the nest does not hold the bytes (a bridge-imported item whose media
//     lives at a remote_url was never uploaded here);
//   - the bytes exceed [MaxBlobBytes];
//   - the fetch failed transiently — dropped for THIS pass, and because the
//     post then lands in post_map, not retried. Recorded honestly rather than
//     hidden: making it retryable needs a per-attachment projection state this
//     slice does not build.
//   - the bytes are not an image the network can render, for an item that
//     declared no MIME type (a profile picture — see [MediaItem.MIME]).
//
// A blob already stored for this repo is neither re-fetched nor re-hashed, so a
// re-projection and a second post reusing the same image are both free.
func ResolveMedia(
	ctx context.Context,
	store *Store,
	source BlobSource,
	did string,
	items []MediaItem,
	logger *slog.Logger,
) ([]ResolvedImage, error) {
	if len(items) == 0 || source == nil {
		return nil, nil
	}
	if logger == nil {
		logger = slog.Default()
	}
	out := make([]ResolvedImage, 0, len(items))
	for _, item := range items {
		// Already published for this repo? Reuse it — no fetch, no re-hash.
		// This is why the row remembers its Fauna CID: BLAKE3 → sha256 is only
		// computable by holding the bytes, so without the provenance column
		// "have I published this already?" would itself cost the fetch.
		if prior, ok, err := store.BlobByFaunaCID(ctx, did, item.BlobCID); err != nil {
			return nil, err
		} else if ok {
			out = append(out, ResolvedImage{
				BlobCID:   prior.CID,
				MIME:      prior.MIME,
				SizeBytes: prior.Size,
				Width:     item.Width,
				Height:    item.Height,
				Alt:       item.Alt,
			})
			continue
		}
		data, found, err := source.FetchBlob(ctx, item.BlobCID)
		if err != nil {
			logger.Warn("atproto projection: blob fetch failed; publishing the post without this image",
				"did", did, "fauna_cid", item.BlobCID, "err", err)
			continue
		}
		if !found {
			logger.Info("atproto projection: nest does not hold these blob bytes; dropping the image",
				"did", did, "fauna_cid", item.BlobCID)
			continue
		}
		if len(data) > MaxBlobBytes {
			logger.Info("atproto projection: blob exceeds the publish ceiling; dropping the image",
				"did", did, "fauna_cid", item.BlobCID, "size", len(data), "ceiling", MaxBlobBytes)
			continue
		}
		mime := item.MIME
		if mime == "" {
			// Undeclared (a profile picture): the bytes are the only source of
			// truth for what this is. A non-image sniff is dropped like any
			// other unpublishable attachment.
			sniffed, ok := SniffImageMIME(data)
			if !ok {
				logger.Info("atproto projection: blob bytes are not a renderable image; dropping it",
					"did", did, "fauna_cid", item.BlobCID)
				continue
			}
			mime = sniffed
		}
		blobCID, err := BlobCIDForBytes(data)
		if err != nil {
			return nil, fmt.Errorf("blob cid for %s: %w", item.BlobCID, err)
		}
		blobCIDStr := blobCID.String()
		if err := store.PutBlob(ctx, did, blobCIDStr, item.BlobCID, mime, data); err != nil {
			return nil, err
		}
		out = append(out, ResolvedImage{
			BlobCID:   blobCIDStr,
			MIME:      mime,
			SizeBytes: int64(len(data)),
			Width:     item.Width,
			Height:    item.Height,
			Alt:       item.Alt,
		})
	}
	return out, nil
}

// SniffImageMIME reports the image media type of raw bytes, for an item whose
// Fauna record declared none (a profile picture — see [MediaItem.MIME]).
//
// Magic-byte detection via net/http's sniffer, then an image/* check: the type
// goes into a blob ref the whole network reads, so bytes that are not an image
// must be dropped rather than published under an invented type. ok=false is
// the caller's signal to drop the field.
//
// Deliberately NOT applied to an OUTBOUND item that DID declare a type. A post's
// attachment carries the uploader's own declaration and the projection
// republishes it as-is; second-guessing that here would make the projected
// record disagree with the Fauna post it derives from.
//
// **Both directions share this one sniffer, and they differ only in what they do
// with ok=false** (F2.4 slice 1). Outbound, the caller DROPS the attachment and
// publishes the post without it — withholding content the user already consented
// to publish is the worse failure, and the post survives. Inbound
// (com.atproto.repo.uploadBlob) the caller REFUSES the upload, because nothing
// has been published yet: the app learns immediately, nothing is half-done, and
// it can retry with a format we can name — whereas accepting bytes whose type we
// had to invent means the nest serves them under a guessed Content-Type forever.
func SniffImageMIME(data []byte) (string, bool) {
	// DetectContentType reads at most the first 512 bytes and always returns a
	// type ("application/octet-stream" when it recognises nothing).
	mime := http.DetectContentType(data)
	// It can append parameters (e.g. "text/plain; charset=utf-8"); the blob
	// ref wants the bare type.
	if i := strings.IndexByte(mime, ';'); i >= 0 {
		mime = strings.TrimSpace(mime[:i])
	}
	if !strings.HasPrefix(mime, "image/") {
		return "", false
	}
	return mime, true
}
