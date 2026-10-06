package atprotorepo

import (
	"bytes"
	"context"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// The projection's video half: turning a Fauna video — an HLS manifest plus
// per-resolution MPEG-TS segments — into the single mp4 blob
// app.bsky.embed.video takes.
//
// The split is § Where logic lives', same as the image path. Shared Rust says
// which segments make up which rendition, in playback order, and how big each
// rendition claims to be; Go picks one, does the I/O, and hands a ResolvedVideo
// back. What differs from an image is that the published bytes are *assembled*
// rather than republished — so they have no Fauna CID, which is why dedup keys
// on the video's manifest hash instead (see [VideoItem.ManifestCID]).
//
// The assembly is lossless. The segments of one rendition come from a single
// ffmpeg HLS run (bins/fauna-nest/src/video/transcode.rs), so they are h264 +
// aac in MPEG-TS with continuous timestamps: concatenating them yields a valid
// stream, and remuxing that stream to mp4 copies the encoded frames through
// untouched. Nothing is re-encoded, so nothing is degraded and the cost is
// roughly a disk copy.

// MaxVideoBlobBytes is the per-video ceiling the projection will publish, and
// the number the rendition choice is made against.
//
// A hard-coded constant, never configuration (§ Product invariants) — nobody
// would ever want to choose it. It is video's own rather than [MaxBlobBytes]
// because the two answer different questions: an image ceiling that admitted a
// video would be an absurd image ceiling, and a video that fit in an image's
// budget does not exist. The lexicon itself allows up to 100 MB; this sits
// below that at the size bsky.app enforced in practice, which keeps one post
// from ballooning the bridge's sqlite while still admitting an ordinary phone
// video at a good resolution.
const MaxVideoBlobBytes = 50 * 1024 * 1024

// videoMIME is the only type app.bsky.embed.video accepts, and what the remux
// produces.
const videoMIME = "video/mp4"

// remuxTimeout bounds the ffmpeg subprocess. Stream-copy remuxing runs at
// roughly disk speed, so a ceiling-sized video finishes in seconds; this is the
// "something is wrong" bound, not a budget the normal path approaches.
const remuxTimeout = 2 * time.Minute

// VideoRendition is one resolution's worth of a Fauna video, as the shared-Rust
// extraction described it. Mirrors the FFI AtprotoVideoRendition.
type VideoRendition struct {
	// Height in pixels — 360, 720, 1080, 2160.
	Height uint32
	// SegmentCIDs are the FAUNA content CIDs of this rendition's segments,
	// base32, in PLAYBACK ORDER — each a path segment of
	// GET /api/v1/blob/{cid_b32}. The order is load-bearing: it is what makes
	// the concatenation a coherent stream rather than shuffled video.
	SegmentCIDs []string
	// DeclaredBytes is the sum of the segments' declared sizes — weighed
	// against the ceiling BEFORE any fetch. An upper bound on the assembled
	// mp4, never an underestimate, because the remux strips the 4-byte header
	// MPEG-TS carries every 188 bytes. So choosing on it can reject a rendition
	// that would just fit, but can never accept one that would not.
	DeclaredBytes int64
}

// VideoItem is the publishable video the shared-Rust extraction found, before
// any bytes have been fetched. It mirrors the FFI AtprotoVideo.
type VideoItem struct {
	// ManifestCID is the video's HLS manifest CID — the stable per-video
	// identity this path dedups on.
	//
	// It is deliberately NOT the identity of the bytes that get published:
	// those are assembled here and exist nowhere in Fauna. Without a per-video
	// key, answering "have I already published this video?" would cost
	// re-fetching and re-assembling every segment — the same argument that made
	// the blob row remember its Fauna CID for images.
	ManifestCID string
	// Renditions, HIGHEST RESOLUTION FIRST, so taking the first that fits the
	// ceiling yields the best copy this PDS can serve.
	Renditions []VideoRendition
	// The video's display aspect ratio, from the post — never measured from a
	// chosen rendition, so which rendition fits cannot change the layout.
	AspectWidth  uint32
	AspectHeight uint32
}

// ResolvedVideo is a video that has been assembled, re-hashed and stored — safe
// to reference from a record. It mirrors the FFI AtprotoResolvedVideo.
type ResolvedVideo struct {
	// BlobCID is the ATPROTO blob CID (CIDv1/raw/sha2-256) of the mp4.
	BlobCID      string
	MIME         string
	SizeBytes    int64
	AspectWidth  uint32
	AspectHeight uint32
}

// ResolveVideo assembles, stores and returns did's publishable video, or nil
// when there is nothing publishable — in which case the caller must SKIP the
// post rather than commit it, because a Fauna video post carries no text of its
// own and the record would otherwise be blank (§ Projection & backfill).
//
// The blob is stored BEFORE this returns, so by the time the caller commits the
// record referencing it, com.atproto.sync.getBlob can already serve it — the
// same ordering rule the image path obeys, for the same reason.
//
// Drop causes, each logged:
//
//   - every rendition exceeds [MaxVideoBlobBytes];
//   - the nest does not hold some segment's bytes;
//   - a fetch failed transiently — dropped for THIS pass and, because the post
//     is skipped rather than mapped, retried the next time the post is offered;
//   - the remux failed or produced something over the ceiling after all.
//
// A video already published for this repo is neither re-fetched nor
// re-assembled, so a re-projection is free.
func ResolveVideo(
	ctx context.Context,
	store *Store,
	source BlobSource,
	did string,
	item *VideoItem,
	logger *slog.Logger,
) (*ResolvedVideo, error) {
	if item == nil || source == nil || len(item.Renditions) == 0 {
		return nil, nil
	}
	if logger == nil {
		logger = slog.Default()
	}

	// Already published for this repo? Reuse it — no fetch, no assembly.
	if prior, ok, err := store.BlobByFaunaCID(ctx, did, item.ManifestCID); err != nil {
		return nil, err
	} else if ok {
		return &ResolvedVideo{
			BlobCID:      prior.CID,
			MIME:         prior.MIME,
			SizeBytes:    prior.Size,
			AspectWidth:  item.AspectWidth,
			AspectHeight: item.AspectHeight,
		}, nil
	}

	rendition := pickRendition(item.Renditions)
	if rendition == nil {
		logger.Info("atproto projection: every video rendition exceeds the publish ceiling; skipping the post",
			"did", did, "manifest_cid", item.ManifestCID,
			"smallest", smallestDeclared(item.Renditions), "ceiling", MaxVideoBlobBytes)
		return nil, nil
	}

	ts, ok, err := fetchSegments(ctx, source, did, item, rendition, logger)
	if err != nil || !ok {
		return nil, err
	}

	mp4, err := remuxToMP4(ctx, ts)
	if err != nil {
		logger.Warn("atproto projection: video remux failed; skipping the post",
			"did", did, "manifest_cid", item.ManifestCID, "height", rendition.Height, "err", err)
		return nil, nil
	}
	// The declared total was an upper bound, so this should always hold — but
	// it is the actual bytes that get stored and served, so it is the actual
	// bytes that must be checked.
	if len(mp4) > MaxVideoBlobBytes {
		logger.Info("atproto projection: assembled video exceeds the publish ceiling; skipping the post",
			"did", did, "manifest_cid", item.ManifestCID, "size", len(mp4), "ceiling", MaxVideoBlobBytes)
		return nil, nil
	}

	blobCID, err := BlobCIDForBytes(mp4)
	if err != nil {
		return nil, fmt.Errorf("blob cid for video %s: %w", item.ManifestCID, err)
	}
	blobCIDStr := blobCID.String()
	if err := store.PutBlob(ctx, did, blobCIDStr, item.ManifestCID, videoMIME, mp4); err != nil {
		return nil, err
	}
	logger.Info("atproto projection: published video",
		"did", did, "manifest_cid", item.ManifestCID, "height", rendition.Height,
		"segments", len(rendition.SegmentCIDs), "size", len(mp4))

	return &ResolvedVideo{
		BlobCID:      blobCIDStr,
		MIME:         videoMIME,
		SizeBytes:    int64(len(mp4)),
		AspectWidth:  item.AspectWidth,
		AspectHeight: item.AspectHeight,
	}, nil
}

// pickRendition returns the best rendition that fits the ceiling, or nil when
// none does.
//
// "Best" is highest resolution, and the descriptor already arrives in that
// order — but this scans for the first that fits rather than trusting the
// order, so a descriptor that ever stopped being sorted would degrade to a
// worse-quality pick instead of publishing something over the ceiling.
func pickRendition(renditions []VideoRendition) *VideoRendition {
	var best *VideoRendition
	for i := range renditions {
		r := &renditions[i]
		if len(r.SegmentCIDs) == 0 || r.DeclaredBytes > MaxVideoBlobBytes {
			continue
		}
		if best == nil || r.Height > best.Height {
			best = r
		}
	}
	return best
}

func smallestDeclared(renditions []VideoRendition) int64 {
	smallest := int64(-1)
	for _, r := range renditions {
		if smallest < 0 || r.DeclaredBytes < smallest {
			smallest = r.DeclaredBytes
		}
	}
	return smallest
}

// fetchSegments pulls one rendition's segments in playback order and
// concatenates them. ok=false means the video is unpublishable this pass and
// the caller should skip the post; err is reserved for real failures.
//
// A missing or unfetchable segment fails the WHOLE video rather than being
// skipped over: a gap in the middle of a stream is not a shorter video, it is a
// corrupt one, and publishing it would misrepresent what the user posted. This
// is the one place the image path's "drop it and carry on" reading does not
// transfer — there, each dropped item is a whole attachment the viewer simply
// does not see.
func fetchSegments(
	ctx context.Context,
	source BlobSource,
	did string,
	item *VideoItem,
	rendition *VideoRendition,
	logger *slog.Logger,
) ([]byte, bool, error) {
	var buf bytes.Buffer
	buf.Grow(int(rendition.DeclaredBytes))
	for _, cid := range rendition.SegmentCIDs {
		data, found, err := source.FetchBlob(ctx, cid)
		if err != nil {
			logger.Warn("atproto projection: video segment fetch failed; skipping the post this pass",
				"did", did, "manifest_cid", item.ManifestCID, "segment_cid", cid, "err", err)
			return nil, false, nil
		}
		if !found {
			logger.Info("atproto projection: nest does not hold a video segment; skipping the post",
				"did", did, "manifest_cid", item.ManifestCID, "segment_cid", cid)
			return nil, false, nil
		}
		buf.Write(data)
		// Guard the accumulation itself: declared sizes are advisory, so a
		// descriptor that under-reported must not let a repo read unbounded
		// bytes into memory.
		if buf.Len() > MaxVideoBlobBytes {
			logger.Info("atproto projection: video segments exceed the publish ceiling mid-fetch; skipping the post",
				"did", did, "manifest_cid", item.ManifestCID,
				"declared", rendition.DeclaredBytes, "ceiling", MaxVideoBlobBytes)
			return nil, false, nil
		}
	}
	if buf.Len() == 0 {
		return nil, false, nil
	}
	return buf.Bytes(), true, nil
}

// remuxToMP4 rewraps a concatenated MPEG-TS stream as mp4 without re-encoding.
//
// `-c copy` is what makes this lossless and cheap: the h264 and aac streams are
// already what mp4 wants, so only the container changes.
//
// `-bsf:a aac_adtstoasc` converts AAC's ADTS framing (how MPEG-TS carries it) to
// the ASC form mp4 requires. Stated explicitly rather than relied on: current
// ffmpeg inserts this filter itself for TS→mp4 stream copy — verified, by
// removing the flag and watching the assembly test stay green — so it is
// belt-and-braces against a future version that stops inferring it, NOT a flag
// the tests can distinguish. Do not read the assembly test as pinning it.
//
// `-movflags +faststart` moves the moov atom to the front, so a consumer can
// begin playback without fetching the whole blob first — the difference between
// a video that streams and one that must be downloaded.
//
// ffmpeg ships in the runtime image already (it is there for the nest's own
// transcode path), so this adds no packaging dependency.
func remuxToMP4(ctx context.Context, ts []byte) ([]byte, error) {
	dir, err := os.MkdirTemp("", "atproto-video-")
	if err != nil {
		return nil, fmt.Errorf("temp dir: %w", err)
	}
	defer os.RemoveAll(dir)

	inPath := filepath.Join(dir, "in.ts")
	outPath := filepath.Join(dir, "out.mp4")
	if err := os.WriteFile(inPath, ts, 0o600); err != nil {
		return nil, fmt.Errorf("write remux input: %w", err)
	}

	ctx, cancel := context.WithTimeout(ctx, remuxTimeout)
	defer cancel()

	var stderr bytes.Buffer
	cmd := exec.CommandContext(ctx, "ffmpeg",
		"-nostdin",
		"-loglevel", "error",
		"-f", "mpegts",
		"-i", inPath,
		"-c", "copy",
		"-bsf:a", "aac_adtstoasc",
		"-movflags", "+faststart",
		"-y", outPath,
	)
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("ffmpeg remux: %w (%s)", err, truncateForLog(stderr.String()))
	}

	mp4, err := os.ReadFile(outPath)
	if err != nil {
		return nil, fmt.Errorf("read remux output: %w", err)
	}
	if len(mp4) == 0 {
		return nil, fmt.Errorf("ffmpeg remux produced no output (%s)", truncateForLog(stderr.String()))
	}
	return mp4, nil
}

func truncateForLog(s string) string {
	const max = 500
	if len(s) <= max {
		return s
	}
	return s[:max] + "…"
}
