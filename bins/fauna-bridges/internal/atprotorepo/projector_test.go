package atprotorepo

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"strings"
	"testing"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/bluesky-social/indigo/atproto/syntax"
)

// hexID returns a lowercase-hex 32-byte content-row id filled with seed.
func hexID(seed byte) string {
	return hex.EncodeToString(bytes.Repeat([]byte{seed}, 32))
}

// fakeTranslator stands in for the shared-Rust FFI: it emits minimal but valid
// atproto record JSON and a real (deterministic) TID clock keyed by post id.
type fakeTranslator struct {
	tids map[string]string // postID hex -> assigned rkey
	clk  *syntax.TIDClock
	// refs is what the fake reports a payload's Fauna references to be — the
	// stand-in for the shared-Rust extraction. Keyed by payload (= post text).
	refs map[string]fakeRefs
	// media are the image attachments a payload declares (what the shared-Rust
	// extraction would return).
	media map[string][]MediaItem
	// videos is the publishable video a payload declares, keyed the same way.
	videos map[string]*VideoItem
	// textless marks payloads whose post body carries no text of its own — a
	// media or video post. Only these can reach the blank-record skip.
	textless map[string]bool
	// seen records the resolved refs each payload was translated with, so a
	// test can assert what actually crossed the translator seam.
	seen map[string]fakeSeen
	// profileMedia is what the fake reports a profile's pictures to be, keyed
	// by profile bytes (the stand-in for the shared-Rust extraction).
	profileMedia map[string]fakeProfileMedia
	// seenProfile is what ProfileRecord was last handed.
	seenProfile fakeSeenProfile
}

// fakeProfileMedia are the picture descriptors a profile declares.
type fakeProfileMedia struct{ avatar, banner *MediaItem }

// fakeSeenProfile is what ProfileRecord was handed; nil = the field is omitted.
type fakeSeenProfile struct {
	avatar, banner *ResolvedImage
	called         bool
}

// fakeRefs are the Fauna post ids a payload refers to (post_map keys).
type fakeRefs struct{ replyParent, quote string }

// fakeSeen is what PostRecord was handed; nil = the ref was dropped.
type fakeSeen struct {
	reply, quote *string
	images       []ResolvedImage
	video        *ResolvedVideo
}

func newFakeTranslator() *fakeTranslator {
	return &fakeTranslator{
		tids:         map[string]string{},
		clk:          syntax.NewTIDClock(0),
		refs:         map[string]fakeRefs{},
		seen:         map[string]fakeSeen{},
		media:        map[string][]MediaItem{},
		profileMedia: map[string]fakeProfileMedia{},
		videos:       map[string]*VideoItem{},
		textless:     map[string]bool{},
	}
}

func (f *fakeTranslator) PostRecord(postBytes []byte, replyJSON, quoteJSON *string, images []ResolvedImage, video *ResolvedVideo, selfLabels []string) (string, bool, error) {
	f.seen[string(postBytes)] = fakeSeen{reply: replyJSON, quote: quoteJSON, images: images, video: video}
	// Mirror the real translator's skip rule: a record with neither text nor an
	// embed is not projected at all. The payload doubles as the post text, so a
	// text-less post is one whose payload is the empty marker.
	blankText := f.textless[string(postBytes)]
	if blankText && len(images) == 0 && video == nil && quoteJSON == nil {
		return "", false, nil
	}
	text := string(postBytes)
	if blankText {
		text = ""
	}
	// The payload doubles as the post text so tests can assert content flow.
	rec := fmt.Sprintf(`{"$type":%q,"text":%q,"createdAt":"2026-01-01T00:00:00Z","langs":["en"]`,
		NsidFeedPost, text)
	if replyJSON != nil {
		rec += `,"reply":` + *replyJSON
	}
	if quoteJSON != nil {
		rec += `,"embed":` + *quoteJSON
	}
	if video != nil {
		rec += fmt.Sprintf(`,"embed":{"$type":"app.bsky.embed.video","video":{"$type":"blob","ref":{"$link":%q},"mimeType":%q,"size":%d}}`,
			video.BlobCID, video.MIME, video.SizeBytes)
	}
	return rec + "}", true, nil
}

