package atprotorepo

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"testing"

	comatproto "github.com/bluesky-social/indigo/api/atproto"
	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/events"
	"github.com/ipfs/go-cid"
)

// fakeBlobSource is a canned nest byte route: what it holds, what it refuses,
// and how many times each CID was actually fetched (the dedup assertion).
type fakeBlobSource struct {
	blobs   map[string][]byte
	fails   map[string]error
	fetched map[string]int
}

func newFakeBlobSource() *fakeBlobSource {
	return &fakeBlobSource{
		blobs:   map[string][]byte{},
		fails:   map[string]error{},
		fetched: map[string]int{},
	}
}

func (f *fakeBlobSource) FetchBlob(_ context.Context, faunaCID string) ([]byte, bool, error) {
	f.fetched[faunaCID]++
	if err, ok := f.fails[faunaCID]; ok {
		return nil, false, err
	}
	data, ok := f.blobs[faunaCID]
	if !ok {
		return nil, false, nil
	}
	return data, true, nil
}

func newTestProjectorWithBlobs(t *testing.T) (*Store, *Projector, Signer, string, *fakeTranslator, *fakeBlobSource) {
	t.Helper()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	tr := newFakeTranslator()
	src := newFakeBlobSource()
	did := "did:web:alice.example"
	announced(t, st, did)
	return st, NewProjector(st, f, tr, src, nil), key, did, tr, src
}

func imageItem(faunaCID string) MediaItem {
	w, h := uint32(800), uint32(600)
	return MediaItem{
		BlobCID:   faunaCID,
		MIME:      "image/png",
		SizeBytes: 3,
		Width:     &w,
		Height:    &h,
		Alt:       "a picture",
	}
}

// The CID a record's blob ref carries is the sha256 of the exact bytes served,
// so a consumer that fetches it can verify what it got. Pinned rather than
// asserted round-trip: an accidental switch to BLAKE3 (Fauna's own addressing,
// which is right there in the same code path) would still round-trip.
func TestBlobCIDIsRawSha256(t *testing.T) {
	c, err := BlobCIDForBytes([]byte("hello"))
	if err != nil {
		t.Fatal(err)
	}
	// Derived independently of this code, so the vector cannot drift with it:
	// sha256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824,
	// prefixed <0x01 v1><0x55 raw><0x12 sha2-256><0x20 len> and base32-lower'd
	// with the multibase 'b'.
	const want = "bafkreibm6jg3ux5qumhcn2b3flc3tyu6dmlb4xa7u5bf44yegnrjhc4yeq"
	if c.String() != want {
		t.Fatalf("blob cid = %s, want %s", c.String(), want)
	}
	dec, err := cid.Decode(c.String())
	if err != nil {
		t.Fatalf("decode own cid: %v", err)
	}
	if dec.Prefix().Codec != cid.Raw {
		t.Fatalf("codec = %d, want raw (%d)", dec.Prefix().Codec, cid.Raw)
	}
}

func TestBlobStoreRoundTripAndDedup(t *testing.T) {
	ctx := context.Background()
	st, _ := newTestFunnel(t, nil)
	const did = "did:web:alice.example"
	announced(t, st, did)
	data := []byte("image-bytes")
	c, err := BlobCIDForBytes(data)
	if err != nil {
		t.Fatal(err)
	}
	if err := st.PutBlob(ctx, did, c.String(), "fauna-cid-1", "image/png", data); err != nil {
		t.Fatal(err)
	}
	// Re-storing the same blob is a no-op, not an error — that is what makes a
	// re-projection free.
	if err := st.PutBlob(ctx, did, c.String(), "fauna-cid-1", "image/png", data); err != nil {
		t.Fatalf("re-put must be idempotent: %v", err)
	}

	got, ok, err := st.GetBlob(ctx, did, c.String())
	if err != nil || !ok {
		t.Fatalf("GetBlob: ok=%v err=%v", ok, err)
	}
	if !bytes.Equal(got.Bytes, data) {
		t.Fatalf("bytes round-trip mismatch")
	}
	if got.MIME != "image/png" || got.Size != int64(len(data)) || got.FaunaCID != "fauna-cid-1" {
		t.Fatalf("metadata round-trip mismatch: %+v", got)
	}

	// Another repo does not serve it: blobs are per-DID, so one user's repo can
	// never be used to enumerate another's media.
	if _, ok, err := st.GetBlob(ctx, "did:web:bob.example", c.String()); err != nil || ok {
		t.Fatalf("blob leaked across repos: ok=%v err=%v", ok, err)
	}

	prior, ok, err := st.BlobByFaunaCID(ctx, did, "fauna-cid-1")
	if err != nil || !ok {
		t.Fatalf("BlobByFaunaCID: ok=%v err=%v", ok, err)
	}
	if prior.CID != c.String() {
		t.Fatalf("provenance lookup returned %s, want %s", prior.CID, c.String())
	}
}

