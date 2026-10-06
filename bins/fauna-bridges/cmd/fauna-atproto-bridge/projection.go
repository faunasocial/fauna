// The S3 projection loop: turn a Fauna user's public posts + profile into
// signed ATProto repo commits and firehose frames (atproto-pds-bridge.md
// § Projection & backfill / § Where logic lives).
//
// It mirrors the S2 mint loop's shape — a poll ticker plus a push nudge
// (fauna.bridges.atproto.projection_ready), the outbound_ready best-effort
// pattern where the poll is the correctness backstop. Each pass sweeps the
// identity roster and, per ACTIVE identity (a DID has been minted), pulls the
// public-post stream from its stored cursor and the current profile, translates
// each via the shared-Rust FFI, and applies them through the per-user commit
// funnel (internal/atprotorepo). Translation runs in the bridge over the tracked
// FFI binding (D-s3-1): nest kinds speak Fauna domain shapes, protocol
// translation happens here.
//
// The FIRST firehose frame for a user is HARD-GATED on VerifyIdentityResolvable
// (atproto-pds-full.md § Ecosystem reality, first-impression trap): a DID that
// does not resolve at first AppView index can permanently 404, so a gated user
// projects nothing until it resolves. S2's mint-time check stays warn-only (a
// mint is not an emit); this is the real gate at the emit site.
package main

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	neturl "net/url"
	"strings"
	"sync"
	"time"

	"github.com/bluesky-social/indigo/atproto/atcrypto"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaAtproto "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_bridge_atproto"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// projPollInterval is how often the projection loop re-sweeps the roster even
// with no nudge — the poll backstop. A hard-coded cadence (product invariant:
// no config surface), matched to the mint loop's.
const projPollInterval = 30 * time.Second

// projPageLimit is the fetch_public_posts page size the loop requests; the nest
// clamps to its own ceiling, so this is only a hint.
const projPageLimit = 200

// ffiTranslator is the production atprotorepo.Translator: a thin passthrough to
// the shared-Rust translation exports over the tracked FFI binding. Isolating it
// behind the Translator interface keeps the projection orchestration
// (internal/atprotorepo) CGO-free and unit-testable with a fake.
type ffiTranslator struct{}

func (ffiTranslator) PostRecord(postBytes []byte, replyJSON, quoteJSON *string, images []atprotorepo.ResolvedImage, video *atprotorepo.ResolvedVideo, selfLabels []string) (string, bool, error) {
	ffiImages := make([]faunaFfi.AtprotoResolvedImage, 0, len(images))
	for _, img := range images {
		ffiImages = append(ffiImages, faunaFfi.AtprotoResolvedImage{
			BlobCid:   img.BlobCID,
			Mime:      img.MIME,
			SizeBytes: img.SizeBytes,
			Width:     img.Width,
			Height:    img.Height,
			Alt:       img.Alt,
		})
	}
	var ffiVideo *faunaFfi.AtprotoResolvedVideo
	if video != nil {
		ffiVideo = &faunaFfi.AtprotoResolvedVideo{
			BlobCid:      video.BlobCID,
			Mime:         video.MIME,
			SizeBytes:    video.SizeBytes,
			AspectWidth:  video.AspectWidth,
			AspectHeight: video.AspectHeight,
		}
	}
	record, err := faunaFfi.AtprotoTranslatePostRecord(postBytes, replyJSON, quoteJSON, ffiImages, ffiVideo, selfLabels)
	if err != nil {
		return "", false, err
	}
	// A nil record is the shared translator's "this post must not be projected"
	// — the record would carry neither text nor an embed. Distinct from an
	// error: nothing went wrong, there is simply nothing honest to publish.
	if record == nil {
		return "", false, nil
	}
	return *record, true, nil
}

func (ffiTranslator) ProfileRecord(profileBytes []byte, avatar, banner *atprotorepo.ResolvedImage) (string, error) {
	return faunaFfi.AtprotoTranslateProfileRecord(profileBytes, ffiResolvedImage(avatar), ffiResolvedImage(banner))
}

func (ffiTranslator) ProfileMedia(profileBytes []byte) (avatar, banner *atprotorepo.MediaItem, err error) {
	media, err := faunaFfi.AtprotoExtractProfileMedia(profileBytes)
	if err != nil {
		return nil, nil, err
	}
	return mediaItem(media.Avatar), mediaItem(media.Banner), nil
}

