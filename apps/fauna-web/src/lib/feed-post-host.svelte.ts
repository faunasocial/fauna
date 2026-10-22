// The feed card's host — everything a `post-card` needs from the page that
// paints it, over ONE feed manager (`feed.md` § Where logic lives; priority #2).
//
// Two pages paint the feed's own card: the Feed page over the timeline manager,
// and the profile's Posts tab over an AUTHOR WINDOW of it
// (`WasmFeedManager.authorWindow`, `feed.md` § State & data shape → *The author
// window*; `profile.md` § Layout & flow → *The Posts tab*). The card cannot
// reach a manager, so its page resolves media, folds the session-local reveal
// sets and the content-policy verdict into props, and dispatches its verbs —
// like / repost / quote, the reply dialog's submit, delete, the web-publishing
// verbs, the buy, the training verbs. This module is that wiring, built once
// per (page, manager) and handed to `FeedPostCard`, `FeedReplyDialog` and
// `FeedTrainSheet`, so a verb on the profile acts on the window — its counts
// move — exactly as the same verb on the feed acts on the timeline. The tui
// twin is `feed::post_card_rows` / `feed::post_overlays` / `feed::apply_on`
// over the manager a row came from (`apps/fauna-tui/src/profile/posts.rs`).
//
// The refresh contract (`feed-refresh-contract.test.ts`) and the manager-gate
// contract (`manager-gate-contract.test.ts`) read this file as they read the
// feed page: every function that awaits the manager re-reads the snapshot
// (`refreshFeed`, the page's own re-read), and a user action refused on a null
// manager says so on the page's error surface.

import { get } from 'svelte/store';
import { identity } from './store';
import { postBlobGet } from './rpc';
import { getPost, uploadBlobMultipart, fetchBlob, fetchNestPath, nodeUrl } from './api';
import {
  decodePostFetchedAs,
  postBlobHome,
  logMessage,
  contentLabelBadgeFor,
  feedRefusalI18nKey,
  reportPostTarget,
  webPostPageUrl,
  webTokenedUrl,
  type ContentLabelEntry,
  type ContentRender,
  type ReportTarget,
  type ItemPlaceholder as RegionPlaceholderValue,
} from './wasm';
import { paintableDecodedPost } from './feed-utils';
import { quotedPostBlock, resolvingLinkPreviewUrls } from './document';
import { contentRender, guardianReasonOf } from './contentPolicy.svelte';
import { trainedTopicsList, webPaywallMintToken, webPublishSet, webPublishUnset, type TrainedTopicRow } from './rpc';
import { bestEffortCopyToClipboard, siteLink } from './web-publish';
import { hexToBytes } from './hex';
import { toArrayBufferView } from './bytes';
import { isSafeNavUrl } from './safe-url';
import { verbErrorCopy as verbErrorCopyOf } from './feed-verb-error';
import { contactOverlays } from './conversations';
import { t } from './i18n/strings';
import type { WasmFeedManager } from '../../static/fauna_wasm.js';

/** What a host reads off the page that owns it. */
export interface FeedPostHostDeps {
  /** The manager the cards' rows came from, or `null` while it builds. */
  manager: () => WasmFeedManager | null;
  /** The rows on screen — a reactive read (the page's derived snapshot). */
  posts: () => any[]; // eslint-disable-line @typescript-eslint/no-explicit-any
  /** The rows as the manager holds them right after a re-read — a synchronous
   *  read, for a resolve's "did it land?" check after `refreshFeed()`. */
  latestPosts: () => any[]; // eslint-disable-line @typescript-eslint/no-explicit-any
  /** Re-read the manager's snapshot into the page (web has no observer seam
   *  for the page's own read loop — every awaited manager call re-reads). */
  refreshFeed: () => void;
  /** Paint (or, with `''`, clear) the page's `error-message`. */
  setError: (message: string) => void;
  /** Whether the page has been destroyed (late resolves then drop). */
  destroyed: () => boolean;
}

export function blobUrl(hash: string): string {
  return `${nodeUrl()}/api/v1/blob/${hash}`;
}

/** A post author's name through the private overlay's one resolver — the
 *  viewer's nickname, else a bridged author's display name / handle, else the
 *  short id (contacts.md § The private overlay → *Where the nickname paints*).
 *  Read inside a `$derived` that touches `$contactsRevision`. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function authorName(p: any): string {
  return contactOverlays().peerLabel(
    p.author_display?.display_name ?? null,
    p.author_display?.handle ?? null,
    p.author,
  ).primary;
}

/** This post's badge label, `"category:confidence"` (the `ContentLabelBadge`
 *  wire format) — the highest-confidence entry of `PostSummary.labels`
 *  (`moderation.md` § Per-row badge data path). `undefined` when unlabelled. */