// The inbound-picture resolution's bridge-side index read: an ATProto blob CID
// an external app echoed back at us resolves to the Fauna content it is, and a
// CID this repo never published resolves to nothing — which is the nest's
// signal to fall back to its own upload ledger and, failing that, refuse.
//
// Repo-scoping is asserted too, and it is not decoration: blob refs are
// repo-scoped, so vouching across DIDs would let one account's echo attach
// another account's picture to its own profile.
func TestFaunaCIDForBlobResolvesOnlyThisReposPublishedBlobs(t *testing.T) {
	ctx := context.Background()
	st, _ := newTestFunnel(t, nil)
	const did = "did:web:alice.example"
	announced(t, st, did)
	data := []byte("avatar-bytes")
	c, err := BlobCIDForBytes(data)
	if err != nil {
		t.Fatal(err)
	}
	if err := st.PutBlob(ctx, did, c.String(), "fauna-avatar-cid", "image/png", data); err != nil {
		t.Fatal(err)
	}

	faunaCID, ok, err := st.FaunaCIDForBlob(ctx, did, c.String())
	if err != nil || !ok {
		t.Fatalf("FaunaCIDForBlob: ok=%v err=%v", ok, err)
	}
	if faunaCID != "fauna-avatar-cid" {
		t.Fatalf("resolved to %q, want %q", faunaCID, "fauna-avatar-cid")
	}

	// A blob this repo never published vouches for nothing.
	if _, ok, err := st.FaunaCIDForBlob(ctx, did, "bafkreinevepublished"); err != nil || ok {
		t.Fatalf("an unpublished blob resolved: ok=%v err=%v", ok, err)
	}

	// And another repo's echo of the same CID resolves to nothing here.
	if _, ok, err := st.FaunaCIDForBlob(ctx, "did:web:bob.example", c.String()); err != nil || ok {
		t.Fatalf("resolution leaked across repos: ok=%v err=%v", ok, err)
	}
}

func TestListBlobCIDsPaginates(t *testing.T) {
	ctx := context.Background()
	st, _ := newTestFunnel(t, nil)
	const did = "did:web:alice.example"
	announced(t, st, did)
	want := map[string]bool{}
	for i := 0; i < 5; i++ {
		data := []byte(fmt.Sprintf("blob-%d", i))
		c, err := BlobCIDForBytes(data)
		if err != nil {
			t.Fatal(err)
		}
		if err := st.PutBlob(ctx, did, c.String(), fmt.Sprintf("f-%d", i), "image/png", data); err != nil {
			t.Fatal(err)
		}
		want[c.String()] = true
	}

	seen := map[string]bool{}
	cursor := ""
	pages := 0
	for {
		cids, next, err := st.ListBlobCIDs(ctx, did, cursor, 2)
		if err != nil {
			t.Fatal(err)
		}
		pages++
		if len(cids) > 2 {
			t.Fatalf("page served %d cids, ceiling was 2", len(cids))
		}
		for _, c := range cids {
			if seen[c] {
				t.Fatalf("cursor walk repeated %s", c)
			}
			seen[c] = true
		}
		if next == "" {
			break
		}
		cursor = next
		if pages > 10 {
			t.Fatal("cursor walk did not terminate")
		}
	}
	if len(seen) != len(want) {
		t.Fatalf("walked %d blobs, stored %d", len(seen), len(want))
	}
	for c := range want {
		if !seen[c] {
			t.Fatalf("blob %s never appeared in the walk", c)
		}
	}
}

