package atprotorepo

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
)

// NSIDs the projection maps Fauna objects onto.
const (
	NsidFeedPost = "app.bsky.feed.post"
	NsidProfile  = "app.bsky.actor.profile"
	RkeyProfile  = "self" // the profile is a mutable singleton
)

// Translator is the shared-Rust translation surface, reached over the tracked
// FFI binding in production and faked in tests. It isolates the CGO boundary so
// the projection orchestration stays unit-testable. The three methods mirror
// the fauna-ffi exports AtprotoTranslatePostRecord / AtprotoTranslateProfileRecord
// / AtprotoDeterministicTid.
type Translator interface {
	// PostRecord translates stored public-post bytes to an app.bsky.feed.post
	// record JSON. replyJSON/quoteJSON carry a resolved parent reference JSON
	// (nil = project standalone). images are attachments already fetched,
	// re-hashed and STORED (empty = no media embed); video is the same contract
	// for an assembled video blob (nil = none), and the two are mutually
	// exclusive by construction. selfLabels is the (currently empty) label set.
	//
	// ok=false means the post must NOT be projected: the record would carry
	// neither text nor an embed, and committing it would publish a blank post
	// (§ Projection & backfill).
	PostRecord(postBytes []byte, replyJSON, quoteJSON *string, images []ResolvedImage, video *ResolvedVideo, selfLabels []string) (recordJSON string, ok bool, err error)
	// ProfileRecord translates stored profile bytes to app.bsky.actor.profile.
	// avatar/banner are pictures already fetched, re-hashed and STORED; nil
	// omits that field from the record (a cleared or unpublishable picture).
	ProfileRecord(profileBytes []byte, avatar, banner *ResolvedImage) (string, error)
	// ProfileMedia extracts the profile's avatar/banner, each nil when the
	// profile carries none. Unlike PostMedia's items these declare no MIME
	// type — a profile stores a bare content hash (see [MediaItem.MIME]).
	ProfileMedia(profileBytes []byte) (avatar, banner *MediaItem, err error)
	// PostRefs extracts the Fauna post ids this post replies to / quotes, as
	// post_map keys. Empty strings mean "no reference of that kind". The bridge
	// never decodes a Fauna post itself — this is the shared-Rust extraction
	// reached over the same FFI as the translators.
	PostRefs(postBytes []byte) (replyParentPostID, quotePostID string, err error)
	// PostMedia extracts the post's image attachments — already filtered to
	// image/*, capped at the lexicon's four, and video-free — so the caller can
	// fetch, re-hash and store each one. Same shared-Rust extraction as
	// PostRefs: Go interprets nothing about the post.
	PostMedia(postBytes []byte) ([]MediaItem, error)
	// PostVideo extracts the post's publishable video, or nil when it carries
	// none (not a video post, no segments, or a degenerate aspect ratio). The
	// renditions arrive best-first; choosing between them is the caller's, since
	// it is a byte-ceiling decision. Same shared-Rust extraction as PostMedia.
	PostVideo(postBytes []byte) (*VideoItem, error)
	// DeterministicTID derives the record key TID from a post's creation
	// instant + its 32-byte content-row digest (D-s3-2; never wall-clock).
	DeterministicTID(createdAtMicros int64, postID []byte) string
}

// ItemKind mirrors PublicPostItem.kind on the fetch_public_posts wire.
type ItemKind string

const (
	KindPost      ItemKind = "post"
	KindTombstone ItemKind = "tombstone"
)

// ProjectionItem is one decoded fetch_public_posts item (the Go wsrpc wrapper
// decodes the wire PublicPostItem into this; task 4b's deleted_post_id lands in
// DeletedPostID for tombstones).
type ProjectionItem struct {
	PostID          string // lowercase-hex 32-byte content-row id
	CreatedAtMicros int64
	Kind            ItemKind
	Payload         []byte // stored post bytes (post) / bare Tombstone (tombstone, unused here)
	DeletedPostID   string // lowercase-hex digest of the deleted post (tombstone only)
}