// ffiResolvedImage converts an already-stored image to its FFI face; nil stays
// nil, which is what omits the field from the projected record.
func ffiResolvedImage(img *atprotorepo.ResolvedImage) *faunaFfi.AtprotoResolvedImage {
	if img == nil {
		return nil
	}
	return &faunaFfi.AtprotoResolvedImage{
		BlobCid:   img.BlobCID,
		Mime:      img.MIME,
		SizeBytes: img.SizeBytes,
		Width:     img.Width,
		Height:    img.Height,
		Alt:       img.Alt,
	}
}

// mediaItem converts an FFI media descriptor to the projection's own type.
func mediaItem(item *faunaFfi.AtprotoMediaItem) *atprotorepo.MediaItem {
	if item == nil {
		return nil
	}
	return &atprotorepo.MediaItem{
		BlobCID:   item.BlobCid,
		MIME:      item.Mime,
		SizeBytes: item.SizeBytes,
		Width:     item.Width,
		Height:    item.Height,
		Alt:       item.Alt,
	}
}

func (ffiTranslator) DeterministicTID(createdAtMicros int64, postID []byte) string {
	return faunaFfi.AtprotoDeterministicTid(createdAtMicros, postID)
}

func (ffiTranslator) PostRefs(postBytes []byte) (replyParentPostID, quotePostID string, err error) {
	refs, err := faunaFfi.AtprotoExtractPostRefs(postBytes)
	if err != nil {
		return "", "", err
	}
	return optString(refs.ReplyParentPostId), optString(refs.QuotePostId), nil
}

func (ffiTranslator) PostMedia(postBytes []byte) ([]atprotorepo.MediaItem, error) {
	items, err := faunaFfi.AtprotoExtractPostMedia(postBytes)
	if err != nil {
		return nil, err
	}
	out := make([]atprotorepo.MediaItem, 0, len(items))
	for _, item := range items {
		out = append(out, atprotorepo.MediaItem{
			BlobCID:   item.BlobCid,
			MIME:      item.Mime,
			SizeBytes: item.SizeBytes,
			Width:     item.Width,
			Height:    item.Height,
			Alt:       item.Alt,
		})
	}
	return out, nil
}

func (ffiTranslator) PostVideo(postBytes []byte) (*atprotorepo.VideoItem, error) {
	video, err := faunaFfi.AtprotoExtractPostVideo(postBytes)
	if err != nil {
		return nil, err
	}
	if video == nil {
		return nil, nil
	}
	renditions := make([]atprotorepo.VideoRendition, 0, len(video.Renditions))
	for _, r := range video.Renditions {
		renditions = append(renditions, atprotorepo.VideoRendition{
			Height:        r.Height,
			SegmentCIDs:   r.SegmentCids,
			DeclaredBytes: r.DeclaredBytes,
		})
	}
	return &atprotorepo.VideoItem{
		ManifestCID:  video.ManifestCid,
		Renditions:   renditions,
		AspectWidth:  video.AspectWidth,
		AspectHeight: video.AspectHeight,
	}, nil
}

// nestBlobSource is the production atprotorepo.BlobSource: it pulls a public
// post's attachment bytes from the nest over the sanctioned CID-addressed byte
// route `GET /api/v1/blob/{cid_b32}` (api-layers.md § The four API protocols —
// the bulk-binary carve-out, and the target shape). The nest verifies
// blake3(body) == cid.digest() before answering, so the bytes arrive
// integrity-checked and this side re-hashes them only to derive the ATProto
// sha256 CID.
//
// Reading them is legal under § Firm boundaries because a public post's
// attachments are AudienceClass::PublicPost — the one class Fauna does not
// AEAD-seal. Nothing here can reach a sealed blob: the projection only ever
// walks public posts.
type nestBlobSource struct {
	client   *http.Client
	endpoint string
}