func (f *fakeTranslator) PostVideo(postBytes []byte) (*VideoItem, error) {
	return f.videos[string(postBytes)], nil
}

func (f *fakeTranslator) PostRefs(postBytes []byte) (replyParentPostID, quotePostID string, err error) {
	r := f.refs[string(postBytes)]
	return r.replyParent, r.quote, nil
}

func (f *fakeTranslator) PostMedia(postBytes []byte) ([]MediaItem, error) {
	return f.media[string(postBytes)], nil
}

// ProfileRecord mirrors the real translator: the pictures the caller resolved
// land as data-model blob refs, and a nil one omits its field entirely.
func (f *fakeTranslator) ProfileRecord(profileBytes []byte, avatar, banner *ResolvedImage) (string, error) {
	f.seenProfile = fakeSeenProfile{avatar: avatar, banner: banner, called: true}
	rec := fmt.Sprintf(`{"$type":%q,"displayName":%q`, NsidProfile, string(profileBytes))
	blobRef := func(img *ResolvedImage) string {
		return fmt.Sprintf(`{"$type":"blob","ref":{"$link":%q},"mimeType":%q,"size":%d}`,
			img.BlobCID, img.MIME, img.SizeBytes)
	}
	if avatar != nil {
		rec += `,"avatar":` + blobRef(avatar)
	}
	if banner != nil {
		rec += `,"banner":` + blobRef(banner)
	}
	return rec + "}", nil
}

func (f *fakeTranslator) ProfileMedia(profileBytes []byte) (avatar, banner *MediaItem, err error) {
	m, ok := f.profileMedia[string(profileBytes)]
	if !ok {
		return nil, nil, nil
	}
	return m.avatar, m.banner, nil
}

func (f *fakeTranslator) DeterministicTID(createdAtMicros int64, postID []byte) string {
	// A stable rkey per post id (real FFI derives it from created_at+digest;
	// the funnel only needs a valid, unique-per-record TID).
	key := hex.EncodeToString(postID)
	if t, ok := f.tids[key]; ok {
		return t
	}
	t := f.clk.Next().String()
	f.tids[key] = t
	return t
}

func newTestProjector(t *testing.T) (*Store, *Projector, Signer, string) {
	t.Helper()
	st, p, key, did, _ := newTestProjectorWithFake(t)
	return st, p, key, did
}

// newTestProjectorWithFake additionally hands back the fake translator, so a
// test can declare a payload's references and assert what the resolution fed
// back across the seam.
func newTestProjectorWithFake(t *testing.T) (*Store, *Projector, Signer, string, *fakeTranslator) {
	t.Helper()
	st, f := newTestFunnel(t, nil)
	key, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}
	tr := newFakeTranslator()
	// The projector's subject is projection, never the first-emit gate: in
	// production the projection loop opens the gate before it ever reaches
	// ProjectItems, so the fixture starts from that same state.
	did := "did:web:alice.example"
	announced(t, st, did)
	return st, NewProjector(st, f, tr, nil, nil), key, did, tr
}

// TestProjectPostsAndSkipDuplicates: fresh posts project; re-projecting the same
// items is a no-op (idempotent on post_map).
func TestProjectPostsAndSkipDuplicates(t *testing.T) {
	ctx := context.Background()
	st, p, key, did := newTestProjector(t)
	items := []ProjectionItem{
		{PostID: hexID(0x11), CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("hello")},
		{PostID: hexID(0x22), CreatedAtMicros: 2_000_000, Kind: KindPost, Payload: []byte("world")},
	}
	n, err := p.ProjectItems(ctx, did, key, items)
	if err != nil {
		t.Fatal(err)
	}
	if n != 2 {
		t.Fatalf("projected %d, want 2", n)
	}
	for _, it := range items {
		if _, _, ok, _ := st.PostAtURI(ctx, did, it.PostID); !ok {
			t.Errorf("post %s not in post_map", it.PostID)
		}
	}
	// Re-project: idempotent.
	n2, err := p.ProjectItems(ctx, did, key, items)
	if err != nil {
		t.Fatal(err)
	}
	if n2 != 0 {
		t.Errorf("re-project applied %d, want 0 (idempotent)", n2)
	}
}