// Projector turns a stream of public-post items + a profile into repo commits
// through the funnel. It is the projection loop's engine; the loop (in the
// bridge main) supplies the fetched items, the signing key, and the
// resolvability gate around the first per-user emit.
type Projector struct {
	store      *Store
	funnel     *Funnel
	translator Translator
	blobs      BlobSource
	logger     *slog.Logger
}

// NewProjector builds a projector over the given store, funnel, and translator.
//
// A nil blobs source projects posts without media embeds — every other
// translation is unaffected, so a caller that has no byte route (a unit test,
// for instance) still produces valid records.
func NewProjector(store *Store, funnel *Funnel, translator Translator, blobs BlobSource, logger *slog.Logger) *Projector {
	if logger == nil {
		logger = slog.Default()
	}
	return &Projector{store: store, funnel: funnel, translator: translator, blobs: blobs, logger: logger}
}

// ProjectItems applies a batch of fetched items to did's repo, one commit per
// effective item, and returns how many commits it produced. Items already
// projected (present in post_map) are skipped, so the loop is idempotent and a
// mid-batch crash never double-projects. Each post becomes a createRecord; each
// tombstone whose deleted post is mapped becomes a deleteRecord (an unmapped
// tombstone is a no-op — the post was never projected).
//
// signer is the repo's K-256 signing key, unsealed by the caller.
//
// Reply/quote references are resolved against post_map (S5 slice 3): a
// reference whose target is bridged becomes a real `reply`/`embed.record`, and
// one whose target is not projects **standalone** with the ref dropped — never a
// dangling ref (atproto-pds-bridge.md § Projection & backfill, translation edges).
//
// opts are passed to every commit this batch produces; the projection loop uses
// [DeferFrameToSync] to collapse a huge downtime gap (S5 slice 4).
func (p *Projector) ProjectItems(ctx context.Context, did string, signer Signer, items []ProjectionItem, opts ...BatchOption) (int, error) {
	applied := 0
	for _, item := range items {
		switch item.Kind {
		case KindPost:
			ok, err := p.projectPost(ctx, did, signer, item, opts...)
			if err != nil {
				return applied, err
			}
			if ok {
				applied++
			}
		case KindTombstone:
			ok, err := p.projectTombstone(ctx, did, signer, item, opts...)
			if err != nil {
				return applied, err
			}
			if ok {
				applied++
			}
		default:
			return applied, fmt.Errorf("unknown projection item kind %q for post %s", item.Kind, item.PostID)
		}
	}
	return applied, nil
}

func (p *Projector) projectPost(ctx context.Context, did string, signer Signer, item ProjectionItem, opts ...BatchOption) (bool, error) {
	// Idempotency: skip a post already mapped to an AT-URI.
	if _, _, mapped, err := p.store.PostAtURI(ctx, did, item.PostID); err != nil {
		return false, err
	} else if mapped {
		return false, nil
	}
	replyJSON, quoteJSON, rootURI, rootCID, err := p.resolveRefs(ctx, item)
	if err != nil {
		return false, err
	}
	// Media is resolved — fetched, re-hashed, STORED — before the commit that
	// references it, so the blob is servable the instant the #commit reaches a
	// relay. A crash between the two leaves an orphan blob row, which is
	// harmless and re-derivable; the inverse order would publish a ref to bytes
	// no repo serves (atproto-pds-bridge.md § Projection & backfill).
	images, err := p.resolveMedia(ctx, did, item)
	if err != nil {
		return false, err
	}
	video, err := p.resolveVideo(ctx, did, item)
	if err != nil {
		return false, err
	}
	recordJSON, ok, err := p.translator.PostRecord(item.Payload, replyJSON, quoteJSON, images, video, nil)
	if err != nil {
		return false, fmt.Errorf("translate post %s: %w", item.PostID, err)
	}
	if !ok {
		// Neither text nor embed: a media post whose every attachment was
		// dropped, or a video that could not be assembled. Committing it would
		// publish a blank post, which asserts the user said nothing where
		// absence asserts nothing at all (§ Projection & backfill).
		//
		// Nothing is written — no record, no post_map row — so the post is not
		// counted as applied; the batch watermark still advances past it, and a
		// post that later becomes publishable is picked up by re-derivation
		// rather than by the live pass.
		p.logger.Info("atproto projection: post would project as a blank record; skipping it",
			"did", did, "post_id", item.PostID)
		return false, nil
	}
	recordCBOR, err := JSONRecordToDagCBOR(recordJSON)
	if err != nil {
		return false, fmt.Errorf("encode post %s: %w", item.PostID, err)
	}
	digest, err := hex.DecodeString(item.PostID)
	if err != nil {
		return false, fmt.Errorf("post id %q not hex: %w", item.PostID, err)
	}
	rkey := p.translator.DeterministicTID(item.CreatedAtMicros, digest)
	blobCIDs := make([]string, 0, len(images)+1)
	for _, img := range images {
		blobCIDs = append(blobCIDs, img.BlobCID)
	}
	if video != nil {
		blobCIDs = append(blobCIDs, video.BlobCID)
	}

	res, err := p.funnel.ApplyBatch(ctx, did, signer, []RepoOp{{
		Action: ActionCreate, Collection: NsidFeedPost, Rkey: rkey,
		RecordCBOR: recordCBOR, FaunaPostID: item.PostID,
		RootURI: rootURI, RootCID: rootCID, BlobCIDs: blobCIDs,
	}}, opts...)
	if err != nil {
		return false, err
	}
	return !res.NoChange, nil
}