func (s nestBlobSource) FetchBlob(ctx context.Context, faunaCID string) ([]byte, bool, error) {
	if s.endpoint == "" {
		return nil, false, nil
	}
	url := strings.TrimSuffix(s.endpoint, "/") + "/api/v1/blob/" + neturl.PathEscape(faunaCID)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, false, fmt.Errorf("build blob request: %w", err)
	}
	resp, err := s.client.Do(req)
	if err != nil {
		return nil, false, fmt.Errorf("fetch blob %s: %w", faunaCID, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode == http.StatusNotFound {
		return nil, false, nil
	}
	if resp.StatusCode != http.StatusOK {
		return nil, false, fmt.Errorf("fetch blob %s: nest answered %s", faunaCID, resp.Status)
	}
	// Bounded read: a blob past the publish ceiling is dropped anyway, so
	// reading one extra byte is enough to recognise it without buffering an
	// unbounded body from a peer that mis-reports its size.
	data, err := io.ReadAll(io.LimitReader(resp.Body, atprotorepo.MaxBlobBytes+1))
	if err != nil {
		return nil, false, fmt.Errorf("read blob %s: %w", faunaCID, err)
	}
	return data, true, nil
}

// optString flattens an FFI optional to "" — the projection's "no reference of
// this kind", which it already treats the same as an unresolvable one.
func optString(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}

// ffiAuthorizer is the production D8 seam (atprotopds.Authorizer): every
// authenticated XRPC call's verdict comes from the shared-Rust module over the
// tracked binding. The adapter is pure carriage — see internal/atprotopds/authz.go
// for why no decision may live on this side of it.
//
// The binding lives in its own generated package because uniffi emits one Go
// package per uniffi namespace, and `authorize` is exported from
// fauna-bridge-atproto (where its input/verdict types are declared) rather
// than wrapped in fauna-ffi — a cross-namespace type reference is exactly what
// uniffi-bindgen-go cannot express.
type ffiAuthorizer struct{}

func (ffiAuthorizer) Authorize(in atprotopds.AuthzInput) atprotopds.AuthzVerdict {
	v := faunaAtproto.Authorize(faunaAtproto.AuthzInput{
		Plane:               in.Plane,
		Scopes:              in.Scopes,
		ExternalAppsEnabled: in.ExternalAppsEnabled,
		Lxm:                 in.Lxm,
		Aud:                 in.Aud,
		EndpointClass:       in.EndpointClass,
	})
	switch t := v.(type) {
	case faunaAtproto.AuthzVerdictAllow:
		return atprotopds.AuthzVerdict{Allow: true}
	case faunaAtproto.AuthzVerdictDeny:
		return atprotopds.AuthzVerdict{XrpcError: t.XrpcError, Message: t.Message}
	default:
		// Unreachable unless the module grows a verdict this build predates —
		// closed world: refuse rather than assume.
		return atprotopds.AuthzVerdict{
			XrpcError: "AuthenticationRequired",
			Message:   "authentication required",
		}
	}
}

// projDeps carries the projection loop's injectable seams. Production wiring
// comes from newProjDeps; tests substitute the unseal fn, the resolvability
// resolver/HTTP client/directory URL, and the page limit.
type projDeps struct {
	store     *atprotorepo.Store
	projector *atprotorepo.Projector
	// funnel is the same writer the projector commits through; the rename hook
	// uses it directly to emit #identity, which is a firehose frame with no
	// repo mutation behind it (D-s3-6).
	funnel *atprotorepo.Funnel
	// x25519Secret is the bridge's service-user X25519 private key — the
	// recipient secret the per-user identity key blobs are sealed to.
	x25519Secret []byte
	// unseal opens a sealed AtprotoIdentityBlob (production:
	// mailfauna.UnsealAtprotoIdentityBlob over the FFI).
	unseal unsealIdentityBlobFn
	// The pre-firehose resolvability gate's seams (atprotoid.VerifyIdentityResolvable).
	resolver         atprotoid.TXTResolver
	httpClient       *http.Client
	directoryBaseURL string
	pageLimit        uint32
	// selfChecked remembers which DIDs have had their head commit's SIGNATURE
	// verified this boot. main.go's boot check covers structure for every repo
	// without key material; the signature half runs here, once, because this is
	// where the unsealed signer exists.
	selfChecked sync.Map
	// didWebRenameWarned remembers which did:web identities have already had
	// their unfollowable rename reported this boot, so the 30s pass does not
	// repeat a condition only a human can resolve.
	didWebRenameWarned sync.Map
}

func newProjDeps(x25519Secret []byte, store *atprotorepo.Store, funnel *atprotorepo.Funnel, projector *atprotorepo.Projector) *projDeps {
	return &projDeps{
		store:            store,
		projector:        projector,
		funnel:           funnel,
		x25519Secret:     x25519Secret,
		unseal:           mailfauna.UnsealAtprotoIdentityBlob,
		resolver:         atprotoid.TXTResolverFromEnv(),
		httpClient:       &http.Client{Timeout: 30 * time.Second},
		directoryBaseURL: atprotoid.PLCDirectoryBaseURL(),
		pageLimit:        projPageLimit,
	}
}

// runProjectionLoop runs one pass immediately, then on each poll tick or nudge,
// until ctx is cancelled. It never returns an error: a pass failure is logged
// and retried on the next trigger (the poll backstop guarantees progress).
func runProjectionLoop(ctx context.Context, c wsrpc.Caller, deps *projDeps, nudge <-chan struct{}, logger *slog.Logger) {
	logger.Info("atproto projection loop starting (S3)", "poll_interval", projPollInterval.String())
	ticker := time.NewTicker(projPollInterval)
	defer ticker.Stop()
	for {
		runProjectionPass(ctx, c, deps, logger)
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		case <-nudge:
		}
	}
}