// A media post projects with a real images embed, and the blob is stored BEFORE
// the commit that references it — the ordering that keeps a firehose consumer
// from ever seeing a ref to bytes this PDS cannot serve.
func TestProjectPostStoresBlobThenReferencesIt(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	data := []byte("png-bytes")
	src.blobs["fauna-1"] = data
	tr.media["media post"] = []MediaItem{imageItem("fauna-1")}

	items := []ProjectionItem{
		{PostID: hexID(0x11), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("media post")},
	}
	if _, err := p.ProjectItems(ctx, did, key, items); err != nil {
		t.Fatal(err)
	}

	blobCID, err := BlobCIDForBytes(data)
	if err != nil {
		t.Fatal(err)
	}
	// Servable.
	got, ok, err := st.GetBlob(ctx, did, blobCID.String())
	if err != nil || !ok {
		t.Fatalf("blob not servable after projection: ok=%v err=%v", ok, err)
	}
	if !bytes.Equal(got.Bytes, data) {
		t.Fatal("stored bytes differ from the fetched ones")
	}
	// Referenced, with the ATProto CID — never the Fauna one.
	seen := tr.seen["media post"]
	if len(seen.images) != 1 {
		t.Fatalf("translator got %d images, want 1", len(seen.images))
	}
	if seen.images[0].BlobCID != blobCID.String() {
		t.Fatalf("record referenced %s, want the atproto cid %s", seen.images[0].BlobCID, blobCID.String())
	}
	if seen.images[0].SizeBytes != int64(len(data)) {
		t.Fatalf("size %d, want the FETCHED length %d", seen.images[0].SizeBytes, len(data))
	}
	if seen.images[0].Alt != "a picture" {
		t.Fatalf("alt did not survive: %q", seen.images[0].Alt)
	}
}

// A second post reusing the same attachment costs no fetch and no re-hash: the
// blob row remembers which Fauna CID it came from, which is the only way to
// answer "already published?" without holding the bytes.
func TestReusedAttachmentIsFetchedOnce(t *testing.T) {
	ctx := context.Background()
	_, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	src.blobs["fauna-1"] = []byte("png-bytes")
	tr.media["first"] = []MediaItem{imageItem("fauna-1")}
	tr.media["second"] = []MediaItem{imageItem("fauna-1")}

	items := []ProjectionItem{
		{PostID: hexID(0x11), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("first")},
		{PostID: hexID(0x12), CreatedAtMicros: 2_000_000, Kind: KindPost, Payload: []byte("second")},
	}
	if _, err := p.ProjectItems(ctx, did, key, items); err != nil {
		t.Fatal(err)
	}
	if src.fetched["fauna-1"] != 1 {
		t.Fatalf("fetched the same attachment %d times, want 1", src.fetched["fauna-1"])
	}
	// Both records still reference it.
	for _, payload := range []string{"first", "second"} {
		if len(tr.seen[payload].images) != 1 {
			t.Fatalf("post %q lost its image on the dedup path", payload)
		}
	}
}