// resolveMedia turns a post's Fauna image attachments into publishable,
// already-stored blob refs. See [ResolveMedia] for the drop rules; with no
// blob source configured the post simply projects without media.
func (p *Projector) resolveMedia(ctx context.Context, did string, item ProjectionItem) ([]ResolvedImage, error) {
	if p.blobs == nil {
		return nil, nil
	}
	media, err := p.translator.PostMedia(item.Payload)
	if err != nil {
		return nil, fmt.Errorf("extract media of post %s: %w", item.PostID, err)
	}
	if len(media) == 0 {
		return nil, nil
	}
	return ResolveMedia(ctx, p.store, p.blobs, did, media, p.logger)
}

// resolveVideo assembles the post's video into a stored blob ref, or nil when
// the post carries none or none could be published. See [ResolveVideo] for the
// drop rules; with no blob source configured the post simply projects without
// video — which for a video post means it does not project at all, since it has
// no text of its own.
func (p *Projector) resolveVideo(ctx context.Context, did string, item ProjectionItem) (*ResolvedVideo, error) {
	if p.blobs == nil {
		return nil, nil
	}
	video, err := p.translator.PostVideo(item.Payload)
	if err != nil {
		return nil, fmt.Errorf("extract video of post %s: %w", item.PostID, err)
	}
	if video == nil {
		return nil, nil
	}
	return ResolveVideo(ctx, p.store, p.blobs, did, video, p.logger)
}

// resolveProfileMedia fetches, re-hashes and stores the profile's avatar and
// banner, returning nil for either one the projection cannot publish.
//
// The two are resolved INDEPENDENTLY on purpose: one unpublishable picture must
// not cost the other, and neither may cost the profile itself. A nil result is
// the translator's signal to omit that field — the same reading § Projection &
// backfill ratified for a post attachment, where the post publishes minus the
// image rather than not at all. Here the stakes are higher than for a post: a
// profile that failed to project reads as a broken/bot-like account on
// bsky.app, which is the very thing putting profile in projection scope.
//
// With no blob source configured the profile simply projects without pictures.
func (p *Projector) resolveProfileMedia(ctx context.Context, did string, profileBytes []byte) (avatar, banner *ResolvedImage, err error) {
	if p.blobs == nil {
		return nil, nil, nil
	}
	avatarItem, bannerItem, err := p.translator.ProfileMedia(profileBytes)
	if err != nil {
		return nil, nil, fmt.Errorf("extract profile media: %w", err)
	}
	resolve := func(item *MediaItem) (*ResolvedImage, error) {
		if item == nil {
			return nil, nil
		}
		out, err := ResolveMedia(ctx, p.store, p.blobs, did, []MediaItem{*item}, p.logger)
		if err != nil {
			return nil, err
		}
		if len(out) == 0 {
			return nil, nil // dropped — ResolveMedia logged why
		}
		return &out[0], nil
	}
	if avatar, err = resolve(avatarItem); err != nil {
		return nil, nil, err
	}
	if banner, err = resolve(bannerItem); err != nil {
		return nil, nil, err
	}
	return avatar, banner, nil
}