export function contentLabelFor(post: { labels?: ContentLabelEntry[] }): string | undefined {
  return contentLabelBadgeFor(post.labels);
}

export type FeedPostHost = ReturnType<typeof createFeedPostHost>;

/** Build the card host over one manager. Call from a component's `<script>`
 *  (it owns `$state`). */
export function createFeedPostHost(deps: FeedPostHostDeps) {
  const LOG = 'fauna_web::feed';
  const refreshFeed = (): void => deps.refreshFeed();

  // ── Post media (media.md § Encryption at rest — one per-post key seals a
  //    restricted post's body and all its attachments together) ──────────────
  //
  // A public post's blob is plaintext on the wire, so `<img src>` IS the render:
  // the browser fetches and decodes it natively, and only that path can use the
  // nest's `?thumb=1` smaller blob. A tier-restricted post's attachment is
  // AEAD-sealed under the same per-post key its body opened under, so no URL can
  // ever render it — the bytes have to come here, through the manager, and go
  // back out as an object URL.
  //
  // Which of the two is NOT a judgment this module makes: `isSealedMedia` is the
  // shared manager's own answer (it holds the keys), so web and apple — the two
  // apps whose image path is a URL rather than bytes — ask instead of guessing.
  // The other five route every hash through `openMediaBytes` unconditionally.
  // Either way the post card just paints the string it is handed.
  //
  // Cache contract is the conversations-attachment one: a miss returns `null` and
  // is NOT cached, so the next render retries; the fetch is kicked off once per
  // hash and repaints via `$state` when it lands. Object URLs are revoked on
  // `destroy` so a reactive re-render doesn't leak one per tick.
  let sealedMediaUrls = $state<Record<string, string>>({});
  const sealedMediaPending = new Map<string, Promise<string | null>>();

  // A foreign-homed post's own bytes (`PostSummary.blob_home`, the shared pick
  // via `postBlobHome`) come DIRECT from the home they rest on through the
  // shared post-blob plane, the home handed back unchanged; every other
  // post's bytes keep the session-bearer read of this nest. A home that
  // serves a self-signed certificate is unreachable from a browser (no pin
  // in a browser — `transport-trust.md` § The post arm → *The browser's
  // leg*): the read rejects and the item stays blank, never substituted.
  function foreignHome(post: unknown): unknown | null {
    if (!post) return null;
    try {
      return postBlobHome(post as { home_nest_url?: string | null; home_nest_actor_id?: string | null });
    } catch {
      return null;
    }
  }
  async function readPostBlob(home: unknown | null, hash: string): Promise<Uint8Array> {
    const id = get(identity);
    if (!id) throw new Error('no identity');
    return home ? postBlobGet(id.secretHex, home, hash) : fetchBlob(id.secretHex, hash);
  }

  // Fetch + open one sealed item into an object URL — the one opener a sealed
  // post image (`mediaUrl`) and a sealed post video (`playbackUrl`) share. A
  // concurrent ask for the same hash joins the fetch already in flight.
  function openSealedMedia(hash: string, home: unknown | null = null, sealed = true): Promise<string | null> {
    const cached = sealedMediaUrls[hash];
    if (cached) return Promise.resolve(cached);
    let pending = sealedMediaPending.get(hash);
    if (!pending) {
      pending = (async () => {
        try {
          const id = get(identity);
          if (!id) return null;
          const bytes = await readPostBlob(home, hash);
          const opened = sealed ? deps.manager()?.openMediaBytes(hash, bytes) : bytes;
          // `undefined` means the item did not open — leave it uncached and paint
          // nothing, exactly as for bytes the browser cannot decode. Never hand
          // AEAD ciphertext to an <img> or a <video>.
          if (opened && opened.length > 0) {
            const url = URL.createObjectURL(new Blob([toArrayBufferView(opened)]));
            sealedMediaUrls[hash] = url;
            return url;
          }
          return null;
        } catch (e) {
          logMessage('warn', LOG, `sealed post media (card stays blank): ${e}`);
          return null;
        } finally {
          sealedMediaPending.delete(hash);
        }
      })();
      sealedMediaPending.set(hash, pending);
    }
    return pending;
  }

  function mediaUrl(hash: string, post?: unknown): string | null {
    let isSealed = false;
    try {
      isSealed = deps.manager()?.isSealedMedia(hash) ?? false;
    } catch {
      isSealed = false;
    }
    // A foreign-homed post's image is bytes from its home, never a URL on
    // this nest: an object URL once they land (a plain item needs no open).
    const home = foreignHome(post);
    if (home) {
      const got = sealedMediaUrls[hash];
      if (got) return got;
      void openSealedMedia(hash, home, isSealed);
      return null;
    }
    // The common case, and every case before a gated post is unlocked: the plain
    // blob URL, thumbnail variant and all.
    if (!isSealed) return blobUrl(hash);

    const cached = sealedMediaUrls[hash];
    if (cached) return cached;
    void openSealedMedia(hash);
    return null;
  }

  // A bridged post's `ProxiedImage` (render-model.md § D6c): its nest-relative path fetched
  // with the session bearer (`fetchNestPath`, the blob loader's own call with a different
  // argument) into an object URL, cached by path — a subresource `<img src>` cannot carry the
  // bearer. The proxy's answer is plaintext, so there is no open step. `null` while in flight or
  // after a failure (a failure is NOT cached, so the next render retries, as `mediaUrl` does).
  let proxiedMediaUrls = $state<Record<string, string>>({});
  const proxiedMediaPending = new Set<string>();

  function proxiedMediaUrl(path: string): string | null {
    const cached = proxiedMediaUrls[path];
    if (cached) return cached;
    const id = get(identity);
    if (!id || proxiedMediaPending.has(path)) return null;
    proxiedMediaPending.add(path);
    void (async () => {
      try {
        const bytes = await fetchNestPath(id.secretHex, path);
        if (bytes.length > 0 && !deps.destroyed()) {
          proxiedMediaUrls[path] = URL.createObjectURL(new Blob([toArrayBufferView(bytes)]));
        }
      } catch (e) {
        logMessage('warn', LOG, `proxied post media (placeholder stays): ${e}`);
      } finally {
        proxiedMediaPending.delete(path);
      }
    })();
    return null;
  }

  // What a tapped `video-thumbnail` plays (render-model.md § D6c → *Inline
  // playback*). The decision is the shared manager's `playbackSource`; this
  // module only turns its answer into a `<video src>`: a nest-relative URL gets
  // the nest origin, a sealed item goes through the same opener a sealed image
  // does. `null` = nothing playable — the player's native error UI is the surface.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async function playbackUrl(block: any, post?: unknown): Promise<string | null> {
    const manager = deps.manager();
    if (!manager) { deps.setError(t.common.still_loading); return null; }
    try {
      const source = await manager.playbackSource(block);
      const home = foreignHome(post);
      if (home && source && 'Url' in source) {
        const hash = block?.Video?.hash;
        return hash ? await openSealedMedia(hash, home, false) : null;
      }
      if (source && 'Url' in source) return `${nodeUrl()}${source.Url.url}`;
      if (source && 'Sealed' in source) return await openSealedMedia(source.Sealed.hash, home);
      logMessage('warn', LOG, `video not playable: ${JSON.stringify(source)}`);
      return null;
    } finally {
      refreshFeed();
    }
  }

  // ── Muted-keyword collapse + the content-policy render. Both ride the shared
  //    FeedManager / render state; the card cannot reach them, so the host reads
  //    them per post and folds in its session-local reveal sets. ──

  /** Posts the user un-collapsed this session. Session-local RENDER state — the
   *  term stays muted (un-muting the word is what stops future collapse). */
  let revealedMuted = $state<Set<string>>(new Set());

  function isPostMuted(postId: string): boolean {
    if (revealedMuted.has(postId)) return false;
    try {
      return deps.manager()?.isMuted(postId) ?? false;
    } catch {
      return false;
    }
  }

  function revealMuted(postId: string): void {
    // Copy-on-write: Svelte tracks the reassignment, not an in-place mutation.
    const next = new Set(revealedMuted);
    next.add(postId);
    revealedMuted = next;
  }

  /** Posts the user un-collapsed past a content-policy `collapse` floor this
   *  session — the separate reveal set the content pillar keeps (distinct from
   *  `revealedMuted`). A `block` floor is never revealable. */
  let revealedContent = $state<Set<string>>(new Set());
  function revealContent(postId: string): void {
    const next = new Set(revealedContent);
    next.add(postId);
    revealedContent = next;
  }
  // The shared content-policy render for a post — the region, the guardian
  // floor and the viewer's own thresholds composed strictest-wins in Rust
  // (`contentRender`). A REGION placeholder paints ahead of every other arm
  // (tui's order); `block` then paints the family notice ahead of the muted
  // arm; `collapse` collapses with a session-local reveal (one reveal set for
  // the region and the family collapse).
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function postRender(post: any): ContentRender {
    return contentRender(post.labels, {
      contentIdHex: post.post_id,
      authorHex: post.author,
      text: post.body ?? '',
      hashtags: post.tags ?? [],
      hasMedia: !!post.has_media,
    });
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function regionPlaceholderFor(post: any): RegionPlaceholderValue | null {
    const placeholder = postRender(post).placeholder;
    if (!placeholder) return null;
    return placeholder.verb === 'block' || !revealedContent.has(post.post_id) ? placeholder : null;
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function isContentBlocked(post: any): boolean {
    return postRender(post).verdict === 'block';
  }
  // The guardian rule's reason, painted verbatim beside the family-policy
  // frame (`content-policy-blocked-reason`); `null` when none drove it.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function guardianReasonFor(post: any): string | null {
    return guardianReasonOf(postRender(post));
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function isContentReported(post: any): boolean {
    return postRender(post).reported === true;
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function isContentCollapsed(post: any): boolean {
    return !revealedContent.has(post.post_id) && postRender(post).verdict === 'collapse';
  }

  /** The report sheet's target for one of this host's rows (`report-sheet`,
   *  opened by `feed-post-report-button`); `null` for a row not on screen. */
  function reportTargetFor(postId: string): ReportTarget | null {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const post = deps.posts().find((p: any) => p.post_id === postId);
    if (!post) return null;
    return reportPostTarget(
      post.post_id,
      post.author,
      post.body ?? '',
      !!(post.gated_tier || post.gated_room),
    );
  }

  // ── Trained-topic training verbs (topic-factors.md § Training signals). ──

  /** The manager's single in-context trained topic, or null (then the verbs
   *  open the target sheet). Not reactive itself: the page re-reads it off its
   *  snapshot. */
  function trainTargetFactor(): string | null {
    try {
      return deps.manager()?.trainTargetFactor() ?? null;
    } catch {
      return null;
    }
  }

  /** This post's example marker for the in-context factor ('more'/'less'/null). */
  function trainMarkerFor(postId: string, target: string | null): string | null {
    if (!target) return null;
    try {
      return deps.manager()?.exampleLabelFor(postId, target) ?? null;
    } catch {
      return null;
    }
  }

  /** The post awaiting a factor choice, when no in-context factor resolved —
   *  `FeedTrainSheet` paints it. */
  const trainSheet = $state<{
    target: { post_id: string; verb: 'more' | 'less' } | null;
    topics: TrainedTopicRow[];
  }>({ target: null, topics: [] });

  async function trainPost(postId: string, verb: 'more' | 'less', target: string | null): Promise<void> {
    if (!target) {
      // No dominant trained factor — ask which topic to train (the sheet).
      const id = get(identity);
      if (!id) return;
      try {
        trainSheet.topics = (await trainedTopicsList(id.secretHex)).filter((r) => r.factor_key);
      } catch {
        trainSheet.topics = [];
      }
      trainSheet.target = { post_id: postId, verb };
      return;
    }
    await trainInto(postId, verb, target);
  }

  /** Toggle-shaped: tapping the marked verb again untrains (the manager applies
   *  the exact inverse delta); flipping verbs re-trains. */
  async function trainInto(postId: string, verb: 'more' | 'less', factor: string): Promise<void> {
    const manager = deps.manager();
    try {
      if (manager?.exampleLabelFor(postId, factor) === verb) {
        await manager?.untrainPost(postId, factor);
      } else {
        await manager?.trainPost(postId, factor, verb);
      }
    } catch (e: unknown) {
      logMessage('warn', LOG, `train failed: ${e}`);
    } finally {
      refreshFeed();
    }
  }

  // ── Reply compose dialog (feed.md § Implementation status today: reply is
  //    COMPOSED via FeedManager::reply, never interact — its native arm
  //    discards body). `FeedReplyDialog` paints it over this host. ──
  const reply = $state<{ targetId: string | null; text: string; publicConfirm: boolean }>({
    targetId: null,
    text: '',
    /** The dialog's explicit-public answer (`feed-reply-public-confirm`) —
     *  unchecked at every open, never remembered (restricted-posts.md
     *  § Encryption at rest → *Ruling 5's build — the shape*, (e)). */
    publicConfirm: false,
  });

  /** The reply target's row — its manager-derived audience (`reply_audience`,
   *  `Some` exactly under a restricted post) and its card fields; the dialog
   *  states them and never decides them. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function replyTarget(): any | null {
    const id = reply.targetId;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return id ? (deps.posts().find((p: any) => p.post_id === id) ?? null) : null;
  }

  /** A rejected feed verb as `error-message` paints it (`#lib/feed-verb-error`):
   *  a stated refusal reads in the user's language through the shared
   *  `refusal_i18n_key`; every other failure keeps its own text. */
  function verbErrorCopy(e: unknown): string {
    return verbErrorCopyOf(e, feedRefusalI18nKey);
  }

  function closeReplyDialog(): void {
    reply.targetId = null;
    reply.text = '';
    reply.publicConfirm = false;
  }

  async function submitReply(): Promise<void> {
    // The reply button gates on the typed text alone — never on manager
    // readiness — so this handler is genuinely reachable with a null manager,
    // and a bare return would discard the typed reply with no feedback at all
    // (web.md § Async manager readiness).
    const manager = deps.manager();
    if (!manager) { deps.setError(t.common.still_loading); return; }
    if (!reply.targetId) return;
    const text = reply.text.trim();
    if (!text) return;
    const postId = reply.targetId;
    const publicConfirmed =
      reply.publicConfirm && replyTarget()?.reply_audience === 'PublicByConfirmation';
    closeReplyDialog();
    try {
      if (publicConfirmed) {
        // The one door that composes words public under a restricted target.
        await manager.replyPublicConfirmed(postId, text);
      } else {
        // The composer's fall-through, reused (restricted-posts.md (c)):
        // prepare answers the sealed body or nothing to upload; bytes go
        // through the same upload + submitGatedPost/abortGatedSubmit pair as
        // the composer's gated post, and null falls through to `reply`.
        const sealed: Uint8Array | null = await manager.prepareSealedReply(postId, text);
        if (sealed) {
          const id = get(identity);
          try {
            if (!id) throw new Error('no identity for a sealed reply');
            const sidecar = manager.gatedUploadSidecar();
            const hash = await uploadBlobMultipart(id.secretHex, sidecar, sealed);
            await manager.submitGatedPost(hash);
          } catch (e) {
            manager.abortGatedSubmit(String(e));
            throw e;
          }
        } else {
          await manager.reply(postId, text);
        }
      }
      deps.setError('');
    } catch (e) {
      logMessage('warn', LOG, `reply error: ${e}`);
      deps.setError(verbErrorCopy(e));
    } finally {
      refreshFeed();
    }
  }

  // ── The interaction bar (feed.md § User actions). `reply` opens the compose
  //    dialog instead of firing immediately (`manager.reply` needs typed text).
  async function handleInteract(postId: string, action: string): Promise<void> {
    const manager = deps.manager();
    if (!manager) { deps.setError(t.common.still_loading); return; }
    if (action === 'reply') {
      reply.targetId = postId;
      reply.text = '';
      return;
    }
    try {
      // Two verbs, not one (feed.md § User actions). `like` is the manager's
      // TOGGLE off `viewer_liked`: both directions ride the same interact door
      // on the same post id, so nothing is composed — but the bare `interact`
      // call below can only ever like, never un-like, because the nest's like
      // arm is idempotent per (actor, post). `quote` COMPOSES a post carrying
      // `Reference::Quote`; routed through `interact` it creates nothing at all
      // on a native post.
      if (action === 'like') {
        await manager.like(postId);
      } else if (action === 'quote') {
        await manager.quote(postId, '');
      } else if (action === 'repost') {
        // `repost` is the manager's TOGGLE off `viewer_repost_id` (feed.md
        // § Interaction bar → Repost, ratified 2026-08-10).
        await manager.repost(postId);
      } else {
        await manager.interact(postId, action, null);
      }
      deps.setError('');
    } catch (e) {
      logMessage('warn', LOG, `interact error: ${e}`);
      deps.setError(verbErrorCopy(e));
    } finally {
      refreshFeed();
    }
  }

  // ── Gated post unlock (feed.md § Encryption at rest; monetization.md § Pillars
  //    2+3): resolve the sealed blob hash, fetch its bytes on the bulk plane, and
  //    hand them to the manager to decrypt + swap the full body into the
  //    snapshot. Best-effort — a locked post just keeps its teaser. ──
  async function unlockGated(postId: string): Promise<void> {
    const manager = deps.manager();
    const id = get(identity);
    if (!manager || !id) return; // manager-gate-ok: best-effort detail augmentation — the post keeps its teaser
    try {
      const hash = await manager.gatedBlobHash(postId);
      if (!hash) return;
      const snap = manager.snapshot();
      const post =
        (snap.posts ?? []).find((p: any) => p.post_id === postId) ??
        (snap.deep_linked_post?.post_id === postId ? snap.deep_linked_post : null);
      const bytes = await readPostBlob(foreignHome(post), hash);
      await manager.unlockGatedPost(postId, bytes);
    } catch (e) {
      logMessage('warn', LOG, `gated unlock (post stays teased): ${e}`);
    } finally {
      refreshFeed();
    }
  }

  // ── The self-serve teaser purchase (gap (2c), monetization.md § Per-post
  //    pay-to-unlock → the buyer's price read is post-addressed). ──
  async function buyUnlockOffer(postId: string): Promise<void> {
    const manager = deps.manager();
    if (!manager) { deps.setError(t.common.still_loading); return; }
    try {
      await manager.buyUnlockOffer(postId);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      deps.setError(t.feed.error_buy_unlock({ message: msg }));
    } finally {
      // The manager clears the post's `unlock_offer` on success, so a landed
      // purchase un-renders its own affordance — this re-read pulls that in.
      refreshFeed();
    }
  }

  // The payment link is nest/author-supplied; refuse to open a non-https
  // scheme (security.md § Transport trust) — the same check the profile
  // page's subscription-offer-payment-link applies.
  function openUnlockPaymentLink(url: string): void {
    if (typeof window === 'undefined') return;
    if (!isSafeNavUrl(url)) {
      deps.setError(t.subscriptions.unsafe_payment_url);
      return;
    }
    window.open(url, '_blank', 'noopener');
  }

  /** Own-post delete (feed-post-delete-confirm-button; feed.md § State & data
   *  shape → Post deletion). The manager drops the post from its loaded window
   *  on success — the re-read pulls that in. */
  async function handleDeletePost(postId: string): Promise<void> {
    const manager = deps.manager();
    if (!manager) { deps.setError(t.common.still_loading); return; }
    try {
      await manager.deletePost(postId);
    } catch (e) {
      logMessage('warn', LOG, `delete post failed: ${e}`);
    } finally {
      refreshFeed();
    }
  }

  // ── Own-post web-publishing verbs (web-content-hosting.md
  //    § Published-post management; `#lib/web-publish` is the shared
  //    origin/posts store the `web-settings` section also reads/writes). ──

  // The last web/paywall link copied from the ⋯ menu, keyed by post so a
  // re-render can tell which card's `copied` attr + confirmation line to show.
  const webLink = $state<{ copied: { postId: string; kind: 'web' | 'paywall'; url: string } | null }>({
    copied: null,
  });

  async function publishPostToWeb(postId: string): Promise<void> {
    try {
      // `null` slug: the default is the nest's to mint, never a client-chosen one.
      await webPublishSet(get(identity)?.secretHex ?? '', hexToBytes(postId), null);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      deps.setError(t.web_publish.error_publish({ message: msg }));
    } finally {
      // Re-read rather than editing locally: the card's `web_slug` — which
      // drives the whole verb family — repaints from the nest's own answer.
      refreshFeed();
    }
  }

  async function unpublishPostFromWeb(postId: string): Promise<void> {
    try {
      await webPublishUnset(get(identity)?.secretHex ?? '', hexToBytes(postId));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      deps.setError(t.web_publish.error_unpublish({ message: msg }));
    } finally {
      refreshFeed();
    }
  }

  // Purely local — the origin and the slug are both already resolved on
  // screen, so the public link costs no round trip.
  async function copyPostWebLink(postId: string): Promise<void> {
    const origin = get(siteLink)?.origin;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const post = deps.posts().find((p: any) => p.post_id === postId);
    if (!origin || !post?.web_slug) return;
    const url = webPostPageUrl(origin, post.web_slug);
    await bestEffortCopyToClipboard(url);
    webLink.copied = { postId, kind: 'web', url };
  }

  // A fresh mint per click: the token is short-lived by ratified design and
  // re-minting is free, so re-copying always yields a link that works from
  // now rather than a cached one that already expired.
  async function copyPostPaywallLink(postId: string): Promise<void> {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const post = deps.posts().find((p: any) => p.post_id === postId);
    const origin = get(siteLink)?.origin;
    if (!origin || !post?.web_slug) return;
    try {
      const minted = await webPaywallMintToken(get(identity)?.secretHex ?? '', post.web_slug);
      const url = webTokenedUrl(origin, minted.path, minted.token);
      await bestEffortCopyToClipboard(url);
      webLink.copied = { postId, kind: 'paywall', url };
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      deps.setError(t.web_publish.error_paywall_link({ message: msg }));
    }
  }

  // ── Per-post lazy augmentation (mirrors linux post_list.rs's render-time
  //    resolve): decode fauna/nostr bodies for rich rendering, resolve the media
  //    blob hash for has_media posts, and project the quoted-post embed. Each is
  //    guarded so the caller's effect (which re-runs on every snapshot change)
  //    is idempotent. ──
  //
  // Rich-render augmentation caches (feed.md § Where logic lives — web keeps the
  // structuredView body-decode for structured cards / markdown / multi-media;
  // the snapshot owns the list, these only enrich rendering). Decoded bodies keyed
  // by post id; `null` ⇒ resolved-empty (stop retrying).
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let decodedPosts = $state<Record<string, any>>({});
  const decoding = new Set<string>();
  const mediaResolving = new Set<string>();
  const quoteResolving = new Set<string>();
  const previewResolving = new Set<string>();
  const unlockOfferResolving = new Set<string>();
  const tipsResolving = new Set<string>();

  /// Every cache above is ACTOR-SCOPED and must die with the actor: they are
  /// keyed by post id alone, and each doubles as a fire-once guard. A page
  /// that is NOT guaranteed to unmount between actors (the feed — a
  /// client-side actor switch `goto`s to the route it is already on) calls
  /// this from its actor-scoped reset; `decodedPosts` is the sharper one, since
  /// a surviving entry renders the outgoing actor's decoded content.
  function resetAugmentation(): void {
    decodedPosts = {};
    decoding.clear();
    mediaResolving.clear();
    quoteResolving.clear();
    previewResolving.clear();
    unlockOfferResolving.clear();
    tipsResolving.clear();
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function landed(id: string): any | undefined {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return deps.latestPosts().find((q: any) => q.post_id === id);
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function augmentPost(p: any): void {
    const manager = deps.manager();
    if (!manager) return; // manager-gate-ok: render-time augmentation, not a user action
    const id = p.post_id as string;
    if ((p.source === 'fauna' || p.source === 'nostr') && decodedPosts[id] === undefined && !decoding.has(id)) {
      decoding.add(id);
      void fetchAndDecodePost(id);
    }
    // The guard is the snapshot's `media_hash == null` — `== null`, never falsiness: an
    // all-remote post resolves to `media_hash: ""` (render-model.md § D6c — a truthful
    // "resolved, nothing"). `mediaResolving` only dedupes a resolve in flight: once the post
    // reads resolved its id is forgotten, so a post a reload lands unresolved resolves it
    // again — as tui and linux do, whose guard is the snapshot alone. A resolve that left
    // the post unresolved (the `posts.get` failed) keeps its id: no retry loop.
    if (p.has_media && p.media_hash == null && !mediaResolving.has(id)) {
      mediaResolving.add(id);
      // resolve_media fetches+decodes the body to fill PostSummary.media_hash
      // (the feed index never carries it — feed.md § The read model).
      void manager.resolveMedia(id).then(() => {
        refreshFeed();
        const after = landed(id);
        if (after && after.media_hash != null) mediaResolving.delete(id);
      });
    }
    // Either embed field folds through the same call: a QUOTE embeds its
    // target, a REPOST ROW embeds its original (feed.md § Interaction bar →
    // Repost, ratified 2026-08-10). The guard is the snapshot: the embed is
    // still unfolded; `quoteResolving` only dedupes a resolve in flight.
    const qid = (p.quoted_post_id ?? p.reposted_post_id) as string | undefined;
    if (qid && quotedPostBlock(p.document) === null && !quoteResolving.has(qid)) {
      quoteResolving.add(qid);
      // resolve_quoted_post folds a `QuotedPost` block into the quoting/reposting
      // post's document (render-model.md § D6). The id is released once the post
      // reads folded; a resolve that left it unfolded keeps it: no retry loop.
      void manager.resolveQuotedPost(qid).then(() => {
        refreshFeed();
        const after = landed(id);
        if (after && quotedPostBlock(after.document) !== null) quoteResolving.delete(qid);
      });
    }
    // Link previews (render-model.md § D4): the producer emits a `LinkPreview`
    // block (Resolving) for each bare-url paragraph; resolve_link_preview calls
    // fauna.linkpreview.resolve and re-emits it Resolved/Failed. Fire once per url.
    for (const url of resolvingLinkPreviewUrls(p.document)) {
      if (!previewResolving.has(url)) {
        previewResolving.add(url);
        void manager.resolveLinkPreview(url).then(() => refreshFeed());
      }
    }
    // The buyer's price read (gap (2c), monetization.md § Per-post
    // pay-to-unlock → the buyer's price read is post-addressed) — fire once
    // per post for a designated `post-unlock-*` tier. `unlockOfferResolving`
    // dedupes only a resolve in flight: a reload rebuilds every post with
    // `unlock_offer` unset and resolves it again. A resolve that answered no
    // offer (the author's own post, an undesignated tier) keeps its id — except
    // a pass the purchase guard declined (`unlock_purchase_pending`, set by the
    // manager): it releases the id so the reload that clears the marker resolves
    // again, and the marker itself stops a refire until then.
    const gatedTier = p.gated_tier as string | undefined;
    if (gatedTier?.startsWith('post-unlock-') && !p.unlock_offer && !p.unlock_purchase_pending
        && !unlockOfferResolving.has(id)) {
      unlockOfferResolving.add(id);
      void manager.resolvePostUnlockOffer(id).then(() => {
        refreshFeed();
        const after = landed(id);
        if (after && (after.unlock_offer != null || after.unlock_purchase_pending)) {
          unlockOfferResolving.delete(id);
        }
      });
    }
    // The tip surface (monetization.md § Tips). No data trigger exists —
    // nothing in the feed projection says whether a post has tips — so the
    // guard is the resolved field alone, and the resolve is fire-once
    // *because* it writes a view on every outcome (an untipped post resolves
    // to zeroes).
    if (!p.tips && !tipsResolving.has(id)) {
      tipsResolving.add(id);
      void manager.resolvePostTips(id).then(() => {
        refreshFeed();
        const after = landed(id);
        if (after && after.tips != null) tipsResolving.delete(id);
      });
    }
  }

  async function fetchAndDecodePost(postId: string): Promise<void> {
    const id = get(identity);
    if (!id) return;
    try {
      const bytes = await getPost(id.secretHex, postId);
      // Bound to `postId`: a body not validly signed AS this post caches `null`,
      // so the manager's snapshot body renders under the manager's badge.
      const decoded = paintableDecodedPost(await decodePostFetchedAs(postId, bytes));
      decodedPosts = { ...decodedPosts, [postId]: decoded };
    } catch (e) {
      // A post whose stored bytes aren't envelope-wrapped, or a transient fetch
      // failure: cache `null` so the snapshot preview body renders and we stop
      // retrying (the snapshot list is unaffected — this only enriches rendering).
      decodedPosts = { ...decodedPosts, [postId]: null };
      logMessage('warn', LOG, `decode post error (post ${postId}): ${e}`);
    }
  }

  /** Revoke every object URL this host opened — they hold decrypted bytes, and
   *  a page's teardown must not leak them. */
  function destroy(): void {
    for (const u of Object.values(sealedMediaUrls)) URL.revokeObjectURL(u);
    for (const u of Object.values(proxiedMediaUrls)) URL.revokeObjectURL(u);
  }

  return {
    get decodedPosts() { return decodedPosts; },
    reply,
    trainSheet,
    webLink,
    mediaUrl,
    proxiedMediaUrl,
    playbackUrl,
    isPostMuted,
    revealMuted,
    revealContent,
    postRender,
    regionPlaceholderFor,
    isContentBlocked,
    guardianReasonFor,
    isContentReported,
    isContentCollapsed,
    reportTargetFor,
    trainTargetFactor,
    trainMarkerFor,
    trainPost,
    trainInto,
    replyTarget,
    closeReplyDialog,
    submitReply,
    handleInteract,
    unlockGated,
    buyUnlockOffer,
    openUnlockPaymentLink,
    handleDeletePost,
    publishPostToWeb,
    unpublishPostFromWeb,
    copyPostWebLink,
    copyPostPaywallLink,
    augmentPost,
    resetAugmentation,
    destroy,
    /** The manager this host acts on (the detail's reveal + thread doors). */
    manager: (): WasmFeedManager | null => deps.manager(),
    refreshFeed,
  };
}