// runProjectionPass sweeps the roster once and projects every active identity.
// Roster-read failures are logged and left for the next trigger; per-identity
// failures never stop the sweep.
func runProjectionPass(ctx context.Context, c wsrpc.Caller, deps *projDeps, logger *slog.Logger) {
	identities, err := wsrpc.FetchAtprotoIdentities(ctx, c)
	if err != nil {
		logger.Warn("atproto projection: identity roster fetch failed (retrying next trigger)", "err", err)
		return
	}
	for _, id := range identities {
		if id.DID == nil || *id.DID == "" {
			continue // not yet minted — the mint loop owns it; no repo, no #account
		}
		did := *id.DID
		// A retired identity (S5 slice 5b) is inert here. Its repo was already
		// purged by the delete sweep this status requires, and its DID no longer
		// resolves — so there is nothing to serve, nothing to announce (a relay
		// cannot verify a frame from an identity it cannot resolve), and nothing
		// to project. Skipping explicitly rather than letting it fall through:
		// the fall-through happens to be harmless today only because
		// reconcileAccountStatus finds no repo, and a behaviour that depends on
		// another step having already run is one refactor away from announcing
		// #account(deactivated) for an identity that no longer exists.
		if id.Status == wsrpc.IdentityStatusTombstoned {
			continue
		}
		active := id.Status == wsrpc.IdentityStatusActive
		// The delete-presence sweep pre-empts the deactivation reconcile below
		// (S5 slice 5). A `deleted` identity is inactive too, so falling through
		// would announce #account(deactivated) for a presence that is about to
		// be destroyed — telling the network the wrong thing first, and telling
		// it twice. The sweep makes its own terminal announcement.
		if id.Status == wsrpc.IdentityStatusDeleted {
			if err := sweepDeletedIdentity(ctx, c, deps, id, logger); err != nil {
				logger.Warn("atproto delete-presence sweep failed (retrying next trigger)",
					"handle", id.Handle, "did", did, "err", err)
			}
			continue
		}
		// Reconcile the repo's served-status against the roster and announce a
		// transition on the firehose (#account) — both directions (S4-D). This
		// runs for DEACTIVATED identities too (that is the whole point), so it
		// sits BEFORE the projection skip below.
		if err := reconcileAccountStatus(ctx, deps, did, active, logger); err != nil {
			logger.Warn("atproto account-status reconcile failed (retrying next trigger)",
				"handle", id.Handle, "did", did, "err", err)
		}
		if !active {
			continue // deactivated — projection stops immediately (layer 2)
		}
		if err := projectIdentity(ctx, c, deps, id, logger); err != nil {
			logger.Warn("atproto projection failed for identity (retrying next trigger)",
				"handle", id.Handle, "did", did, "err", err)
		}
	}
}

// reconcileAccountStatus announces a hosting-status change on the firehose when
// the roster's active-ness diverges from what this bridge has last served and
// announced for did's repo (atproto-pds-bridge.md § Disable & revocation, layer
// 2 — "the repo is no longer served, an #account (deactivated) firehose event is
// emitted … re-enabling restores the same identity").
//
// It acts ONLY on a DID that already has a repo: a never-projected identity has
// nothing served to unserve and nothing the network has seen to announce
// inactive, so exists=false short-circuits. A freshly-projected repo's head is
// created active (repo_heads.active DEFAULT 1), so the first steady pass finds
// prev==desired and emits nothing — the account's initial activeness is implicit
// in its #commits, never a spurious #account(active=true).
//
// Crash-safety mirrors the rename hook: emit the #account frame FIRST (durable +
// seq-allocated in the outbox), then flip the stored flag — a crash between
// re-announces next pass (a duplicate #account is harmless to a relay) rather
// than dropping the announcement.
func reconcileAccountStatus(ctx context.Context, deps *projDeps, did string, active bool, logger *slog.Logger) error {
	prevActive, exists, err := deps.store.RepoActive(ctx, did)
	if err != nil {
		return fmt.Errorf("read repo active flag: %w", err)
	}
	if !exists || prevActive == active {
		return nil // no repo yet, or already in the announced state
	}
	status := ""
	if !active {
		status = atprotorepo.AccountStatusDeactivated
	}
	seq, err := deps.funnel.EmitAccount(ctx, did, active, status)
	if err != nil {
		return fmt.Errorf("emit #account: %w", err)
	}
	if err := deps.store.SetRepoActive(ctx, did, active); err != nil {
		return fmt.Errorf("record repo active flag: %w", err)
	}
	logger.Info("atproto #account emitted for hosting-status change",
		"did", did, "active", active, "seq", seq)
	return nil
}