// resolveRefs turns a post's Fauna reply/quote references into the translator's
// replyJSON/quoteJSON arguments, and reports the thread root the new record will
// anchor to (empty when it is itself a root).
//
// The rule, per atproto-pds-bridge.md § Projection & backfill: a reference whose
// target is not in post_map is DROPPED and the post projects standalone — never
// a ref to a record no repo serves. Dropping (rather than skipping the post) is
// the resolved reading of that section's either/or: skipping would cascade,
// since a skipped reply is itself absent from post_map and every reply beneath
// it would skip too, silently withholding a whole subtree of content the user
// did consent to publish.
//
// Lookups are cross-repo on purpose — replying to another bridged actor is the
// ordinary case, and their record lives in their own DID's repo.
func (p *Projector) resolveRefs(ctx context.Context, item ProjectionItem) (replyJSON, quoteJSON *string, rootURI, rootCID string, err error) {
	parentPostID, quotePostID, err := p.translator.PostRefs(item.Payload)
	if err != nil {
		return nil, nil, "", "", fmt.Errorf("extract refs of post %s: %w", item.PostID, err)
	}
	if parentPostID != "" {
		parent, mapped, err := p.store.ProjectedPostAnyRepo(ctx, parentPostID)
		if err != nil {
			return nil, nil, "", "", fmt.Errorf("resolve reply parent of %s: %w", item.PostID, err)
		}
		if mapped {
			// The reply's root is the parent's root, or the parent itself when
			// the parent starts the thread.
			rootURI, rootCID = parent.ThreadRoot()
			encoded, err := json.Marshal(map[string]string{
				"parent_uri": parent.AtURI, "parent_cid": parent.RecordCID,
				"root_uri": rootURI, "root_cid": rootCID,
			})
			if err != nil {
				return nil, nil, "", "", fmt.Errorf("encode reply refs of %s: %w", item.PostID, err)
			}
			s := string(encoded)
			replyJSON = &s
		}
	}
	if quotePostID != "" {
		quoted, mapped, err := p.store.ProjectedPostAnyRepo(ctx, quotePostID)
		if err != nil {
			return nil, nil, "", "", fmt.Errorf("resolve quote target of %s: %w", item.PostID, err)
		}
		if mapped {
			encoded, err := json.Marshal(map[string]string{
				"uri": quoted.AtURI, "cid": quoted.RecordCID,
			})
			if err != nil {
				return nil, nil, "", "", fmt.Errorf("encode quote ref of %s: %w", item.PostID, err)
			}
			s := string(encoded)
			quoteJSON = &s
		}
	}
	return replyJSON, quoteJSON, rootURI, rootCID, nil
}

func (p *Projector) projectTombstone(ctx context.Context, did string, signer Signer, item ProjectionItem, opts ...BatchOption) (bool, error) {
	if item.DeletedPostID == "" {
		return false, nil // nothing to resolve
	}
	atURI, _, mapped, err := p.store.PostAtURI(ctx, did, item.DeletedPostID)
	if err != nil {
		return false, err
	}
	if !mapped {
		return false, nil // never projected -> nothing to delete
	}
	collection, rkey, err := parseATURI(atURI)
	if err != nil {
		return false, fmt.Errorf("post_map at_uri %q: %w", atURI, err)
	}
	res, err := p.funnel.ApplyBatch(ctx, did, signer, []RepoOp{{
		Action: ActionDelete, Collection: collection, Rkey: rkey, FaunaPostID: item.DeletedPostID,
	}}, opts...)
	if err != nil {
		return false, err
	}
	return !res.NoChange, nil
}