// TestProjectTombstoneDeletes: a tombstone whose deleted post was projected
// removes it; an unmapped tombstone is a no-op.
func TestProjectTombstoneDeletes(t *testing.T) {
	ctx := context.Background()
	st, p, key, did := newTestProjector(t)
	postID := hexID(0x33)
	if _, err := p.ProjectItems(ctx, did, key, []ProjectionItem{
		{PostID: postID, CreatedAtMicros: 1_000_000, Kind: KindPost, Payload: []byte("doomed")},
	}); err != nil {
		t.Fatal(err)
	}

	// Unmapped tombstone: no-op.
	n, err := p.ProjectItems(ctx, did, key, []ProjectionItem{
		{PostID: "row1", CreatedAtMicros: 3_000_000, Kind: KindTombstone, DeletedPostID: hexID(0x44)},
	})
	if err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Errorf("unmapped tombstone applied %d, want 0", n)
	}

	// Mapped tombstone: deletes.
	n, err = p.ProjectItems(ctx, did, key, []ProjectionItem{
		{PostID: "row2", CreatedAtMicros: 4_000_000, Kind: KindTombstone, DeletedPostID: postID},
	})
	if err != nil {
		t.Fatal(err)
	}
	if n != 1 {
		t.Fatalf("mapped tombstone applied %d, want 1", n)
	}
	if _, _, ok, _ := st.PostAtURI(ctx, did, postID); ok {
		t.Error("deleted post should be gone from post_map")
	}
}

// TestProjectProfile: first projection creates the singleton, a second updates
// it in place at rkey "self".
func TestProjectProfile(t *testing.T) {
	ctx := context.Background()
	_, p, key, did := newTestProjector(t)
	ok, err := p.ProjectProfile(ctx, did, key, []byte("Alice"))
	if err != nil {
		t.Fatal(err)
	}
	if !ok {
		t.Fatal("first profile projection should commit")
	}
	// Empty profile is a no-op.
	ok, err = p.ProjectProfile(ctx, did, key, nil)
	if err != nil {
		t.Fatal(err)
	}
	if ok {
		t.Error("empty profile should be a no-op")
	}
	// Update in place (different bytes).
	ok, err = p.ProjectProfile(ctx, did, key, []byte("Alice Updated"))
	if err != nil {
		t.Fatal(err)
	}
	if !ok {
		t.Error("profile update should commit")
	}
}

// TestProjectProfileUnchangedDoesNotRecommit: the projection loop re-projects
// the profile on EVERY pass, so a profile nobody edited must reduce to no
// commit at all. Without this the bridge mints a fresh rev and broadcasts a
// #commit for a byte-identical record forever — firehose noise addressed to
// the whole network, and (once avatar/banner land) a re-announcement of blobs
// that never changed.
func TestProjectProfileUnchangedDoesNotRecommit(t *testing.T) {
	ctx := context.Background()
	_, p, key, did := newTestProjector(t)
	ok, err := p.ProjectProfile(ctx, did, key, []byte("Alice"))
	if err != nil {
		t.Fatal(err)
	}
	if !ok {
		t.Fatal("first profile projection should commit")
	}
	// The next pass fetches the same profile and re-projects it.
	ok, err = p.ProjectProfile(ctx, did, key, []byte("Alice"))
	if err != nil {
		t.Fatal(err)
	}
	if ok {
		t.Error("re-projecting an UNCHANGED profile must not commit again")
	}
	// A real edit still gets through — the skip must not swallow changes.
	ok, err = p.ProjectProfile(ctx, did, key, []byte("Alice Edited"))
	if err != nil {
		t.Fatal(err)
	}
	if !ok {
		t.Error("an edited profile must still commit")
	}
}

// ── Reply/quote reference resolution (S5 slice 3) ──────────────────────────

// projectOne projects a single post whose payload is its text, returning nothing
// but failing the test if it does not commit.
func projectOne(t *testing.T, p *Projector, ctx context.Context, did string, key Signer, id string, micros int64, text string) {
	t.Helper()
	n, err := p.ProjectItems(ctx, did, key, []ProjectionItem{
		{PostID: id, CreatedAtMicros: micros, Kind: KindPost, Payload: []byte(text)},
	})
	if err != nil {
		t.Fatal(err)
	}
	if n != 1 {
		t.Fatalf("projecting %q applied %d commits, want 1", text, n)
	}
}