// projectIdentity projects one active identity's posts + profile into its repo.
func projectIdentity(ctx context.Context, c wsrpc.Caller, deps *projDeps, id wsrpc.AtprotoIdentityView, logger *slog.Logger) error {
	did := *id.DID

	state, err := deps.store.ProjectionState(ctx, did)
	if err != nil {
		return fmt.Errorf("read projection state: %w", err)
	}

	// Pre-firehose gate: until this user's identity resolves ecosystem-side,
	// refuse to project anything — a DID that does not resolve at first AppView
	// index can permanently 404. The mint loop's warn-only check surfaced this
	// earlier; here it is load-bearing. Once resolvable, clear the gate right
	// away: resolvability is monotonic, so re-checking every pass is waste, and
	// clearing before the emit (rather than after) leaves no crash-window where
	// a committed-but-ungated repo re-runs the check forever.
	if state.FirstEmitGated {
		if verr := atprotoid.VerifyIdentityResolvable(
			ctx, deps.resolver, deps.httpClient, deps.directoryBaseURL, did, id.Handle,
		); verr != nil {
			logger.Warn("atproto projection: first-emit gated — identity not (yet) resolvable, deferring",
				"handle", id.Handle, "did", did, "err", verr)
			return nil
		}
		if err := deps.store.SetFirstEmitGated(ctx, did, false); err != nil {
			return fmt.Errorf("clear first-emit gate: %w", err)
		}
		logger.Info("atproto projection: identity resolvable — first-emit gate cleared",
			"handle", id.Handle, "did", did)
	}

	// Rename hook: converge the published handle onto the one nest derives now,
	// before any commit of this pass. A failure here must not block projection
	// — a stale handle is a resolution problem, a stalled repo is a data one.
	if err := reconcileHandle(ctx, c, deps, id, state, logger); err != nil {
		logger.Warn("atproto rename hook failed (retrying next trigger)",
			"handle", id.Handle, "did", did, "err", err)
	}

	signer, err := unsealRepoSigner(ctx, c, deps, id)
	if err != nil {
		return fmt.Errorf("unseal repo signer: %w", err)
	}

	// Signature half of the boot self-check, once per DID per process: does the
	// repo we are serving still verify under the key this identity signs with?
	// A mismatch means a re-minted identity left a repo the relay will reject —
	// invisible until then, so say it loudly here. Not fatal: the pass proceeds,
	// and the next commit re-signs the head with the current key.
	//
	// An account with NO repo yet is skipped WITHOUT being marked checked: there
	// is nothing to verify, so VerifyRepo's "no repo for did" is not a failed
	// self-check but an empty one, and shouting it would be a false alarm on the
	// ordinary path — the first-emit gate clears before the first commit, so the
	// pass that opens it reaches here with an empty repo BY DESIGN. Leaving the
	// DID unmarked is what makes the check run for real on the first pass that
	// has a head to check, instead of being consumed by the empty one.
	if _, hasRepo, rerr := deps.store.RepoActive(ctx, did); rerr != nil {
		logger.Warn("atproto self-check skipped: cannot read repo presence",
			"did", did, "err", rerr)
	} else if hasRepo {
		if _, done := deps.selfChecked.LoadOrStore(did, true); !done {
			pub, perr := signer.PublicKey()
			switch {
			case perr != nil:
				logger.Warn("atproto self-check skipped: cannot derive public key from signer",
					"did", did, "err", perr)
			default:
				if verr := deps.store.VerifyRepo(ctx, did, pub); verr != nil {
					logger.Error("atproto self-check FAILED: repo head does not verify under the current signing key",
						"did", did, "handle", id.Handle, "err", verr)
				}
			}
		}
	}

	// A #sync debt outstanding from an earlier pass forces collapse mode for this
	// whole pass, however small the remaining gap: that pass moved the head with
	// its frames deferred and died before announcing it, so the network is behind
	// and an ordinary #commit now would carry a Sync v1.1 `prevData` naming an MST
	// root no consumer has. Announce the head first, replay incrementally after.
	collapse, err := deps.store.SyncOwed(ctx, did)
	if err != nil {
		return fmt.Errorf("read owed #sync: %w", err)
	}

	// Project the public-post stream, paging from the stored cursor. Each page's
	// commits persist individually (per-commit funnel txns), so the watermark is
	// advanced per page — a crash re-fetches only the unpersisted tail, which
	// ProjectItems skips idempotently via post_map.
	cursor := cursorFromState(state)
	totalApplied := 0
	firstPage := true
	for {
		items, next, ferr := wsrpc.FetchPublicPosts(ctx, c, id.ActorID, cursor, deps.pageLimit)
		if ferr != nil {
			return fmt.Errorf("fetch public posts: %w", ferr)
		}
		// The downtime-catch-up decision (atproto-pds-bridge.md § Projection &
		// backfill, watermark row 3): a backlog that does not fit in ONE fetch
		// page is the "huge gap" whose replay collapses to a single #sync. Taken
		// from the FIRST page's next_cursor, so it is made before any frame is
		// emitted — a collapsing pass never spends frames it is about to make
		// redundant, and an ordinary catch-up (the overwhelming case) still
		// replays as individual #commits, which is strictly better for a relay
		// than provoking a whole getRepo.
		if firstPage && next != nil {
			collapse = true
			logger.Info("atproto projection: backlog exceeds one page — collapsing the catch-up to one #sync",
				"handle", id.Handle, "did", did, "page_limit", deps.pageLimit)
		}
		firstPage = false
		if len(items) > 0 {
			applied, perr := deps.projector.ProjectItems(ctx, did, signer, toProjectionItems(items), frameOptions(collapse)...)
			totalApplied += applied
			if perr != nil {
				// Stop this user's pass here; do NOT advance past the failing
				// item — next trigger retries from the persisted watermark, and
				// ProjectItems skips the already-applied prefix. (S5 hardening
				// owns dead-lettering a genuinely poison item.)
				return fmt.Errorf("project posts: %w", perr)
			}
			last := items[len(items)-1]
			if werr := deps.store.SetProjectionWatermark(ctx, did, last.CreatedAtMicros, last.PostID); werr != nil {
				return fmt.Errorf("advance watermark: %w", werr)
			}
		}
		if next == nil {
			break
		}
		cursor = next
	}

	// Project the profile (create-or-update at rkey "self"). A profile failure
	// must not undo the posts already committed above; it is retried next pass.
	profileBytes, err := wsrpc.FetchProfile(ctx, c, id.ActorID)
	if err != nil {
		return fmt.Errorf("fetch profile: %w", err)
	}
	profileApplied, err := deps.projector.ProjectProfile(ctx, did, signer, profileBytes, frameOptions(collapse)...)
	if err != nil {
		return fmt.Errorf("project profile: %w", err)
	}
	if profileApplied {
		totalApplied++
	}
	if totalApplied > 0 {
		logger.Info("atproto projection: applied commits for identity",
			"handle", id.Handle, "did", did, "commits", totalApplied)
	}

	// Pay off the collapse. Read the debt back from the store rather than from
	// `collapse`: the flag is the durable fact (a pass may be here to settle an
	// earlier one's debt without having deferred anything itself), and a pass
	// that forced collapse mode but found nothing to commit owes nothing new.
	owed, err := deps.store.SyncOwed(ctx, did)
	if err != nil {
		return fmt.Errorf("read owed #sync: %w", err)
	}
	if owed {
		seq, serr := deps.funnel.EmitSync(ctx, did)
		if serr != nil {
			return fmt.Errorf("emit #sync: %w", serr)
		}
		logger.Info("atproto projection: catch-up collapsed — #sync announced the head",
			"handle", id.Handle, "did", did, "seq", seq)
	}
	return nil
}

