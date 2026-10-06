package atprotorepo

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"testing"
)

// The video path is tested against REAL bytes, not canned ones: the whole claim
// this slice makes is that concatenating one rendition's MPEG-TS segments and
// remuxing them yields a playable mp4, and only real segments through real
// ffmpeg can falsify that. Canned bytes would prove the plumbing and nothing
// about the mapping.

// makeTSSegments produces genuine HLS segments the way the nest's own transcode
// path does (bins/fauna-nest/src/video/transcode.rs): h264 + aac in MPEG-TS,
// segmented on a fixed interval. Returns the segment bytes in playback order.
func makeTSSegments(t *testing.T, seconds int, height int) [][]byte {
	t.Helper()
	requireFFmpeg(t)
	dir := t.TempDir()

	// A synthetic clip with both a video and an audio track — the audio matters,
	// because AAC's TS framing is exactly what the remux has to convert.
	cmd := exec.Command("ffmpeg",
		"-nostdin", "-loglevel", "error",
		"-f", "lavfi", "-i", fmt.Sprintf("testsrc=size=%dx%d:rate=15:duration=%d", height*16/9, height, seconds),
		"-f", "lavfi", "-i", fmt.Sprintf("sine=frequency=440:duration=%d", seconds),
		"-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
		// A keyframe every second, so the HLS muxer can actually cut at the
		// 1 s boundary — the default GOP is far longer than the clip, which
		// would yield a single segment and test nothing about concatenation.
		"-g", "15", "-keyint_min", "15", "-sc_threshold", "0",
		"-c:a", "aac", "-b:a", "64k",
		"-f", "hls", "-hls_time", "1", "-hls_list_size", "0",
		"-hls_segment_filename", filepath.Join(dir, "seg%03d.ts"),
		filepath.Join(dir, "stream.m3u8"),
	)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("ffmpeg could not build test segments: %v\n%s", err, out)
	}

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".ts") {
			names = append(names, e.Name())
		}
	}
	sort.Strings(names) // seg000, seg001, … = playback order
	if len(names) < 2 {
		t.Fatalf("expected several segments, got %d — the assembly claim is only "+
			"interesting across a segment boundary", len(names))
	}
	var segs [][]byte
	for _, n := range names {
		b, err := os.ReadFile(filepath.Join(dir, n))
		if err != nil {
			t.Fatal(err)
		}
		segs = append(segs, b)
	}
	return segs
}

func requireFFmpeg(t *testing.T) {
	t.Helper()
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		// Deliberately not a skip: ffmpeg ships in the runtime image and the
		// projection cannot publish video without it, so its absence is a
		// broken environment, not a reason to report success.
		t.Fatalf("ffmpeg is required by the video projection and by this test: %v", err)
	}
}

// probeStreams reports the codec of each stream ffprobe finds, which is what
// "playable" reduces to here: a container a decoder can open, carrying the
// tracks we put in.
func probeStreams(t *testing.T, mp4 []byte) (format string, codecs []string, durationOK bool) {
	t.Helper()
	if _, err := exec.LookPath("ffprobe"); err != nil {
		t.Fatalf("ffprobe is required by this test: %v", err)
	}
	path := filepath.Join(t.TempDir(), "probe.mp4")
	if err := os.WriteFile(path, mp4, 0o600); err != nil {
		t.Fatal(err)
	}
	out, err := exec.Command("ffprobe",
		"-loglevel", "error", "-print_format", "json",
		"-show_format", "-show_streams", path,
	).Output()
	if err != nil {
		t.Fatalf("ffprobe could not open the assembled blob — it is not a playable mp4: %v", err)
	}
	var probed struct {
		Format struct {
			FormatName string `json:"format_name"`
			Duration   string `json:"duration"`
		} `json:"format"`
		Streams []struct {
			CodecName string `json:"codec_name"`
		} `json:"streams"`
	}
	if err := json.Unmarshal(out, &probed); err != nil {
		t.Fatal(err)
	}
	for _, s := range probed.Streams {
		codecs = append(codecs, s.CodecName)
	}
	sort.Strings(codecs)
	return probed.Format.FormatName, codecs, probed.Format.Duration != "" && probed.Format.Duration != "0"
}