// Every drop cause publishes the POST and drops the IMAGE — withholding content
// the user consented to publish is the worse failure, and unlike a dropped ref
// it does not cascade.
func TestUnpublishableAttachmentDropsTheImageNotThePost(t *testing.T) {
	cases := []struct {
		name  string
		setup func(*fakeBlobSource)
	}{
		{"nest does not hold the bytes", func(s *fakeBlobSource) {}},
		{"fetch failed", func(s *fakeBlobSource) { s.fails["fauna-1"] = errors.New("connection reset") }},
		{"over the publish ceiling", func(s *fakeBlobSource) {
			s.blobs["fauna-1"] = make([]byte, MaxBlobBytes+1)
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			st, p, key, did, tr, src := newTestProjectorWithBlobs(t)
			tc.setup(src)
			tr.media["media post"] = []MediaItem{imageItem("fauna-1")}

			applied, err := p.ProjectItems(ctx, did, key, []ProjectionItem{
				{PostID: hexID(0x11), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("media post")},
			})
			if err != nil {
				t.Fatalf("an unpublishable attachment must not fail the pass: %v", err)
			}
			if applied != 1 {
				t.Fatalf("applied %d commits, want the post to project anyway", applied)
			}
			if len(tr.seen["media post"].images) != 0 {
				t.Fatal("an unpublishable attachment must not reach the record")
			}
			cids, _, err := st.ListBlobCIDs(ctx, did, "", 10)
			if err != nil {
				t.Fatal(err)
			}
			if len(cids) != 0 {
				t.Fatalf("stored %d blobs for an unpublishable attachment", len(cids))
			}
		})
	}
}

// A projector with no blob source (a unit test)
// still projects every post — media resolution is additive, never a gate.
func TestNilBlobSourceProjectsWithoutMedia(t *testing.T) {
	ctx := context.Background()
	_, p, key, did := newTestProjector(t)
	applied, err := p.ProjectItems(ctx, did, key, []ProjectionItem{
		{PostID: hexID(0x11), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("media post")},
	})
	if err != nil || applied != 1 {
		t.Fatalf("applied=%d err=%v", applied, err)
	}
}

// The #commit frame announces the blobs its records reference, so a consumer
// learns which to fetch without decoding the record. Asserted by parsing the
// emitted frame with INDIGO's own event decoder — the library a relay runs —
// rather than by an assertion about our own bytes.
func TestCommitFrameAnnouncesBlobs(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	data := []byte("png-bytes")
	src.blobs["fauna-1"] = data
	tr.media["media post"] = []MediaItem{imageItem("fauna-1")}

	if _, err := p.ProjectItems(ctx, did, key, []ProjectionItem{
		{PostID: hexID(0x11), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("media post")},
	}); err != nil {
		t.Fatal(err)
	}
	blobCID, err := BlobCIDForBytes(data)
	if err != nil {
		t.Fatal(err)
	}

	evt := decodeCommitFrame(t, outboxFrame(t, st, 1))
	if len(evt.Blobs) != 1 {
		t.Fatalf("frame announced %d blobs, want 1", len(evt.Blobs))
	}
	if got := evt.Blobs[0].String(); got != blobCID.String() {
		t.Fatalf("frame announced blob %s, want %s", got, blobCID.String())
	}

	// A media-free commit still announces an empty list, never null — the
	// lexicon's `blobs` is required.
	if _, err := p.ProjectItems(ctx, did, key, []ProjectionItem{
		{PostID: hexID(0x12), CreatedAtMicros: 2_000_000, Kind: KindPost, Payload: []byte("text only")},
	}); err != nil {
		t.Fatal(err)
	}
	if blobs := decodeCommitFrame(t, outboxFrame(t, st, 2)).Blobs; len(blobs) != 0 {
		t.Fatalf("text-only commit announced %d blobs", len(blobs))
	}
}