// RenderProfileRecord renders the `app.bsky.actor.profile` record for did from
// the account's current Fauna profile bytes — resolving the pictures (fetch,
// sniff, re-hash, store) and translating — and returns the record JSON plus the
// ATProto blob CIDs it references.
//
// **This is the ONE renderer of a projection-owned record, and that is the
// point.** [Projector.ProjectProfile] calls it, and so does the external-write
// path, through the `RepoWriter` seam, when the nest answers
// `reproject_record` (F2.4 slice 3). The write path must commit the record a
// projection pass *would* commit, or the `cid` it answers synchronously names
// bytes the next pass overwrites (atproto-pds-full.md § F2 detail, "What the
// repo carries is the NEST's rendering"). Sharing this function makes that
// **identity rather than agreement** — there is no second rendering to keep in
// step, which matters because two of the three answers here exist only on this
// side of the wire: a picture's ATProto CID/MIME/size come from the blob store
// keyed by its FAUNA CID, and the publishability verdict is sniffed from the
// bytes (atproto-pds-bridge.md § Projection & backfill owns the drop rule). A
// nest-side copy could not compute either.
//
// The returned blob CIDs are for a caller that must announce the set *before*
// encoding — the projection's own `#commit` op. The write path ignores them and
// walks the committed bytes instead (`ExtractBlobRefs`), which is the ratified
// source there precisely because it cannot miss a blob the record carries.
func (p *Projector) RenderProfileRecord(
	ctx context.Context, did string, profileBytes []byte,
) (recordJSON string, blobCIDs []string, err error) {
	avatar, banner, err := p.resolveProfileMedia(ctx, did, profileBytes)
	if err != nil {
		return "", nil, err
	}
	recordJSON, err = p.translator.ProfileRecord(profileBytes, avatar, banner)
	if err != nil {
		return "", nil, fmt.Errorf("translate profile: %w", err)
	}
	blobCIDs = make([]string, 0, 2)
	for _, img := range []*ResolvedImage{avatar, banner} {
		if img != nil {
			blobCIDs = append(blobCIDs, img.BlobCID)
		}
	}
	return recordJSON, blobCIDs, nil
}

// ProjectProfile projects the user's profile to app.bsky.actor.profile at rkey
// "self" (create on first sight, update thereafter — the funnel corrects the
// firehose op action). A nil/empty profile is a no-op: most users project posts
// before ever setting a profile.
//
// The projection loop calls this on EVERY pass, edit or not, so the batch runs
// with [SkipUnchangedRecords]: a profile nobody touched re-derives to the same
// bytes and must not mint a rev or broadcast a #commit. "Follows profile edits"
// (atproto-pds-bridge.md § Projection & backfill) means exactly the edits.
func (p *Projector) ProjectProfile(ctx context.Context, did string, signer Signer, profileBytes []byte, opts ...BatchOption) (bool, error) {
	if len(profileBytes) == 0 {
		return false, nil
	}
	recordJSON, blobCIDs, err := p.RenderProfileRecord(ctx, did, profileBytes)
	if err != nil {
		return false, err
	}
	recordCBOR, err := JSONRecordToDagCBOR(recordJSON)
	if err != nil {
		return false, fmt.Errorf("encode profile: %w", err)
	}
	// A fresh slice, never append-onto-opts: a spread variadic passes the
	// caller's own slice, so appending could write into its backing array.
	batchOpts := make([]BatchOption, 0, len(opts)+1)
	batchOpts = append(batchOpts, opts...)
	batchOpts = append(batchOpts, SkipUnchangedRecords())
	res, err := p.funnel.ApplyBatch(ctx, did, signer, []RepoOp{{
		Action: ActionUpdate, Collection: NsidProfile, Rkey: RkeyProfile, RecordCBOR: recordCBOR,
		BlobCIDs: blobCIDs,
	}}, batchOpts...)
	if err != nil {
		return false, err
	}
	return !res.NoChange, nil
}

// parseATURI splits at://<did>/<collection>/<rkey> into its collection and rkey.
func parseATURI(uri string) (collection, rkey string, err error) {
	rest, ok := strings.CutPrefix(uri, "at://")
	if !ok {
		return "", "", fmt.Errorf("not an at:// uri")
	}
	parts := strings.Split(rest, "/")
	if len(parts) != 3 {
		return "", "", fmt.Errorf("want did/collection/rkey, got %d segments", len(parts))
	}
	return parts[1], parts[2], nil
}