func videoItem(manifestCID string, renditions ...VideoRendition) *VideoItem {
	return &VideoItem{
		ManifestCID:  manifestCID,
		Renditions:   renditions,
		AspectWidth:  16,
		AspectHeight: 9,
	}
}

// loadSegments puts real segments on the fake byte route and returns the
// rendition descriptor naming them, in playback order.
func loadSegments(src *fakeBlobSource, prefix string, height uint32, segs [][]byte) VideoRendition {
	r := VideoRendition{Height: height}
	for i, s := range segs {
		cid := fmt.Sprintf("%s-%d", prefix, i)
		src.blobs[cid] = s
		r.SegmentCIDs = append(r.SegmentCIDs, cid)
		r.DeclaredBytes += int64(len(s))
	}
	return r
}

// The load-bearing claim: segments in, one playable mp4 out, stored before it
// is referenced. Proven by handing the result to ffprobe rather than by
// inspecting magic bytes — a container that merely starts with `ftyp` can still
// be undecodable, and the audio bitstream conversion is exactly the part a
// magic-byte check would miss.
func TestVideoSegmentsAssembleIntoAPlayableMP4(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	segs := makeTSSegments(t, 3, 360)
	rendition := loadSegments(src, "seg720", 720, segs)
	tr.videos["video post"] = videoItem("manifest-1", rendition)
	tr.textless["video post"] = true

	items := []ProjectionItem{
		{PostID: hexID(0x21), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("video post")},
	}
	if _, err := p.ProjectItems(ctx, did, key, items); err != nil {
		t.Fatal(err)
	}

	seen := tr.seen["video post"]
	if seen.video == nil {
		t.Fatal("the translator was handed no video — nothing was assembled")
	}
	if seen.video.MIME != "video/mp4" {
		t.Fatalf("mime %q, want video/mp4 — the only type the lexicon accepts", seen.video.MIME)
	}
	if seen.video.AspectWidth != 16 || seen.video.AspectHeight != 9 {
		t.Fatalf("aspect ratio %dx%d did not come from the post",
			seen.video.AspectWidth, seen.video.AspectHeight)
	}

	// Servable BEFORE the record referencing it was committed — the ordering
	// § Projection & backfill requires.
	blob, ok, err := st.GetBlob(ctx, did, seen.video.BlobCID)
	if err != nil || !ok {
		t.Fatalf("assembled video not servable after projection: ok=%v err=%v", ok, err)
	}
	if int64(len(blob.Bytes)) != seen.video.SizeBytes {
		t.Fatalf("record says %d bytes, store holds %d", seen.video.SizeBytes, len(blob.Bytes))
	}

	format, codecs, durationOK := probeStreams(t, blob.Bytes)
	if !strings.Contains(format, "mp4") {
		t.Fatalf("assembled blob is %q, not an mp4", format)
	}
	if len(codecs) != 2 || codecs[0] != "aac" || codecs[1] != "h264" {
		t.Fatalf("assembled blob carries %v, want both tracks copied through [aac h264]", codecs)
	}
	if !durationOK {
		t.Fatal("assembled blob reports no duration — the segments did not concatenate into a timeline")
	}
	// Lossless: the remux copies frames, so the payload cannot have grown.
	var totalTS int64
	for _, s := range segs {
		totalTS += int64(len(s))
	}
	if seen.video.SizeBytes > totalTS {
		t.Fatalf("assembled mp4 (%d) is larger than its MPEG-TS input (%d) — that is a re-encode, not a remux",
			seen.video.SizeBytes, totalTS)
	}
}