// outboxFrame returns the persisted firehose frame at seq — the exact bytes the
// broadcaster writes to a subscriber.
func outboxFrame(t *testing.T, st *Store, seq int64) []byte {
	t.Helper()
	events, err := st.EventsSince(context.Background(), seq-1, 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(events) == 0 || events[0].Seq != seq {
		t.Fatalf("no outbox frame at seq %d", seq)
	}
	return events[0].Payload
}

// decodeCommitFrame parses a persisted frame with INDIGO's own decoders — the
// header, then the #commit body — so what is asserted is what a relay would
// actually read off the wire.
func decodeCommitFrame(t *testing.T, frame []byte) *comatproto.SyncSubscribeRepos_Commit {
	t.Helper()
	r := bytes.NewReader(frame)
	var hdr events.EventHeader
	if err := hdr.UnmarshalCBOR(r); err != nil {
		t.Fatalf("decode frame header: %v", err)
	}
	if hdr.MsgType != "#commit" {
		t.Fatalf("frame type = %q, want #commit", hdr.MsgType)
	}
	var evt comatproto.SyncSubscribeRepos_Commit
	if err := evt.UnmarshalCBOR(r); err != nil {
		t.Fatalf("decode #commit body: %v", err)
	}
	return &evt
}

// ── Profile pictures (avatar / banner) ─────────────────────────────────────

// realPNG is the smallest byte prefix net/http's sniffer recognises as a PNG.
var realPNG = append([]byte("\x89PNG\r\n\x1a\n"), bytes.Repeat([]byte{0}, 16)...)

// TestProjectProfilePublishesAvatarAndBanner: both pictures are fetched,
// re-hashed and STORED before the commit that references them, land on the
// record as blob refs, and are announced in the #commit frame's blobs field.
func TestProjectProfilePublishesAvatarAndBanner(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	avatarBytes := realPNG
	bannerBytes := append(bytes.Clone(realPNG), 'b')
	src.blobs["fauna-avatar"] = avatarBytes
	src.blobs["fauna-banner"] = bannerBytes
	tr.profileMedia["Alice"] = fakeProfileMedia{
		avatar: &MediaItem{BlobCID: "fauna-avatar"},
		banner: &MediaItem{BlobCID: "fauna-banner"},
	}

	ok, err := p.ProjectProfile(ctx, did, key, []byte("Alice"))
	if err != nil {
		t.Fatal(err)
	}
	if !ok {
		t.Fatal("profile with pictures should commit")
	}
	// Both crossed the translator seam as resolved (already-stored) images.
	if tr.seenProfile.avatar == nil || tr.seenProfile.banner == nil {
		t.Fatalf("both pictures must reach the translator: %+v", tr.seenProfile)
	}
	// The ref carries the ATPROTO CID of the exact bytes served, never the
	// Fauna one — and the blob is servable BEFORE the commit referencing it.
	for _, tc := range []struct {
		name string
		data []byte
		got  *ResolvedImage
	}{
		{"avatar", avatarBytes, tr.seenProfile.avatar},
		{"banner", bannerBytes, tr.seenProfile.banner},
	} {
		want, err := BlobCIDForBytes(tc.data)
		if err != nil {
			t.Fatal(err)
		}
		if tc.got.BlobCID != want.String() {
			t.Errorf("%s ref = %q, want the sha256 CID %q", tc.name, tc.got.BlobCID, want)
		}
		if tc.got.MIME != "image/png" {
			t.Errorf("%s mime = %q, want image/png sniffed from the bytes", tc.name, tc.got.MIME)
		}
		if tc.got.SizeBytes != int64(len(tc.data)) {
			t.Errorf("%s size = %d, want %d", tc.name, tc.got.SizeBytes, len(tc.data))
		}
		if _, served, err := st.GetBlob(ctx, did, tc.got.BlobCID); err != nil || !served {
			t.Errorf("%s must be servable by getBlob before the commit: ok=%v err=%v", tc.name, served, err)
		}
	}
}

// TestProjectProfileUnpublishableAvatarDropsTheFieldNotTheProfile: the profile
// still projects when a picture cannot be published — dropping the FIELD, never
// withholding the profile. A handle with posts and no profile record reads as
// broken/bot-like on bsky.app, which is what puts profile in projection scope.
func TestProjectProfileUnpublishableAvatarDropsTheFieldNotTheProfile(t *testing.T) {
	ctx := context.Background()
	_, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	// The nest does not hold the avatar bytes; the banner is fine.
	src.blobs["fauna-banner"] = realPNG
	tr.profileMedia["Alice"] = fakeProfileMedia{
		avatar: &MediaItem{BlobCID: "fauna-avatar-missing"},
		banner: &MediaItem{BlobCID: "fauna-banner"},
	}

	ok, err := p.ProjectProfile(ctx, did, key, []byte("Alice"))
	if err != nil {
		t.Fatal(err)
	}
	if !ok {
		t.Fatal("an unpublishable picture must not withhold the profile")
	}
	if tr.seenProfile.avatar != nil {
		t.Errorf("unfetchable avatar must be dropped, got %+v", tr.seenProfile.avatar)
	}
	if tr.seenProfile.banner == nil {
		t.Error("one unpublishable picture must not cost the other")
	}
}

// TestProjectProfileDropsNonImageBytes: a picture whose bytes are not an image
// is dropped rather than published under an invented media type. The blob ref
// requires a mimeType, and a profile declares none — so the bytes are the only
// source of truth, and bytes that are not an image have no honest answer.
func TestProjectProfileDropsNonImageBytes(t *testing.T) {
	ctx := context.Background()
	_, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	src.blobs["fauna-avatar"] = []byte("this is plainly not an image at all")
	tr.profileMedia["Alice"] = fakeProfileMedia{avatar: &MediaItem{BlobCID: "fauna-avatar"}}

	if _, err := p.ProjectProfile(ctx, did, key, []byte("Alice")); err != nil {
		t.Fatal(err)
	}
	if tr.seenProfile.avatar != nil {
		t.Errorf("non-image bytes must be dropped, got mime %q", tr.seenProfile.avatar.MIME)
	}
}

// TestProjectProfileClearingAPictureRemovesTheRef: the arm a create-only path
// gets wrong. Clearing an avatar must stop the record referencing it — and,
// because the record then genuinely differs, must still produce a commit.
func TestProjectProfileClearingAPictureRemovesTheRef(t *testing.T) {
	ctx := context.Background()
	_, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	src.blobs["fauna-avatar"] = realPNG
	tr.profileMedia["Alice"] = fakeProfileMedia{avatar: &MediaItem{BlobCID: "fauna-avatar"}}
	if _, err := p.ProjectProfile(ctx, did, key, []byte("Alice")); err != nil {
		t.Fatal(err)
	}
	if tr.seenProfile.avatar == nil {
		t.Fatal("setup: the avatar should have projected")
	}

	// The user clears it: same profile bytes, no picture declared any more.
	tr.profileMedia["Alice"] = fakeProfileMedia{}
	ok, err := p.ProjectProfile(ctx, did, key, []byte("Alice"))
	if err != nil {
		t.Fatal(err)
	}
	if tr.seenProfile.avatar != nil {
		t.Errorf("a cleared avatar must not still be referenced: %+v", tr.seenProfile.avatar)
	}
	if !ok {
		t.Error("clearing a picture changes the record, so it must commit")
	}
}

// TestRenderProfileRecordIsExactlyWhatAProjectionPassCommits is the F2.4-slice-3
// pin, and the reason the write path renders through the projection instead of
// having the nest render its own copy.
//
// An external app's profile write must commit the record a projection pass would
// commit, or the `cid` answered synchronously names bytes the next pass
// overwrites (atproto-pds-full.md § F2 detail, "What the repo carries is the
// NEST's rendering"). Equality here is **identity, not agreement**: the write
// path calls [Projector.RenderProfileRecord] and so does [Projector.ProjectProfile],
// so there is no second rendering to keep in step.
//
// The three scenarios are the three a nest-side renderer would have got wrong,
// because each answer lives in bytes or state only this side holds: the picture's
// ATProto CID/MIME/size (the blob store, keyed by FAUNA CID), and the
// publishability verdict (sniffed from the bytes — atproto-pds-bridge.md
// § Projection & backfill, which owns the drop rule).
func TestRenderProfileRecordIsExactlyWhatAProjectionPassCommits(t *testing.T) {
	for _, tc := range []struct {
		name   string
		blobs  map[string][]byte
		media  fakeProfileMedia
		expect string
	}{
		{
			name: "both pictures publish",
			blobs: map[string][]byte{
				"fauna-avatar": realPNG,
				"fauna-banner": append(bytes.Clone(realPNG), 'b'),
			},
			media: fakeProfileMedia{
				avatar: &MediaItem{BlobCID: "fauna-avatar"},
				banner: &MediaItem{BlobCID: "fauna-banner"},
			},
			expect: "the blob refs the store resolved",
		},
		{
			name:  "an unfetchable avatar drops its field",
			blobs: map[string][]byte{"fauna-banner": realPNG},
			media: fakeProfileMedia{
				avatar: &MediaItem{BlobCID: "fauna-avatar-missing"},
				banner: &MediaItem{BlobCID: "fauna-banner"},
			},
			expect: "the drop verdict, which needs the bytes",
		},
		{
			name:   "non-image bytes drop their field",
			blobs:  map[string][]byte{"fauna-avatar": []byte("plainly not an image")},
			media:  fakeProfileMedia{avatar: &MediaItem{BlobCID: "fauna-avatar"}},
			expect: "the sniff verdict, which needs the bytes",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			st, p, key, did, tr, src := newTestProjectorWithBlobs(t)
			for cid, data := range tc.blobs {
				src.blobs[cid] = data
			}
			tr.profileMedia["Alice"] = tc.media

			// What the WRITE PATH commits, and answers the caller's `cid` from.
			renderedJSON, _, err := p.RenderProfileRecord(ctx, did, []byte("Alice"))
			if err != nil {
				t.Fatalf("render: %v", err)
			}
			renderedCBOR, err := JSONRecordToDagCBOR(renderedJSON)
			if err != nil {
				t.Fatalf("encode the rendered record: %v", err)
			}
			renderedCID, err := RecordCID(renderedCBOR)
			if err != nil {
				t.Fatalf("cid of the rendered record: %v", err)
			}

			// What a PROJECTION PASS commits for the very same account.
			if ok, err := p.ProjectProfile(ctx, did, key, []byte("Alice")); err != nil || !ok {
				t.Fatalf("projection pass: ok=%v err=%v", ok, err)
			}
			committedCID, committedCBOR, ok, err := st.GetRecord(ctx, did, NsidProfile, RkeyProfile)
			if err != nil || !ok {
				t.Fatalf("read back the projected record: ok=%v err=%v", ok, err)
			}

			if committedCID != renderedCID {
				t.Errorf("the answered cid would not survive the next projection pass:\n"+
					" rendered  %s\n committed %s\n(the two must agree on %s)",
					renderedCID, committedCID, tc.expect)
			}
			if !bytes.Equal(committedCBOR, renderedCBOR) {
				t.Errorf("rendered bytes differ from what the projection commits:\n"+
					" rendered  %x\n committed %x", renderedCBOR, committedCBOR)
			}
		})
	}
}