// frameOptions maps the pass's collapse decision to the funnel options each
// commit is applied with: deferred frames while collapsing, the ordinary
// per-commit #commit otherwise.
func frameOptions(collapse bool) []atprotorepo.BatchOption {
	if !collapse {
		return nil
	}
	return []atprotorepo.BatchOption{atprotorepo.DeferFrameToSync()}
}

// reconcileHandle converges a user's PUBLISHED ATProto handle onto the one nest
// derives right now, and announces the change to the network — the rename hook
// (atproto-pds-bridge.md § Identity: "A Fauna handle or domain change
// re-derives, republishes the _atproto TXT, updates the DID document's
// alsoKnownAs, and emits an #identity firehose event so the network
// re-resolves").
//
// It is a CONVERGENT reconcile rather than a hook carrying the new handle,
// because the roster row already carries the current handle (nest derives it at
// read time, never stores it) and BOTH causes collapse into that one value: a
// per-user handle change, and a deployment primary-domain change that
// re-derives every identity's handle at once. Nest's pushes only shorten the
// latency; the poll backstop is what makes a rename impossible to lose.
//
// The steady state costs nothing — an unchanged handle returns before any
// network call.
func reconcileHandle(
	ctx context.Context,
	c wsrpc.Caller,
	deps *projDeps,
	id wsrpc.AtprotoIdentityView,
	state atprotorepo.ProjectionCursor,
	logger *slog.Logger,
) error {
	did := *id.DID
	if state.PublishedHandle == id.Handle {
		return nil
	}

	// did:web's DID *is* its handle, so a rename would be a different identity,
	// not an update to this one — there is nothing to submit and nothing true to
	// announce. Surface the limit instead of faking it (the § Identity caveat),
	// and leave published_handle alone so the divergence stays observable rather
	// than being silently absorbed.
	if id.Method != "plc" {
		if _, warned := deps.didWebRenameWarned.LoadOrStore(did, true); !warned {
			logger.Error("atproto handle changed but this identity's DID method cannot follow it — the account is now unresolvable under its new handle",
				"did", did, "method", id.Method,
				"published_handle", state.PublishedHandle, "current_handle", id.Handle)
		}
		return nil
	}

	// The directory's own log is the source of truth for both halves: the state
	// the next op must carry forward, and the prev CID that chains to it.
	last, prevCID, err := atprotoid.FetchLastOp(ctx, deps.httpClient, deps.directoryBaseURL, did)
	if err != nil {
		return fmt.Errorf("read plc log: %w", err)
	}

	if atprotoid.PrimaryHandle(last) != id.Handle {
		// Do not move the DID document onto a handle that does not verify yet:
		// bidirectional verification needs `_atproto.<new>` published, and the
		// client's DNS reconcile publishes it from the same derived handle. If it
		// has not landed, defer — the old document keeps resolving meanwhile,
		// which is the better failure direction than a document pointing at a
		// handle nothing confirms.
		if verr := atprotoid.VerifyIdentityResolvable(
			ctx, deps.resolver, deps.httpClient, deps.directoryBaseURL, did, id.Handle,
		); verr != nil {
			logger.Info("atproto rename deferred — the new handle does not verify yet",
				"did", did, "new_handle", id.Handle, "err", verr)
			return nil
		}
		rotationKey, rerr := unsealRotationKey(ctx, c, deps, id)
		if rerr != nil {
			return fmt.Errorf("unseal rotation key: %w", rerr)
		}
		op := atprotoid.BuildUpdateOpFromPrev(last, prevCID, id.Handle)
		if serr := op.Sign(rotationKey); serr != nil {
			return fmt.Errorf("sign plc update op: %w", serr)
		}
		if serr := atprotoid.SubmitOperation(ctx, deps.httpClient, deps.directoryBaseURL, did, op); serr != nil {
			return fmt.Errorf("submit plc update op: %w", serr)
		}
		logger.Info("atproto identity handle republished at the PLC directory",
			"did", did, "old_handle", atprotoid.PrimaryHandle(last), "new_handle", id.Handle)
	}

	// A non-empty stale value is positive evidence a rename happened and this
	// bridge still owes the announcement — including the crash-after-submit
	// case, where the directory already agrees but the frame never went out.
	// Empty means "never observed" (fresh mint, or a wiped store): converge
	// quietly rather than announce a rename there is no evidence of.
	if state.PublishedHandle != "" {
		seq, eerr := deps.funnel.EmitIdentity(ctx, did, id.Handle)
		if eerr != nil {
			return fmt.Errorf("emit #identity: %w", eerr)
		}
		logger.Info("atproto #identity emitted for handle change",
			"did", did, "old_handle", state.PublishedHandle, "new_handle", id.Handle, "seq", seq)
	}
	// Written last on purpose: a crash before this leaves the stale value that
	// tells the next pass it still owes the frame.
	if err := deps.store.SetPublishedHandle(ctx, did, id.Handle); err != nil {
		return fmt.Errorf("record published handle: %w", err)
	}
	return nil
}