// The rendition choice publishes the best copy the ceiling admits — never the
// cheapest, and never one over the ceiling.
func TestVideoPicksTheBestRenditionUnderTheCeiling(t *testing.T) {
	ctx := context.Background()
	_, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	segs := makeTSSegments(t, 2, 360)

	small := loadSegments(src, "seg360", 360, segs)
	mid := loadSegments(src, "seg720", 720, segs)
	huge := loadSegments(src, "seg1080", 1080, segs)
	huge.DeclaredBytes = MaxVideoBlobBytes + 1 // over the ceiling

	tr.videos["v"] = videoItem("manifest-1", huge, mid, small)
	tr.textless["v"] = true
	items := []ProjectionItem{
		{PostID: hexID(0x22), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("v")},
	}
	if _, err := p.ProjectItems(ctx, did, key, items); err != nil {
		t.Fatal(err)
	}

	// The 1080p rendition must not have been fetched at all — the ceiling is
	// weighed against declared sizes precisely so an over-ceiling rendition
	// costs no bytes.
	if n := src.fetched[huge.SegmentCIDs[0]]; n != 0 {
		t.Fatalf("over-ceiling rendition was fetched %d times; the declared total is what rejects it", n)
	}
	if n := src.fetched[mid.SegmentCIDs[0]]; n == 0 {
		t.Fatal("the best fitting rendition (720p) was never fetched")
	}
	if n := src.fetched[small.SegmentCIDs[0]]; n != 0 {
		t.Fatalf("the 360p rendition was fetched %d times — a fitting better one existed", n)
	}
}

// Every rendition over the ceiling leaves nothing publishable. A Fauna video
// post has no text of its own, so the record would be blank — and a blank
// record is not committed (§ Projection & backfill).
func TestVideoOverTheCeilingSkipsThePostEntirely(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr, _ := newTestProjectorWithBlobs(t)

	only := VideoRendition{
		Height:        1080,
		SegmentCIDs:   []string{"seg-a", "seg-b"},
		DeclaredBytes: MaxVideoBlobBytes + 1,
	}
	tr.videos["big video"] = videoItem("manifest-big", only)
	tr.textless["big video"] = true

	items := []ProjectionItem{
		{PostID: hexID(0x23), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("big video")},
	}
	applied, err := p.ProjectItems(ctx, did, key, items)
	if err != nil {
		t.Fatal(err)
	}
	if applied != 0 {
		t.Fatalf("applied %d commits, want 0 — an unpublishable video must skip the post", applied)
	}
	// Nothing was written: no record, and crucially no post_map row, so the
	// post is not falsely remembered as projected.
	if _, _, mapped, err := st.PostAtURI(ctx, did, hexID(0x23)); err != nil {
		t.Fatal(err)
	} else if mapped {
		t.Fatal("a skipped post must not enter post_map")
	}
}

// A missing segment fails the WHOLE video rather than being skipped over: a gap
// mid-stream is a corrupt video, not a shorter one. This is the one place the
// image path's drop-and-carry-on reading does not transfer.
func TestVideoMissingSegmentSkipsThePostRatherThanPublishingAGap(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	segs := makeTSSegments(t, 2, 360)
	rendition := loadSegments(src, "seg720", 720, segs)
	// The nest no longer holds the middle segment.
	delete(src.blobs, rendition.SegmentCIDs[1])

	tr.videos["gappy"] = videoItem("manifest-gap", rendition)
	tr.textless["gappy"] = true
	items := []ProjectionItem{
		{PostID: hexID(0x24), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("gappy")},
	}
	applied, err := p.ProjectItems(ctx, did, key, items)
	if err != nil {
		t.Fatal(err)
	}
	if applied != 0 {
		t.Fatalf("applied %d commits, want 0 — a gap must not be published as a video", applied)
	}
	if cids, _, err := st.ListBlobCIDs(ctx, did, "", 10); err != nil {
		t.Fatal(err)
	} else if len(cids) != 0 {
		t.Fatalf("a partial assembly was stored anyway: %v", cids)
	}
}

// A re-projection neither re-fetches nor re-assembles: the blob row remembers
// the video's MANIFEST cid, which is the only stable per-video identity (the
// assembled mp4 has no Fauna CID of its own).
func TestReusedVideoIsAssembledOnce(t *testing.T) {
	ctx := context.Background()
	_, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	segs := makeTSSegments(t, 2, 360)
	rendition := loadSegments(src, "seg720", 720, segs)

	tr.videos["first"] = videoItem("manifest-1", rendition)
	tr.videos["second"] = videoItem("manifest-1", rendition)
	tr.textless["first"] = true
	tr.textless["second"] = true

	items := []ProjectionItem{
		{PostID: hexID(0x25), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("first")},
		{PostID: hexID(0x26), CreatedAtMicros: 2_000_000, Kind: KindPost, Payload: []byte("second")},
	}
	if _, err := p.ProjectItems(ctx, did, key, items); err != nil {
		t.Fatal(err)
	}
	for _, cid := range rendition.SegmentCIDs {
		if n := src.fetched[cid]; n != 1 {
			t.Fatalf("segment %s fetched %d times, want 1 — the manifest cid is the dedup key", cid, n)
		}
	}
	if a, b := tr.seen["first"].video, tr.seen["second"].video; a == nil || b == nil || a.BlobCID != b.BlobCID {
		t.Fatal("the two posts must reference the same assembled blob")
	}
}