// TestProjectProfileUnchangedWithPicturesDoesNotRecommit: the dedup path must
// also reduce to no-change. A profile whose picture was already published
// re-resolves to the same blob CID every pass, so the record is identical and
// must not be re-announced to the network.
func TestProjectProfileUnchangedWithPicturesDoesNotRecommit(t *testing.T) {
	ctx := context.Background()
	_, p, key, did, tr, src := newTestProjectorWithBlobs(t)
	src.blobs["fauna-avatar"] = realPNG
	tr.profileMedia["Alice"] = fakeProfileMedia{avatar: &MediaItem{BlobCID: "fauna-avatar"}}
	if ok, err := p.ProjectProfile(ctx, did, key, []byte("Alice")); err != nil || !ok {
		t.Fatalf("first projection: ok=%v err=%v", ok, err)
	}
	ok, err := p.ProjectProfile(ctx, did, key, []byte("Alice"))
	if err != nil {
		t.Fatal(err)
	}
	if ok {
		t.Error("an unchanged profile with a picture must not re-commit")
	}
	// And the blob was not re-fetched: the dedup lookup answers from the store.
	if src.fetched["fauna-avatar"] != 1 {
		t.Errorf("avatar fetched %d times, want 1 (re-projection must be free)", src.fetched["fauna-avatar"])
	}
}