// unsealRotationKey fetches + unseals the identity key blob and returns the
// bridge-custodied JUNIOR rotation key (K-256) — the key a PLC update op is
// signed with. Junior by design: the user's client holds the senior key, so it
// can nullify anything this bridge submits inside PLC's contest window
// (atproto-pds-bridge.md § State & data shape, key-custody split).
func unsealRotationKey(ctx context.Context, c wsrpc.Caller, deps *projDeps, id wsrpc.AtprotoIdentityView) (atcrypto.PrivateKey, error) {
	return rotationKeyForActor(ctx, c, deps.unseal, deps.x25519Secret, id.ActorID)
}

// rotationKeyForActor is unsealRotationKey's dependency-free half — the fetch +
// unseal + curve check + seal-drift self-check, taking only the two seams it
// actually uses. Split out to mirror unsealRepoSigner/repoSignerForActor below,
// so a caller that has no projection state (it needs a rotation key, not a
// projector) does not have to invent a half-nil projDeps to get one.
func rotationKeyForActor(
	ctx context.Context,
	c wsrpc.Caller,
	unseal unsealIdentityBlobFn,
	x25519Secret []byte,
	actorID []byte,
) (atcrypto.PrivateKey, error) {
	bundle, err := openIdentityKeys(ctx, c, unseal, x25519Secret, actorID)
	if err != nil {
		return nil, err
	}
	defer zeroize(bundle.SigningPriv)
	defer zeroize(bundle.RotationPriv)
	if bundle.RotationCurve != "k256" {
		return nil, fmt.Errorf("unsupported rotation curve %q (want k256)", bundle.RotationCurve)
	}
	key, err := atprotoid.PrivateKeyFromK256Scalar(bundle.RotationPriv)
	if err != nil {
		return nil, fmt.Errorf("bridge rotation key from scalar: %w", err)
	}
	// Same seal-drift self-check the mint path runs: signing with a scalar that
	// is not the pubkey the directory has listed produces an op no one accepts.
	derived, err := atprotoid.DIDKeyForPrivate(key)
	if err != nil {
		return nil, fmt.Errorf("derive rotation did:key: %w", err)
	}
	if bundle.RotationPubDidKey != "" && derived != bundle.RotationPubDidKey {
		return nil, fmt.Errorf("unsealed rotation scalar derives %s but nest recorded %s (seal drift)",
			derived, bundle.RotationPubDidKey)
	}
	return key, nil
}