// The #commit frame announces the video blob, so a relay learns which bytes the
// record depends on — the same contract the image path has.
func TestCommitFrameAnnouncesTheVideoBlob(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	segs := makeTSSegments(t, 2, 360)
	rendition := loadSegments(src, "seg720", 720, segs)
	tr.videos["video post"] = videoItem("manifest-1", rendition)
	tr.textless["video post"] = true

	items := []ProjectionItem{
		{PostID: hexID(0x27), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("video post")},
	}
	if _, err := p.ProjectItems(ctx, did, key, items); err != nil {
		t.Fatal(err)
	}

	commit := decodeCommitFrame(t, outboxFrame(t, st, 1))
	want := tr.seen["video post"].video.BlobCID
	var got []string
	for _, b := range commit.Blobs {
		got = append(got, b.String())
	}
	if len(got) != 1 || got[0] != want {
		t.Fatalf("frame announced blobs %v, want exactly [%s]", got, want)
	}
}

// The blank-record rule is not video-specific: a media post whose every
// attachment was dropped reaches the same state, and used to commit an empty
// record the same way.
func TestMediaPostWithEveryAttachmentDroppedIsSkipped(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	// The nest holds none of these bytes.
	tr.media["all dropped"] = []MediaItem{imageItem("missing-1"), imageItem("missing-2")}
	tr.textless["all dropped"] = true
	_ = src

	items := []ProjectionItem{
		{PostID: hexID(0x28), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("all dropped")},
	}
	applied, err := p.ProjectItems(ctx, did, key, items)
	if err != nil {
		t.Fatal(err)
	}
	if applied != 0 {
		t.Fatalf("applied %d commits, want 0 — a record with neither text nor embed must not be committed", applied)
	}
	if _, _, mapped, err := st.PostAtURI(ctx, did, hexID(0x28)); err != nil {
		t.Fatal(err)
	} else if mapped {
		t.Fatal("a skipped post must not enter post_map")
	}
}

// A text post whose media all dropped still projects — the skip turns on the
// record carrying nothing, never on the attachments alone being absent.
func TestTextPostWithDroppedAttachmentsStillProjects(t *testing.T) {
	ctx := context.Background()
	_, p, key, did, tr, _ := newTestProjectorWithBlobs(t)
	tr.media["still has words"] = []MediaItem{imageItem("missing-1")}

	items := []ProjectionItem{
		{PostID: hexID(0x29), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("still has words")},
	}
	applied, err := p.ProjectItems(ctx, did, key, items)
	if err != nil {
		t.Fatal(err)
	}
	if applied != 1 {
		t.Fatalf("applied %d commits, want 1 — the post still has text to publish", applied)
	}
}

// pickRendition is scanned rather than trusted-in-order, so a descriptor that
// ever stopped arriving best-first degrades to a worse pick instead of
// publishing something over the ceiling.
func TestPickRenditionIgnoresDescriptorOrder(t *testing.T) {
	got := pickRendition([]VideoRendition{
		{Height: 360, SegmentCIDs: []string{"a"}, DeclaredBytes: 10},
		{Height: 1080, SegmentCIDs: []string{"b"}, DeclaredBytes: MaxVideoBlobBytes + 1},
		{Height: 720, SegmentCIDs: []string{"c"}, DeclaredBytes: 20},
	})
	if got == nil || got.Height != 720 {
		t.Fatalf("picked %v, want the 720p rendition", got)
	}
	if pickRendition([]VideoRendition{
		{Height: 720, SegmentCIDs: []string{"a"}, DeclaredBytes: MaxVideoBlobBytes + 1},
	}) != nil {
		t.Fatal("a rendition over the ceiling must not be picked")
	}
	if pickRendition([]VideoRendition{{Height: 720, DeclaredBytes: 1}}) != nil {
		t.Fatal("a rendition with no segments has nothing to assemble")
	}
}