// decodeReply pulls the four reply fields the projector encoded for a payload.
func decodeReply(t *testing.T, tr *fakeTranslator, text string) map[string]string {
	t.Helper()
	seen, ok := tr.seen[text]
	if !ok {
		t.Fatalf("payload %q never reached the translator", text)
	}
	if seen.reply == nil {
		t.Fatalf("payload %q projected standalone, expected a reply ref", text)
	}
	var got map[string]string
	if err := json.Unmarshal([]byte(*seen.reply), &got); err != nil {
		t.Fatalf("reply json for %q: %v", text, err)
	}
	return got
}

// A reply to a bridged parent carries a real ref, and — the parent being the
// start of the thread — root == parent.
func TestReplyToBridgedParentResolvesRootToParent(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr := newTestProjectorWithFake(t)
	parentID := hexID(0x11)
	projectOne(t, p, ctx, did, key, parentID, 1_000_000, "parent")

	parentURI, parentCID, ok, err := st.PostAtURI(ctx, did, parentID)
	if err != nil || !ok {
		t.Fatalf("parent not mapped: ok=%v err=%v", ok, err)
	}

	tr.refs["reply"] = fakeRefs{replyParent: parentID}
	projectOne(t, p, ctx, did, key, hexID(0x12), 2_000_000, "reply")

	got := decodeReply(t, tr, "reply")
	if got["parent_uri"] != parentURI || got["parent_cid"] != parentCID {
		t.Errorf("parent = %s/%s, want %s/%s", got["parent_uri"], got["parent_cid"], parentURI, parentCID)
	}
	if got["root_uri"] != parentURI || got["root_cid"] != parentCID {
		t.Errorf("root = %s/%s, want the parent %s/%s (parent starts the thread)",
			got["root_uri"], got["root_cid"], parentURI, parentCID)
	}
}

// A reply to a reply anchors to the ORIGINAL root, not to its immediate parent —
// the inductive rule the stored root columns exist for.
func TestReplyToReplyCarriesTheOriginalRoot(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr := newTestProjectorWithFake(t)
	rootID, midID, leafID := hexID(0x21), hexID(0x22), hexID(0x23)

	projectOne(t, p, ctx, did, key, rootID, 1_000_000, "root")
	rootURI, rootCID, _, _ := st.PostAtURI(ctx, did, rootID)

	tr.refs["mid"] = fakeRefs{replyParent: rootID}
	projectOne(t, p, ctx, did, key, midID, 2_000_000, "mid")
	midURI, midCID, _, _ := st.PostAtURI(ctx, did, midID)

	tr.refs["leaf"] = fakeRefs{replyParent: midID}
	projectOne(t, p, ctx, did, key, leafID, 3_000_000, "leaf")

	got := decodeReply(t, tr, "leaf")
	if got["parent_uri"] != midURI || got["parent_cid"] != midCID {
		t.Errorf("parent = %s, want the mid post %s", got["parent_uri"], midURI)
	}
	if got["root_uri"] != rootURI || got["root_cid"] != rootCID {
		t.Errorf("root = %s, want the thread root %s (not the parent %s)",
			got["root_uri"], rootURI, midURI)
	}
}

// The translation edge: an unbridged parent drops the ref and the post projects
// standalone — it is never skipped, and never emits a dangling ref.
func TestReplyToUnbridgedParentProjectsStandalone(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr := newTestProjectorWithFake(t)
	orphanID := hexID(0x31) // never projected
	replyID := hexID(0x32)

	tr.refs["orphan reply"] = fakeRefs{replyParent: orphanID}
	projectOne(t, p, ctx, did, key, replyID, 1_000_000, "orphan reply")

	if seen := tr.seen["orphan reply"]; seen.reply != nil {
		t.Errorf("expected the ref dropped, got reply=%s", *seen.reply)
	}
	// Projected, not skipped: the post is in post_map and serves as a root.
	row, ok, err := st.ProjectedPostAnyRepo(ctx, replyID)
	if err != nil || !ok {
		t.Fatalf("standalone reply must still be projected: ok=%v err=%v", ok, err)
	}
	if row.RootURI != "" {
		t.Errorf("a standalone post has no stored root, got %q", row.RootURI)
	}
}