// unsealRepoSigner returns the repo-commit signing key (K-256) for one
// projected identity. The fetch + unseal + zeroize live in repoSignerForActor
// (serviceauth_signers.go), shared with F3's service-auth mint — the two sign
// with the same key, so they must never drift on the curve check or the
// short-lifetime discipline.
func unsealRepoSigner(ctx context.Context, c wsrpc.Caller, deps *projDeps, id wsrpc.AtprotoIdentityView) (atprotorepo.Signer, error) {
	return repoSignerForActor(ctx, c, deps.unseal, deps.x25519Secret, id.ActorID)
}

// cursorFromState maps a stored projection watermark to a fetch cursor. The zero
// watermark (a never-projected DID) maps to nil — page from the beginning.
func cursorFromState(state atprotorepo.ProjectionCursor) *wsrpc.PublicPostsCursor {
	if state.LastPostID == "" && state.LastCreatedAtMicros == 0 {
		return nil
	}
	return &wsrpc.PublicPostsCursor{
		CreatedAtMicros: state.LastCreatedAtMicros,
		PostID:          state.LastPostID,
	}
}

// toProjectionItems decodes the wire post items into the projector's item type.
func toProjectionItems(items []wsrpc.PublicPostItem) []atprotorepo.ProjectionItem {
	out := make([]atprotorepo.ProjectionItem, 0, len(items))
	for _, it := range items {
		pi := atprotorepo.ProjectionItem{
			PostID:          it.PostID,
			CreatedAtMicros: it.CreatedAtMicros,
			Kind:            atprotorepo.ItemKind(it.Kind),
			Payload:         it.Payload,
		}
		if it.DeletedPostID != nil {
			pi.DeletedPostID = *it.DeletedPostID
		}
		out = append(out, pi)
	}
	return out
}