// A quote of a bridged post resolves; a quote of an unbridged post drops.
func TestQuoteResolutionAndDrop(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr := newTestProjectorWithFake(t)
	quotedID := hexID(0x41)
	projectOne(t, p, ctx, did, key, quotedID, 1_000_000, "quoted")
	quotedURI, quotedCID, _, _ := st.PostAtURI(ctx, did, quotedID)

	tr.refs["quoter"] = fakeRefs{quote: quotedID}
	projectOne(t, p, ctx, did, key, hexID(0x42), 2_000_000, "quoter")

	seen := tr.seen["quoter"]
	if seen.quote == nil {
		t.Fatal("quote of a bridged post must resolve")
	}
	var got map[string]string
	if err := json.Unmarshal([]byte(*seen.quote), &got); err != nil {
		t.Fatal(err)
	}
	if got["uri"] != quotedURI || got["cid"] != quotedCID {
		t.Errorf("quote = %s/%s, want %s/%s", got["uri"], got["cid"], quotedURI, quotedCID)
	}
	if seen.reply != nil {
		t.Error("a pure quote must not produce a reply ref")
	}

	tr.refs["dangling quoter"] = fakeRefs{quote: hexID(0x4f)}
	projectOne(t, p, ctx, did, key, hexID(0x43), 3_000_000, "dangling quoter")
	if s := tr.seen["dangling quoter"]; s.quote != nil {
		t.Errorf("unbridged quote target must drop, got %s", *s.quote)
	}
}

// Replying to ANOTHER bridged actor resolves into that actor's repo — the
// ordinary case on the network, and the reason the lookup is not did-scoped.
func TestReplyResolvesAcrossRepos(t *testing.T) {
	ctx := context.Background()
	st, p, key, aliceDID, tr := newTestProjectorWithFake(t)
	bobDID := "did:web:bob.example"
	announced(t, st, bobDID)
	bobKey, err := atcrypto.GeneratePrivateKeyK256()
	if err != nil {
		t.Fatal(err)
	}

	bobPostID := hexID(0x51)
	projectOne(t, p, ctx, bobDID, bobKey, bobPostID, 1_000_000, "bob's post")
	bobURI, bobCID, _, _ := st.PostAtURI(ctx, bobDID, bobPostID)

	tr.refs["alice replies to bob"] = fakeRefs{replyParent: bobPostID}
	projectOne(t, p, ctx, aliceDID, key, hexID(0x52), 2_000_000, "alice replies to bob")

	got := decodeReply(t, tr, "alice replies to bob")
	if got["parent_uri"] != bobURI || got["parent_cid"] != bobCID {
		t.Errorf("parent = %s, want bob's record %s", got["parent_uri"], bobURI)
	}
	if !strings.Contains(got["parent_uri"], bobDID) {
		t.Errorf("cross-repo parent must point into bob's repo, got %s", got["parent_uri"])
	}
}

// A parent that has since been deleted is gone from post_map, so a later reply
// to it projects standalone rather than referencing a record no repo serves.
func TestReplyToDeletedParentProjectsStandalone(t *testing.T) {
	ctx := context.Background()
	st, p, key, did, tr := newTestProjectorWithFake(t)
	parentID := hexID(0x61)
	projectOne(t, p, ctx, did, key, parentID, 1_000_000, "doomed parent")

	if _, err := p.ProjectItems(ctx, did, key, []ProjectionItem{
		{PostID: hexID(0x6f), CreatedAtMicros: 2_000_000, Kind: KindTombstone, DeletedPostID: parentID},
	}); err != nil {
		t.Fatal(err)
	}
	if _, _, ok, _ := st.PostAtURI(ctx, did, parentID); ok {
		t.Fatal("tombstone should have removed the parent from post_map")
	}

	tr.refs["late reply"] = fakeRefs{replyParent: parentID}
	projectOne(t, p, ctx, did, key, hexID(0x62), 3_000_000, "late reply")
	if s := tr.seen["late reply"]; s.reply != nil {
		t.Errorf("reply to a deleted parent must drop the ref, got %s", *s.reply)
	}
}
