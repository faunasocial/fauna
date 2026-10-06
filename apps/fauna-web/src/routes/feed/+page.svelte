<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { onStoreChange } from '$lib/store-change';
  import { identity, reconnectTick, onActorChange } from '$lib/store';
  import { registerActorScopedReset } from '$lib/actorScope';
  import { get } from 'svelte/store';
  import { getFeedManager, feedSnapshot, refreshFeed, scheduleDraftSave } from '$lib/feed';
  import { CueCapture, hydrateCuesOnce } from '$lib/feed-cues';
  import { clearSent } from '$lib/compose-sent';
  import { contentRender, hydrateContentPolicy } from '$lib/contentPolicy.svelte';
  import { registerRegionBlockCounter } from '$lib/region.svelte';
  import RegionPlaceholder from '$lib/components/RegionPlaceholder.svelte';
  import { notifyBuffer } from '$lib/familyNotify';
  import { syncFeedRoomPosts } from '$lib/conversations';
  import { decodePost, ensureWasm, processAndSealPublicPost, sealComposeAttachment, logMessage, markdownToDocument, classifySources, parseWeightPermille, formatWeightPermille, ruleTypeOptions, builtinFactorOptions, ruleSummaryLabel, ruleRequiredLabel, canAddRule, legalTakedownTombstone, type RuleTypeOption } from '$lib/wasm';
  import { consumePendingSearchNav } from '$lib/search';
  import { documentToHtml, documentHasBlockedRemoteImages, quotedPostBlock, mediaImageHash, proxiedPostImage, resolvingLinkPreviewUrls } from '$lib/document';
  import { readProvenanceFromBytes } from '$lib/c2pa';
  import { hexToBytes } from '$lib/hex';
  import { toArrayBufferView } from '$lib/bytes';
  import { getPost, uploadBlobMultipart, fetchBlob, fetchNestPath, nodeUrl } from '$lib/api';
  import { trainedTopicsList, webPaywallMintToken, webPublishSet, webPublishUnset, sharedRpcPort, type TrainedTopicRow } from '$lib/rpc';
  import { webPostPageUrl, webTokenedUrl } from '$lib/wasm';
  import { bestEffortCopyToClipboard, siteLink, ensureWebPageHydrated } from '$lib/web-publish';
  import { resolveKey, resolveLocalized } from '$lib/i18n/localized';
  import { feedRefusalI18nKey, contentLabelBadgeFor, type ContentLabelEntry, type ContentRender, type RegionPlaceholder as RegionPlaceholderValue } from '$lib/wasm';
  import { byteSize } from '$lib/value-format';
  import { isSafeNavUrl } from '$lib/safe-url';
  import { createLabelerCatalogMachine } from '$lib/wasm-labeler-catalog';
  import FeedComposeBar from '$lib/components/FeedComposeBar.svelte';
  import MarkdownToolbar from '$lib/components/MarkdownToolbar.svelte';
  import PostCard from '$lib/components/PostCard.svelte';
  import ProxiedImage from '$lib/components/ProxiedImage.svelte';
  import QuotedPost from '$lib/components/QuotedPost.svelte';
  import TipSurface from '$lib/components/payments/TipSurface.svelte';
  import { shortActor } from '$lib/feed-utils';
  import { sourceGlyphEmoji } from '$lib/source-glyph';
  import { t } from '$lib/i18n/strings';
  import type { WasmFeedManager } from '../../../static/fauna_wasm.js';
  import type { LabelerCatalogMachine } from '../../../static/fauna_wasm_labeler_catalog.js';
  import type { LabelerCatalogSnapshot } from '$lib/labeler-catalog-machine';
  import { IDS } from '$lib/generated/uiIds';

  // The create-feed form's rule triple — the shared `fauna_feed::FilterRuleInput`
  // the manager encodes via `encode_filter_rule` (feed.md § Where logic lives —
  // the form is client glue, the encoding is shared Rust). The page builds these
  // triples and hands them to `createFeed`; it never builds the wire `FilterRule`.
  type FilterRuleInput = { rule_type: string; value: string; required: boolean };

  // The factor-weight editor's entry — the shared `fauna_feed::FactorWeightInput`
  // (content-moderation-and-ranking.md § Composition; UI approved 2026-07-08).
  // `global` routes it into the caller's global factor set instead of this
  // feed's own composition — `FeedManager::create_feed` does the split.
  type FactorWeightInput = { factor: string; weight_permille: number; global: boolean };

  function blobUrl(hash: string): string {
    return `${nodeUrl()}/api/v1/blob/${hash}`;
  }

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
  // Which of the two is NOT a judgment this page makes: `isSealedMedia` is the
  // shared manager's own answer (it holds the keys), so web and apple — the two
  // apps whose image path is a URL rather than bytes — ask instead of guessing.
  // The other five route every hash through `openMediaBytes` unconditionally.
  // Either way the post card just paints the string it is handed.
  //
  // Cache contract is the conversations-attachment one: a miss returns `null` and
  // is NOT cached, so the next render retries; the fetch is kicked off once per
  // hash and repaints via `$state` when it lands. Object URLs are revoked on
  // destroy so a reactive re-render doesn't leak one per tick.
  let sealedMediaUrls = $state<Record<string, string>>({});
  const sealedMediaPending = new Map<string, Promise<string | null>>();

  // Fetch + open one sealed item into an object URL — the one opener a sealed
  // post image (`mediaUrl`) and a sealed post video (`playbackUrl`) share. A
  // concurrent ask for the same hash joins the fetch already in flight.
  function openSealedMedia(hash: string): Promise<string | null> {
    const cached = sealedMediaUrls[hash];
    if (cached) return Promise.resolve(cached);
    let pending = sealedMediaPending.get(hash);
    if (!pending) {
      pending = (async () => {
        try {
          const id = $identity;
          if (!id) return null;
          const bytes = await fetchBlob(id.secretHex, hash);
          const opened = manager?.openMediaBytes(hash, bytes);
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
          logMessage('warn', 'fauna_web::feed', `sealed post media (card stays blank): ${e}`);
          return null;
        } finally {
          sealedMediaPending.delete(hash);
        }
      })();
      sealedMediaPending.set(hash, pending);
    }
    return pending;
  }

  function mediaUrl(hash: string): string | null {
    let isSealed = false;
    try {
      isSealed = manager?.isSealedMedia(hash) ?? false;
    } catch {
      isSealed = false;
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
    const id = $identity;
    if (!id || proxiedMediaPending.has(path)) return null;
    proxiedMediaPending.add(path);
    void (async () => {
      try {
        const bytes = await fetchNestPath(id.secretHex, path);
        if (bytes.length > 0 && !destroyed) {
          proxiedMediaUrls[path] = URL.createObjectURL(new Blob([toArrayBufferView(bytes)]));
        }
      } catch (e) {
        logMessage('warn', 'fauna_web::feed', `proxied post media (placeholder stays): ${e}`);
      } finally {
        proxiedMediaPending.delete(path);
      }
    })();
    return null;
  }

  // What a tapped `video-thumbnail` plays (render-model.md § D6c → *Inline
  // playback*). The decision is the shared manager's `playbackSource`; this page
  // only turns its answer into a `<video src>`: a nest-relative URL gets the nest
  // origin, a sealed item goes through the same opener a sealed image does.
  // `null` = nothing playable — the player's native error UI is the surface.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async function playbackUrl(block: any): Promise<string | null> {
    if (!manager) { loadError = t.common.still_loading; return null; }
    try {
      const source = await manager.playbackSource(block);
      if (source && 'Url' in source) return `${nodeUrl()}${source.Url.url}`;
      if (source && 'Sealed' in source) return await openSealedMedia(source.Sealed.hash);
      logMessage('warn', 'fauna_web::feed', `video not playable: ${JSON.stringify(source)}`);
      return null;
    } finally {
      refreshFeed();
    }
  }

  let manager: WasmFeedManager | null = null;
  // Reactive "the feed manager is built" flag (the plain `manager` ref above is
  // not `$state`, so the compose bar can't gate on it directly). Flips true once
  // onMount resolves `getFeedManager()` (~1s after login, gated on the WS-RPC
  // connect). `handleCompose` needs the manager or it silently returns, so the
  // compose submit is disabled until this is true — an enabled submit that
  // drops the post is the create-post e2e's load-independent flake.
  let feedReady = $state(false);

  // ── Snapshot-derived view (feed.md § Architectural rules #1: the page renders
  //    entirely from the shared FeedManager snapshot; no client-side post-list
  //    state). The browser owns the loop — every async manager method is followed
  //    by refreshFeed(), which re-reads snapshot() into the `feedSnapshot` store. ──
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let snap = $derived<any>($feedSnapshot);
  let feeds = $derived(snap?.feeds ?? []);
  let bridgeFeeds = $derived(snap?.bridge_feeds ?? []);
  let selectedFeed = $derived<string | null>(snap?.selected_feed ?? null);
  let trendingSelected = $derived<boolean>(snap?.trending_selected ?? false);
  let posts = $derived(snap?.posts ?? []);
  // Guardian Notify (family-safety.md § Guardian Notify): count each rendered post
  // whose guardian floor enforces — a no-op unless the ward's content_notify knob is
  // on. The buffer dedups per post per local day, so re-runs on any posts change are
  // safe; it flushes the batch to fauna.family.notify_report at most hourly.
  $effect(() => {
    for (const post of posts) notifyBuffer.record(post.post_id, post.labels);
  });
  let status = $derived<string>(snap?.status ?? 'Loading');
  let hasMore = $derived<boolean>(snap?.has_more ?? false);
  // A lifecycle-load failure (see `loadStep`). Declared before `pageError` so the
  // derivation never reads it in its temporal dead zone.
  let loadError = $state('');
  // The manager's own snapshot error wins; `loadError` carries a lifecycle-load
  // failure that has no snapshot to live in, so it is the fallback rather than a
  // competing surface.
  let pageError = $derived(resolveLocalized(snap?.error) || loadError);
  let composeError = $derived(resolveLocalized(snap?.compose?.error));
  // The manager's own `attached_file` — never the local `composeFile` pick
  // alone, so a restored draft's handle (no bytes behind it on this device)
  // is visible too (feed.md § Persistence → *Attachments by content
  // address*). Drives `compose-file-ready` on both the inline bar and the
  // rich dialog.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let attachedFile = $derived<any>(snap?.compose?.attached_file ?? null);
  let bridgeFormError = $derived(resolveLocalized(snap?.bridge_form?.error));

  // ── Local UI state (gestures + the rich-render augmentation caches) ──────────

  // Sidebar: feed creation form
  let showCreateForm = $state(false);
  let newFeedName = $state('');
  let newFeedCombination = $state('all');
  let newFeedRules = $state<FilterRuleInput[]>([]);
  let creatingFeed = $state(false);

  // The rule-type picker's catalog + which input widget each row needs — shared Rust
  // (`fauna_client_feed::rule_type_options`, docs/goal/ui/feed.md § Where logic lives
  // -> Feed rule-builder presentation). Populated in onMount (after ensureWasm) rather
  // than at module scope: wasm isn't guaranteed loaded until then.
  let ruleTypeOptionsList = $state<RuleTypeOption[]>([]);
  let newRuleType = $state('HasHashtag');
  let newRuleInput = $state('');
  let newRuleCount = $state(1);
  let newRuleRequired = $state(true);
  let newRuleInputKind = $derived(
    ruleTypeOptionsList.find((o) => o.value === newRuleType)?.input_kind ?? 'Text',
  );
  // Gates `feed-add-rule-button` — apple's `FeedCreateForm.canAddRule`, lifted
  // into shared Rust (`fauna_client_feed::can_add_rule`, feed.md § Add-rule
  // gating). `newRuleCount` is dual-purpose per `newRuleInputKind` (see `addRule`
  // below): the Number kind's own value, or the TextAndNumber kind's threshold.
  let canAddRuleNow = $derived(
    canAddRule(
      newRuleInputKind,
      newRuleInputKind === 'Number' ? String(newRuleCount) : newRuleInput,
      newRuleInputKind === 'TextAndNumber' ? String(newRuleCount) : '',
    ),
  );

  // Factor-weight editor (content-moderation-and-ranking.md § Composition) —
  // a second repeatable-entry row alongside the filter-rule builder above.
  // `factorOptions` starts with the shared built-ins (`builtinFactorOptions`:
  // engagement, trending — filled once wasm is up) and grows with the caller's
  // subscribed labeler:<hex> factors (raw factor id — no display name exists
  // anywhere yet, matching the labeler-catalog page's own precedent) once the
  // lazily-built LabelerCatalogMachine's refresh resolves.
  let newFeedFactors = $state<FactorWeightInput[]>([]);
  let factorOptions = $state<{ key: string; label: string }[]>([]);

  function builtinFactorEntries(): { key: string; label: string }[] {
    return builtinFactorOptions().map((o) => ({ key: o.value, label: resolveLocalized(o.label) }));
  }
  let newFactorKey = $state('engagement');
  let newFactorWeight = $state('1.0');
  let newFactorGlobal = $state(false);
  let labelerCatalogMachine: LabelerCatalogMachine | null = null;

  async function ensureFactorOptions(): Promise<void> {
    const id = $identity;
    if (!id) return;
    try {
      // The catalog machine is built once (it is expensive and self-refreshing);
      // the OPTIONS are rebuilt on every open — the user may have just minted a
      // trained topic on the Personalization page, and a memoized list would
      // silently fail to offer it.
      if (!labelerCatalogMachine) {
        labelerCatalogMachine = await createLabelerCatalogMachine(
          { onChanged: () => {} },
          await sharedRpcPort(id.secretHex),
          id.secretHex,
        );
      }
      await labelerCatalogMachine.refresh();
      const raw = labelerCatalogMachine.snapshotJson();
      const snap = raw ? (JSON.parse(raw) as LabelerCatalogSnapshot) : null;
      const subscribed = (snap?.entries ?? []).filter((e) => e.subscribed);
      // Trained topics from the sealed registry (topic-factors.md § picker):
      // the option's VALUE is the stable `topic:<hex>` key, its LABEL the user's
      // chosen name — display ≠ key here, which is exactly why the cross-app
      // `select(id, value=key)` contract addresses options by key.
      const topics = await trainedTopicsList(id.secretHex);
      factorOptions = [
        ...builtinFactorEntries(),
        ...subscribed.map((e) => ({ key: e.factor, label: e.factor })),
        ...topics
          .filter((r) => r.factor_key)
          .map((r) => ({ key: r.factor_key as string, label: r.name })),
      ];
    } catch (e: unknown) {
      logMessage('warn', 'fauna_web::feed', `factor options fetch failed: ${e}`);
    }
  }

  function addFactor(): void {
    // Shared decimal→signed-per-mille parse (fauna_core::format::parse_weight_permille):
    // strict whole-string parse with a 1.0-baseline fallback, rounding half-away-from-zero.
    // Never hand-roll — `parseFloat` is lenient and JS `Math.round` is half-up, which drifted
    // this client from linux/windows on `"2abc"` and on every negative midpoint.
    const weight_permille = parseWeightPermille(newFactorWeight);
    newFeedFactors = [
      ...newFeedFactors,
      { factor: newFactorKey, weight_permille, global: newFactorGlobal },
    ];
    newFactorWeight = '1.0';
    newFactorGlobal = false;
  }

  function factorLabel(f: FactorWeightInput): string {
    const opt = factorOptions.find((o) => o.key === f.factor);
    const name = opt?.label ?? f.factor;
    const weight = formatWeightPermille(f.weight_permille);
    return f.global ? `${name} × ${weight} (all feeds)` : `${name} × ${weight}`;
  }

  function removeFactor(i: number): void {
    newFeedFactors = newFeedFactors.filter((_, idx) => idx !== i);
  }

  // Bridge subscribe form (local inputs; the manager owns submit + error state)
  let showBridgeSubscribeForm = $state(false);
  let bridgeFormBridge = $state('bluesky');
  let bridgeFormUri = $state('');
  let bridgeFormName = $state('');
  let bridgeFormSaving = $state(false);

  // Selector options = the bridges the nest can actually serve
  // (`snapshot.available_bridges`, from `fauna.bridges.list`, gated by build
  // feature + runtime `available`) — never a hard-coded protocol list, so the SPA
  // never offers a protocol the nest can't serve (`version-compatibility.md`
  // § Dim 3 — capability consumption). Empty ⇒ the subscribe form is hidden.
  let bridgeOptions = $derived(
    (snap?.available_bridges ?? []).map((b: any) => ({ value: b.id, label: b.name }))
  );
  // Keep the selected bridge valid as the option set loads/changes: if the
  // current pick isn't offered by this nest, fall back to the first available.
  $effect(() => {
    if (bridgeOptions.length > 0 && !bridgeOptions.some((o: any) => o.value === bridgeFormBridge)) {
      bridgeFormBridge = bridgeOptions[0].value;
    }
  });

  // Search — a server-side re-query via the manager (feed.md § Where logic lives:
  // never a client-side filter over the loaded list). Debounced so a burst of
  // keystrokes coalesces into one re-query (the GTK SearchEntry analogue linux
  // uses). `searchInput` mirrors snapshot.search_query for immediate field echo.
  let searchInput = $state('');
  let searchTimer: ReturnType<typeof setTimeout> | undefined;

  // Compose (local; uploaded blob staged into the manager at submit)
  let composeBody = $state('');
  let composeTags = $state('');
  let composing = $state(false);
  let composeFile: File | null = $state(null);
  let composeFileData: Uint8Array | null = $state(null);
  // Gate-to-tier compose (feed.md § Encryption at rest; monetization.md § Pillars
  // 2+3). `gateTier` is the `compose-gate-tier-select` value: '' = Public (ungated),
  // else one of the author's own tier names (from `snap.own_tiers`, auto-refreshed
  // by the manager's reload). `gatePreview` is the plaintext public teaser
  // (`compose-gate-preview-field`), visible only when a tier is selected.
  let gateTier = $state('');
  let gatePreview = $state('');
  let ownTiers = $derived<{ name: string; rank: number }[]>(snap?.own_tiers ?? []);
  // The select's room answer (feed.md § Encryption at rest → *Room-restricted —
  // the app half*): '' or a room's hex channel id, from `snap.own_rooms` — the
  // rooms the author can address a post to, re-read on the conversations tick
  // (`$lib/conversations`' `syncFeedRoomPosts`). Exclusive with a tier and a
  // sale; resolved index-wise by FeedComposeBar like the other two.
  let gateRoom = $state('');
  let ownRooms = $derived<{ room: string; label: string }[]>(snap?.own_rooms ?? []);
  // "Sell this post…" (monetization.md § Per-post pay-to-unlock; IDs
  // user-approved 2026-07-29) — the select's third answer, sharing gatePreview
  // as the teaser. `sellSelected` is derived index-wise by FeedComposeBar
  // (never by comparing against the localized label — an author-named tier can share that exact string, and gateTier
  // itself never holds the sell sentinel). `sellPrice` is the
  // `compose-sell-price` free-text price hint; `sellSubscribersFree` the
  // ratified rank knob (`compose-sell-subscribers-free`), defaults CHECKED.
  let sellSelected = $state(false);
  let sellPrice = $state('');
  // The machine-comparable price (monetization.md § The asking price) —
  // independent of sellPrice above; no parsing ever infers one from the
  // other. Empty means no machine price: the minted tier stays a tip target
  // forever.
  let sellAskingPrice = $state('');
  let sellSubscribersFree = $state(true);

  // The composer's user-authored fields as one value. `handleCompose` reads it
  // ONCE, at the click, and sends only that; its success path clears only the
  // fields that still hold what was sent (`$lib/compose-sent`). Both halves exist
  // because the submit awaits for a long time — uploads, the create, the reload —
  // and the user can go on typing throughout it.
  type ComposeFields = {
    body: string;
    tags: string;
    file: File | null;
    fileData: Uint8Array | null;
    gateTier: string;
    gateRoom: string;
    gatePreview: string;
    sellSelected: boolean;
    sellPrice: string;
    sellAskingPrice: string;
    sellSubscribersFree: boolean;
  };
  const EMPTY_COMPOSE: ComposeFields = {
    body: '',
    tags: '',
    file: null,
    fileData: null,
    gateTier: '',
    gateRoom: '',
    gatePreview: '',
    sellSelected: false,
    sellPrice: '',
    sellAskingPrice: '',
    sellSubscribersFree: true,
  };
  function readCompose(): ComposeFields {
    return {
      body: composeBody,
      tags: composeTags,
      file: composeFile,
      fileData: composeFileData,
      gateTier,
      gateRoom,
      gatePreview,
      sellSelected,
      sellPrice,
      sellAskingPrice,
      sellSubscribersFree,
    };
  }
  function writeCompose(c: ComposeFields): void {
    composeBody = c.body;
    composeTags = c.tags;
    composeFile = c.file;
    composeFileData = c.fileData;
    gateTier = c.gateTier;
    gateRoom = c.gateRoom;
    gatePreview = c.gatePreview;
    sellSelected = c.sellSelected;
    sellPrice = c.sellPrice;
    sellAskingPrice = c.sellAskingPrice;
    sellSubscribersFree = c.sellSubscribersFree;
  }
  // The audience answer is sticky on `clearSent` (owner ruling, feed.md § User actions): it clears only when the composer is
  // otherwise untouched, so it never silently widens to Public while the user
  // keeps typing the next post. Mirrors `FeedComposeState::clear_sent`'s
  // `untouched` gate (`libs/fauna-feed/src/compose.rs`).
  const COMPOSE_STICKY = {
    contentKeys: ['body', 'tags', 'file', 'fileData'] as (keyof ComposeFields)[],
    stickyKeys: ['gateTier', 'gateRoom', 'gatePreview', 'sellSelected', 'sellPrice', 'sellAskingPrice', 'sellSubscribersFree'] as (keyof ComposeFields)[],
  };
  let showComposeDialog = $state(false);
  let dialogTextarea: HTMLTextAreaElement | undefined = $state();
  let dialogDragging = $state(false);

  // ── Muted-keyword collapse + trained-topic training verbs
  //    (topic-factors.md § Scoring / § Training signals) ──────────────────────
  //
  // Both ride the shared FeedManager (`isMuted` / `trainTargetFactor` /
  // `exampleLabelFor` / `trainPost` / `untrainPost`) — no client-side derivation.
  // The card cannot reach the manager, so the page reads these per post and hands
  // them down as props (the `onrevealremote` shape).

  /** Posts the user un-collapsed this session. Session-local RENDER state — the
   *  term stays muted (un-muting the word is what stops future collapse). The
   *  linux twin is `REVEALED_MUTED_POSTS`; the conversation twin is
   *  `revealedMuted` on the conversations page. */
  let revealedMuted = $state<Set<string>>(new Set());

  function isPostMuted(postId: string): boolean {
    if (revealedMuted.has(postId)) return false;
    try {
      return manager?.isMuted(postId) ?? false;
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
   *  session — the separate reveal set the content pillar keeps (linux's
   *  `REVEALED_CONTENT_POSTS`; distinct from `revealedMuted`). A `block` floor is
   *  never revealable. */
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
  // the region and the family collapse). Reads the module `$state`, so these
  // re-run when `hydrateContentPolicy` or a region refresh lands.
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
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function isContentCollapsed(post: any): boolean {
    return !revealedContent.has(post.post_id) && postRender(post).verdict === 'collapse';
  }
  // Convention 17's verdict-side walk (`region-block-never-silent`): the loaded
  // cards the region blocks, plus an open detail it blocks.
  onMount(() =>
    registerRegionBlockCounter('feed', () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      let n = posts.filter((p: any) => postRender(p).placeholder?.verb === 'block').length;
      if (detailPost && postRender(detailPost).placeholder?.verb === 'block') n += 1;
      return n;
    }),
  );

  /** The composed feed's single trained topic, or null (then the verbs open the
   *  target sheet). Re-derived from `snap` so switching feeds re-reads it. */
  let trainTarget = $derived.by<string | null>(() => {
    void snap; // re-run whenever the manager pushes a new snapshot
    try {
      return manager?.trainTargetFactor() ?? null;
    } catch {
      return null;
    }
  });

  /** This post's badge label, `"category:confidence"` (the `ContentLabelBadge`
   *  wire format) — the highest-confidence entry of `PostSummary.labels`
   *  (`moderation.md` § Per-row badge data path). `undefined` when unlabelled. */
  function contentLabelFor(post: { labels?: ContentLabelEntry[] }): string | undefined {
    return contentLabelBadgeFor(post.labels);
  }

  /** This post's example marker for the in-context factor ('more'/'less'/null). */
  function trainMarkerFor(postId: string): string | null {
    if (!trainTarget) return null;
    try {
      return manager?.exampleLabelFor(postId, trainTarget) ?? null;
    } catch {
      return null;
    }
  }

  /** The post awaiting a factor choice, when no in-context factor resolved. */
  let trainSheetFor = $state<{ post_id: string; verb: 'more' | 'less' } | null>(null);
  let trainSheetTopics = $state<TrainedTopicRow[]>([]);

  // ── Reply compose dialog (feed.md § Implementation status today: reply is
  //    COMPOSED via FeedManager::reply, never interact — its native arm
  //    discards body). Minimal modal mirroring linux's build_reply_dialog:
  //    a text field + submit, no cancel needed beyond click-outside/Escape. ──
  let replyTargetId = $state<string | null>(null);
  let replyText = $state('');
  let replyTargetAuthor = $derived.by<string>(() => {
    if (!replyTargetId) return '';
    const p = posts.find((p: any) => p.post_id === replyTargetId);
    return p ? shortActor(p.author) : '';
  });

  /** A rejected feed verb as `error-message` paints it: a **stated refusal** (words
   *  under a restricted post — `ui/feed.md` § Encryption at rest → *A reply, quote
   *  or repost of a restricted post*) reads in the user's language through the
   *  shared `refusal_i18n_key`; every other failure keeps its own text. The twin of
   *  tui's `refusal_copy`. */
  function verbErrorCopy(e: unknown): string {
    const text = e instanceof Error ? e.message : String(e);
    const key = feedRefusalI18nKey(text);
    return key ? resolveKey(key) : text;
  }

  function closeReplyDialog(): void {
    replyTargetId = null;
    replyText = '';
  }

  async function submitReply(): Promise<void> {
    // The reply button gates on `replyText.trim()` alone — never on
    // `feedReady` — so this handler is genuinely reachable with a null
    // manager, and a bare return would discard the typed reply with no
    // feedback at all (web.md § Async manager readiness).
    if (!manager) { loadError = t.common.still_loading; return; }
    if (!replyTargetId) return;
    const text = replyText.trim();
    if (!text) return;
    const postId = replyTargetId;
    closeReplyDialog();
    try {
      await manager.reply(postId, text);
      loadError = '';
    } catch (e) {
      logMessage('warn', 'fauna_web::feed', `reply error: ${e}`);
      loadError = verbErrorCopy(e);
    } finally {
      refreshFeed();
    }
  }

  async function trainPost(postId: string, verb: 'more' | 'less'): Promise<void> {
    if (!trainTarget) {
      // No dominant trained factor — ask which topic to train (the sheet).
      const id = $identity;
      if (!id) return;
      try {
        trainSheetTopics = (await trainedTopicsList(id.secretHex)).filter((r) => r.factor_key);
      } catch {
        trainSheetTopics = [];
      }
      trainSheetFor = { post_id: postId, verb };
      return;
    }
    await trainInto(postId, verb, trainTarget);
  }

  /** Toggle-shaped: tapping the marked verb again untrains (the manager applies
   *  the exact inverse delta); flipping verbs re-trains. */
  async function trainInto(postId: string, verb: 'more' | 'less', factor: string): Promise<void> {
    try {
      if (manager?.exampleLabelFor(postId, factor) === verb) {
        await manager?.untrainPost(postId, factor);
      } else {
        await manager?.trainPost(postId, factor, verb);
      }
      refreshFeed();
    } catch (e: unknown) {
      logMessage('warn', 'fauna_web::feed', `train failed: ${e}`);
    }
  }

  // Rich-render augmentation caches (feed.md § Where logic lives — web keeps the
  // structuredView body-decode for structured cards / markdown / multi-media;
  // the snapshot owns the list, these only enrich rendering). Decoded bodies keyed
  // by post id; `null` ⇒ resolved-empty (stop retrying). The quoted-post embed is no
  // longer cached here — it is folded into `post.document` by the manager and read
  // from the block (render-model.md § D6), so a resolution just `refreshFeed()`s.
  let decodedPosts = $state<Record<string, any>>({});
  const decoding = new Set<string>();
  const mediaResolving = new Set<string>();
  const quoteResolving = new Set<string>();
  const previewResolving = new Set<string>();
  const unlockOfferResolving = new Set<string>();
  const tipsResolving = new Set<string>();

  /// Every cache above is ACTOR-SCOPED and must die with the actor.
  ///
  /// They are all keyed by post id alone, and each doubles as a fire-once guard
  /// (`augmentPost` skips a post whose id is already in the set). That is only
  /// sound while the ids belong to the actor now reading them. This component is
  /// NOT guaranteed to unmount between actors — the same fact the `manager`
  /// rebuild below was rewritten for: a client-side actor switch `goto`s to the
  /// route it is already on, so Svelte keeps the instance and every set survives.
  /// The outgoing actor's ids then permanently suppress the incoming actor's
  /// lazy resolves: the post is listed but its media hash, quoted embed, link
  /// preview and unlock price never resolve again, with no error anywhere —
  /// `gated-post-price` simply never paints (the web half of the teaser buyer
  /// leg). `decodedPosts` is the sharper one: it caches
  /// DECODED BODIES, so a surviving entry renders the outgoing actor's decoded
  /// content to the incoming one.
  ///
  /// tui and linux cannot have this bug: their fire-once guard is the manager
  /// snapshot itself (`post.unlock_offer.is_none()` — `feed/mod.rs::fire_resolves`,
  /// `views/feed/post_list.rs`), which is rebuilt per actor by construction. Web
  /// keeps side caches because it also decodes bodies for rich rendering, so web
  /// is the only client that must reset them explicitly.
  function resetActorScopedAugmentation(): void {
    stopCueCapture();
    decodedPosts = {};
    decoding.clear();
    mediaResolving.clear();
    quoteResolving.clear();
    previewResolving.clear();
    unlockOfferResolving.clear();
    tipsResolving.clear();
  }

  // Post-detail dialog (ui.yaml feed transition `click post-card → post_detail`).
  // Held by id and re-derived from the live snapshot `posts`, not captured as a
  // stale object: a gated post's full body arrives via `unlockGatedPost`, which
  // mutates the snapshot post in place, so the open detail must track the current
  // snapshot row to repaint the unsealed body (the page renders from the shared
  // snapshot — feed.md § Architectural rules #1). Falls back to null when the row
  // pages out, closing the dialog.
  let detailPostId = $state<string | null>(null);
  // The union `resolvePost` makes renderable — the timeline window, THEN the
  // one-slot `deep_linked_post` a deep link parked (`fauna_feed::FeedSnapshot
  // ::rendered_posts` / `find_post`'s own definition of "wherever the
  // snapshot holds it"). A post outside the loaded window has no row in
  // `posts` until `openPostDetail` resolves it into the slot below.
  let detailPost = $derived.by<any | null>(() => {
    if (detailPostId == null) return null;
    const found = posts.find((p: any) => p.post_id === detailPostId);
    if (found) return found;
    const deepLinked = snap?.deep_linked_post;
    return deepLinked && deepLinked.post_id === detailPostId ? deepLinked : null;
  });

  /** Shared post_detail opener — a `post-card` click AND a `search-result-item`
   *  deep-link (`SearchNav::Post`, `../search/+page.svelte`'s `openResult`)
   *  both funnel through here, so they read the manager's post union through
   *  the ONE lookup below, never a second copy (`ui/search.md` § Where logic
   *  lives → *Result navigation (deep link)*). `detailPost` above is the same
   *  lookup expressed as a store-reactive derived, for the template's own
   *  reads once `refreshFeed()` (below) has propagated.
   *
   *  Resolves the post via `resolvePost` FIRST (`feed.md` § The read model →
   *  *Opening a post the timeline never loaded*), mirroring tui's
   *  `Op::OpenPostDetail` step 1 — cheap and idempotent for a `post-card`
   *  click on an already-loaded post (no round trip), and what actually
   *  fetches real content for a search hit outside the window. The four
   *  `PostResolution` outcomes: `Loaded`/`Fetched` land the post in
   *  `manager.snapshot()`'s `posts` or `deep_linked_post`, so the rest of this
   *  function (the reveal seed + the gated unlock) runs unconditionally off
   *  it; `TakenDown` parks a tombstone `PostSummary` (`legal_takedown_ref`
   *  set) in the same slot, so the dialog opens straight into the existing
   *  takedown branch below with nothing further to do here (a withheld post
   *  is never gated); `Unavailable` leaves neither `posts` nor the slot
   *  holding the id, so `target` stays `null` and the `{#if detailPost}` gate
   *  keeps the dialog closed — the same graceful degrade a stale id always
   *  got. */
  async function openPostDetail(postId: string): Promise<void> {
    detailPostId = postId;
    if (!manager) { loadError = t.common.still_loading; return; }
    try {
      await manager.resolvePost(postId);
    } catch (e) {
      logMessage('warn', 'fauna_web::feed', `resolve post detail failed: ${e}`);
    }
    // Read the manager's own snapshot directly rather than this file's
    // `detailPost`/`snap` derived values: those flow through the
    // `feedSnapshot` store's subscription, which is not guaranteed to have
    // repropagated synchronously the instant after the `await` above, and the
    // reveal-seed + gated-unlock decisions below must see the JUST-resolved
    // row, not a stale one. `refreshFeed()` (below) is what the template's
    // own `snap`/`detailPost` reads react to.
    const s = manager.snapshot();
    const target =
      (s.posts ?? []).find((p: any) => p.post_id === postId) ??
      (s.deep_linked_post?.post_id === postId ? s.deep_linked_post : null);
    refreshFeed();
    if (!target) return;
    // Seed the detail's local render toggle from the card's manager state: the
    // snapshot doc already carries `RemoteImage.revealed` projected by the
    // manager, so a post the user already revealed opens with the detail's
    // reveal button hidden (render-model.md § D3).
    detailRevealed = !documentHasBlockedRemoteImages(target.document);
    // Gated post not yet unlocked: kick the async unlock (resolve → bulk fetch
    // → decrypt). On success the manager swaps the full body into the snapshot
    // and the detail (derived from the live row) repaints; until then it shows
    // the teaser.
    if (target.gated_tier && !target.gated_unlocked) void unlockGated(target.post_id);
  }
  let detailDecoded = $derived(detailPost ? decodedPosts[detailPost.post_id] : null);
  let detailRegion = $derived(detailPost ? regionPlaceholderFor(detailPost) : null);
  // The detail's folded embeds, read from `detailPost.document` (render-model.md § D6).
  let detailQuotedBlock = $derived(detailPost ? quotedPostBlock(detailPost.document) : null);
  let detailMediaHash = $derived(detailPost ? mediaImageHash(detailPost.document) : null);
  // A bridged post's own picture takes the detail's `post-image` when it has no blob image.
  let detailProxiedImage = $derived(detailPost ? proxiedPostImage(detailPost.document) : null);
  // The detail body — structured (article/community/…) title+content, else the
  // full decoded body, falling back to the snapshot preview.
  let detailText = $derived.by(() => {
    // A gated post's body is manager-authored: the plaintext teaser until
    // `unlockGatedPost` decrypts the full body into PostSummary.body/document.
    // The client-side decode (`detailDecoded`) only ever sees the sealed
    // envelope's teaser, so never prefer it for a gated post — read the
    // (teaser-then-unsealed) manager body directly.
    if (detailPost?.gated_tier) return detailPost.body ?? '';
    const d = detailDecoded;
    if (d) {
      const s = d.Structured ?? d.structured;
      if (s) {
        const title = (s.fields ?? []).find((f: any) => f.key === 'title')?.value ?? '';
        return [title, s.content].filter(Boolean).join('\n\n');
      }
      if (d.body) return d.body;
    }
    return detailPost?.body ?? '';
  });
  // The detail body as the shared `RenderDocument` (render-model.md § D6), painted
  // by the same `documentToHtml` walker the list card + Conversations page use.
  let detailDoc = $derived(markdownToDocument(detailText));
  // ⚠ Detail-specific exception to the manager-owned reveal contract (render-model.md § D3):
  // `detailDoc` is a CLIENT-BUILT full-body document (`markdownToDocument(detailText)`), NOT a
  // manager snapshot doc, so the feed manager can't project `revealed` onto it. So the detail
  // keeps this single local render toggle and passes it to `documentToHtml` as the per-call
  // override. It is initialized from the card's manager state on open (the snapshot doc tells us
  // whether the post is already revealed), and the button ALSO dispatches to the manager so the
  // underlying card stays consistent. Everywhere else, reveal lives only in the manager.
  let detailRevealed = $state(false);

  // (The detail's tip-attribution open state moved into
  // `payments/TipSurface.svelte` with the surface itself; the `{#key
  // detailPostId}` at its render site is what resets it per post.)

  // Infinite scroll
  let scrollContainer: HTMLElement | undefined;

  // ── Engagement-cue capture (engagement-cues.md § Cue vocabulary &
  //    derivation): the shell is `$lib/feed-cues`; this page only builds it over
  //    the live manager, says whether the post list is what the user sees (a
  //    post-detail or compose dialog covers it), and stops it on the way out. ──
  let cueCapture: CueCapture | null = null;
  let destroyed = false;

  function stopCueCapture(): void {
    void cueCapture?.stop();
    cueCapture = null;
  }

  async function startCueCapture(m: WasmFeedManager): Promise<void> {
    stopCueCapture();
    try {
      await hydrateCuesOnce(m);
    } catch (e) {
      loadError = String(e);
      logMessage('warn', 'fauna_web::feed', `cue rollup hydrate failed: ${e}`);
      return;
    }
    if (destroyed || m !== manager || !scrollContainer || cueCapture) return;
    cueCapture = new CueCapture(
      m,
      scrollContainer,
      () => ({
        showing: !detailPost && !showComposeDialog,
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        windowPostIds: posts.map((p: any) => p.post_id as string),
      }),
      (message) => { loadError = message; },
    );
    cueCapture.start();
  }
  let loadingMore = $state(false);

  let unsubReconnect: (() => void) | null = null;
  let unsubIdentity: (() => void) | null = null;
  let unsubActorReset: (() => void) | null = null;

  // ── Per-post lazy augmentation (mirrors linux post_list.rs's render-time
  //    resolve): decode fauna/nostr bodies for rich rendering, resolve the media
  //    blob hash for has_media posts, and project the quoted-post embed. Each is
  //    guarded so the effect (which re-runs on every snapshot change) is idempotent. ──
  $effect(() => {
    const list = posts; // dependency: re-run when the post list changes
    if (!manager) return; // manager-gate-ok: $effect ordering guard, not a user action
    for (const p of list) augmentPost(p);
  });

  function augmentPost(p: any): void {
    const id = p.post_id as string;
    if ((p.source === 'fauna' || p.source === 'nostr') && decodedPosts[id] === undefined && !decoding.has(id)) {
      decoding.add(id);
      void fetchAndDecodePost(id);
    }
    // The guard is the snapshot's `media_hash == null` — `== null`, never falsiness: an
    // all-remote post resolves to `media_hash: ""` (render-model.md § D6c — a truthful
    // "resolved, nothing"). `mediaResolving` only dedupes a resolve in flight: once the post
    // reads resolved its id is forgotten, so a reload that rebuilds the post unresolved
    // (no fold, `media_hash` absent) resolves it again — as tui and linux do, whose guard is
    // the snapshot alone. Before, the id stayed for the actor's life and a reload painted the
    // card without its media for good. A resolve that left the post unresolved (the
    // `posts.get` failed) keeps its id: no retry loop.
    if (p.has_media && p.media_hash == null && !mediaResolving.has(id)) {
      mediaResolving.add(id);
      // resolve_media fetches+decodes the body to fill PostSummary.media_hash
      // (the feed index never carries it — feed.md § The read model). Refresh to
      // pull the populated hash into the snapshot store.
      void manager!.resolveMedia(id).then(() => {
        refreshFeed();
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const after = (get(feedSnapshot) as any)?.posts?.find((q: any) => q.post_id === id);
        if (after && after.media_hash != null) mediaResolving.delete(id);
      });
    }
    // Either embed field folds through the same call: a QUOTE embeds its
    // target, a REPOST ROW embeds its original (feed.md § Interaction bar →
    // Repost, ratified 2026-08-10).
    const qid = (p.quoted_post_id ?? p.reposted_post_id) as string | undefined;
    if (qid && !quoteResolving.has(qid)) {
      quoteResolving.add(qid);
      // resolve_quoted_post folds a `QuotedPost` block into the quoting/reposting
      // post's document (render-model.md § D6); refresh to pull the folded block
      // into the snapshot store, where PostCard / the detail read it (like
      // resolveMedia).
      void manager!.resolveQuotedPost(qid).then(() => refreshFeed());
    }
    // Link previews (render-model.md § D4): the producer emits a `LinkPreview`
    // block (Resolving) for each bare-url paragraph; resolve_link_preview calls
    // fauna.linkpreview.resolve and re-emits it Resolved/Failed. Fire once per url;
    // refresh to pull the resolved state into the snapshot store, where PostCard
    // paints the card (the same shape as resolveMedia / resolveQuotedPost).
    for (const url of resolvingLinkPreviewUrls(p.document)) {
      if (!previewResolving.has(url)) {
        previewResolving.add(url);
        void manager!.resolveLinkPreview(url).then(() => refreshFeed());
      }
    }
    // The buyer's price read (gap (2c), monetization.md § Per-post
    // pay-to-unlock → the buyer's price read is post-addressed) — fire once
    // per post for a designated `post-unlock-*` tier; refresh to pull the
    // resolved offer into the snapshot store, where PostCard paints it.
    const gatedTier = p.gated_tier as string | undefined;
    if (gatedTier?.startsWith('post-unlock-') && !p.unlock_offer && !unlockOfferResolving.has(id)) {
      unlockOfferResolving.add(id);
      void manager!.resolvePostUnlockOffer(id).then(() => refreshFeed());
    }
    // The tip surface (monetization.md § Tips). No data trigger exists —
    // nothing in the feed projection says whether a post has tips — so the
    // guard is the resolved field alone, and the resolve is fire-once
    // *because* it writes a view on every outcome (an untipped post resolves
    // to zeroes).
    if (!p.tips && !tipsResolving.has(id)) {
      tipsResolving.add(id);
      void manager!.resolvePostTips(id).then(() => refreshFeed());
    }
  }

  async function fetchAndDecodePost(postId: string): Promise<void> {
    const id = $identity;
    if (!id) return;
    try {
      const bytes = await getPost(id.secretHex, postId);
      const decoded = await decodePost(bytes);
      decodedPosts = { ...decodedPosts, [postId]: decoded };
    } catch (e) {
      // A post whose stored bytes aren't envelope-wrapped, or a transient fetch
      // failure: cache `null` so the snapshot preview body renders and we stop
      // retrying (the snapshot list is unaffected — this only enriches rendering).
      decodedPosts = { ...decodedPosts, [postId]: null };
      logMessage('warn', 'fauna_web::feed', `decode post error (post ${postId}): ${e}`);
    }
  }

  // ── Feed selection + paging (manager-driven) ─────────────────────────────────

  async function selectFeed(feedId: string | null): Promise<void> {
    if (!manager) { loadError = t.common.still_loading; return; }
    searchInput = '';
    await manager.selectFeed(feedId ?? undefined);
    refreshFeed();
  }

  /** Select the built-in Trending virtual feed (`feed-trending-item`,
   *  trending.md § The Trending feed) — the scored sibling of the local feed
   *  over `fauna.feed.trending.posts`, mirrors `selectFeed(null)` = local. */
  async function selectTrendingFeed(): Promise<void> {
    if (!manager) { loadError = t.common.still_loading; return; }
    searchInput = '';
    await manager.selectTrendingFeed();
    refreshFeed();
  }

  function onScroll(): void {
    if (!scrollContainer || !manager) return;
    const { scrollTop, scrollHeight, clientHeight } = scrollContainer;
    const nearBottom = scrollTop + clientHeight >= scrollHeight - 200;
    if (nearBottom && status !== 'Loading' && !loadingMore && hasMore) {
      loadingMore = true;
      manager.loadMore().then(() => refreshFeed()).finally(() => { loadingMore = false; });
    }
  }

  // ── Search (server-side re-query) ────────────────────────────────────────────

  function onSearchInput(value: string): void {
    searchInput = value;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(async () => {
      if (!manager) { loadError = t.common.still_loading; return; }
      await manager.setSearchQuery(value.trim() ? value : null);
      refreshFeed();
    }, 250);
  }

  async function clearSearch(): Promise<void> {
    searchInput = '';
    clearTimeout(searchTimer);
    if (!manager) { loadError = t.common.still_loading; return; }
    await manager.clearSearch();
    refreshFeed();
  }

  // ── Compose ──────────────────────────────────────────────────────────────────

  /** The manager's own `attached_file`, carried through every compose staging
   *  on this page. A restored draft's file is a handle whose bytes this device
   *  may not hold (feed.md § Persistence), and it must reach the shared submit
   *  so the refusal there names it — staging `null` in its place would silently
   *  turn the draft into a text-only post. A fresh pick replaces it via
   *  `stageComposeFile`; `compose-file-remove` (`removeComposeFile`) is the
   *  only gesture that clears it. `snapshot()` can throw (see `refreshFeed`'s
   *  guard); then there is nothing to carry. */
  function restoredAttachment(): any {
    try {
      return manager?.snapshot()?.compose?.attached_file ?? null;
    } catch {
      return null;
    }
  }

  /** Stage a freshly picked file's hash-less handle onto the manager
   *  immediately — a draft saved before the submit must carry the file by
   *  name, or a relaunch loses it with nothing left to refuse (feed.md §
   *  Persistence → *Attachments by content address*). `file === null` is
   *  NOT the remove gesture — a file input can clear without the user
   *  choosing to drop the attachment (e.g. a cancelled re-pick) — so it
   *  restages the current answer via `restoredAttachment()` rather than
   *  nulling it; `removeComposeFile` below is the only thing that nulls it. */
  function stageComposeFile(file: File | null, data: Uint8Array | null): void {
    composeFile = file;
    composeFileData = data;
    const handle = file
      ? { name: file.name, size: file.size, blob_hash: null, media_type: null }
      : restoredAttachment();
    manager?.updateCompose(composeBody, composeTags, handle);
    refreshFeed();
    scheduleDraftSave();
  }

  /** Drop the staged/restored attachment (`compose-file-remove`) — the feed
   *  twin of the DM composer's `dm-compose-attachment-remove`. */
  function removeComposeFile(): void {
    composeFile = null;
    composeFileData = null;
    manager?.updateCompose(composeBody, composeTags, null);
    refreshFeed();
    scheduleDraftSave();
  }

  function syncCompose(): void {
    // Keep the manager's ComposeState in step while typing (clears a stale
    // compose error per the manager's update_compose contract). The staged file
    // is attached at submit, once its blob is uploaded.
    manager?.updateCompose(composeBody, composeTags, restoredAttachment());
    // Draft-persistence v2, posts rail (feed.md § Persistence): this is the
    // page's compose-mutator chokepoint (every text/tags edit funnels through
    // it, inline bar and dialog alike), so it is where the debounced
    // `fauna.drafts.put` gets scheduled — the feed twin of the conversations
    // page's `run()` → `scheduleDraftSave()` wiring. The audience has its own
    // forwarder below (`syncAudience`), fired by the audience controls only.
    scheduleDraftSave();
  }

  /** Stage the audience answer (tier / room / sale and its three fields, with
   *  the teaser) into the manager AS IT IS PICKED, not only at submit, so a
   *  half-written post's audience rides the posts draft rail across a restart
   *  (feed.md § Persistence → *Only user-authored input rests*; tui's
   *  `Action::SetGateTier` is the reference). The page-local fields stay the
   *  controls' state; `onActorChange` paints a restored audience back into
   *  them. The three setters clear each other, so exactly one answer lands. */
  function stageAudience(): void {
    if (!manager) return; // manager-gate-ok: draft forwarder; the page fields keep the pick and handleCompose re-stages it at submit
    if (sellSelected) {
      manager.updateComposeSell(true, sellPrice, sellAskingPrice, sellSubscribersFree, gatePreview);
    } else if (gateRoom) {
      manager.updateComposeRoom(gateRoom, gatePreview);
    } else {
      manager.updateComposeGate(gateTier || undefined, gatePreview);
    }
    scheduleDraftSave();
  }

  // One select change reports up to three fields (sell, room, tier) in one
  // synchronous burst; stage once, after all of them, so no half-updated
  // answer (the new sale flag beside the old room) ever reaches the manager.
  let audienceStageQueued = false;
  function syncAudience(): void {
    if (audienceStageQueued) return;
    audienceStageQueued = true;
    queueMicrotask(() => {
      audienceStageQueued = false;
      stageAudience();
    });
  }

  /** The teaser alone — shared by every restricted answer and no part of any
   *  key, so it goes through the setter that touches no answer. */
  function syncGatePreview(): void {
    manager?.updateComposePreview(gatePreview);
    scheduleDraftSave();
  }

  async function handleCompose(): Promise<void> {
    const id = $identity;
    // Split deliberately: the submit button gates on `feedReady` (the manager
    // half) but on NOTHING that tracks the identity, so `!id` reaches an
    // ENABLED submit — the click lands, the post never sends, and no
    // `compose-error` ever appears, since that surface is derived from the
    // manager's snapshot and the manager was never asked to do anything. That
    // is the silent drop convention 11 forbids; both readiness faults now
    // surface on the page's own error element. An empty body stays a plain
    // no-op — it is the one condition the button really does disable on.
    if (!manager || !id) { loadError = t.common.still_loading; return; }
    if (!composeBody.trim()) return;
    // READ ONCE, HERE — everything below sends `sent`, never the live fields.
    // The awaits ahead (provenance, thumbnail, uploads, the create, the reload)
    // leave the composer editable for seconds, and a read after any of them
    // would post whatever the user had typed since: the next post's text under
    // this post's photo. The success path below clears against the same value.
    const sent = readCompose();
    composing = true;
    let succeeded = false;
    try {
      if (sent.sellSelected) {
        // "Sell this post…" (monetization.md § Per-post pay-to-unlock — the
        // select's third answer, mutually exclusive with a tier gate by
        // construction). `prepareSellPost` runs the whole forced mint+seal
        // ordering (`FeedManager::prepare_sell_post`) and stages into the SAME
        // `pending_gated` slot as an ordinary gated compose, so it finishes
        // through the identical upload + submitGatedPost/abortGatedSubmit pair
        // below — no new upload glue (mirrors linux `client.rs::submit_post`'s
        // sell branch). An empty price is a price_hint of `undefined`, not `''`.
        manager.updateComposeSell(true, sent.sellPrice, sent.sellAskingPrice, sent.sellSubscribersFree, sent.gatePreview);
        // Empty or unparseable means no machine price — the minted tier
        // stays a tip target forever (monetization.md § The asking price).
        const askingPriceRaw = sent.sellAskingPrice.trim() ? Number(sent.sellAskingPrice.trim()) : undefined;
        const askingPriceSats = Number.isFinite(askingPriceRaw) ? askingPriceRaw : undefined;
        let sellAttached: any = restoredAttachment();
        let sealed: Uint8Array | null = null;
        try {
          if (sent.file && sent.fileData) {
            // **A sold post's photo seals under the tier the sale mints** — and
            // that tier does not exist yet, so the mint is split in two
            // (media.md § Encryption at rest: the seal is resolved before the
            // attachment is uploaded). `stageSellTier` mints and persists the
            // period key; only then can the manager seal, and only then may the
            // bytes be POSTed. With no attachment this call is skipped and
            // `prepareSellPost` runs it itself — the unchanged one-call flow.
            await manager.stageSellTier(sent.sellSubscribersFree, askingPriceSats);
            const sealedFile = await sealComposeAttachment(manager, sent.fileData);
            if (sealedFile.thumbnail) {
              try {
                await uploadBlobMultipart(id.secretHex, sealedFile.thumbnail.sidecar, sealedFile.thumbnail.bytes);
              } catch (e) {
                logMessage('warn', 'fauna_web::feed', `sold blob upload: thumbnail failed (non-fatal): ${e}`);
              }
            }
            const fileHash = await uploadBlobMultipart(id.secretHex, sealedFile.sidecar, sealedFile.bytes);
            sellAttached = {
              name: sent.file.name,
              size: sent.fileData.length,
              blob_hash: fileHash,
              // The PLAINTEXT's type — the sealed sidecar says
              // `application/octet-stream`, and `File.type` describes bytes the
              // nest never sees.
              media_type: sealedFile.mime,
            };
          }
          manager.updateCompose(sent.body.trim(), sent.tags, sellAttached);
          sealed = await manager.prepareSellPost(
            sent.sellPrice.trim() ? sent.sellPrice.trim() : undefined,
            sent.sellSubscribersFree,
            askingPriceSats,
          );
        } catch (e) {
          logMessage('warn', 'fauna_web::feed', `sell compose rejected: ${e}`);
        }
        if (sealed) {
          try {
            // The class comes off the staged post (`gatedUploadSidecar`) —
            // `GroupRestrictedPost` for a room post, the tier class otherwise —
            // never decided here.
            const sidecar = manager.gatedUploadSidecar();
            const hash = await uploadBlobMultipart(id.secretHex, sidecar, sealed);
            await manager.submitGatedPost(hash);
            succeeded = true;
          } catch (e) {
            manager.abortGatedSubmit(String(e));
            logMessage('warn', 'fauna_web::feed', `sold post upload failed: ${e}`);
          }
        }
      } else if (sent.gateTier || sent.gateRoom) {
        // Gate-to-tier (feed.md § Encryption at rest; monetization.md § Pillars
        // 2+3 — client UX): the manager seals the full body under the tier's
        // period key (`prepareGatedBlob`), the SPA uploads the sealed blob on the
        // bulk plane with the shared `PeriodRestrictedPost` sidecar, then
        // `submitGatedPost` creates the post once the nest echoes the content
        // address. Mirrors linux `client.rs::submit_post`'s gate branch.
        //
        // **The gate is staged BEFORE the attachment is sealed, and that order
        // is load-bearing** (media.md § Encryption at rest): the audience
        // decides the seal, so `sealComposeAttachment` has to be able to read
        // it. The public branch below uploads at submit too, but under
        // `processAndSealPublicPost` — a plaintext blob, which for a
        // tier-restricted post would be a readable copy of the picture that no
        // blob DELETE exists to remove. Until 2026-09-07 this branch passed
        // `null` and simply dropped the photo instead.
        //
        // A ROOM answer rides this same branch (feed.md § Encryption at rest →
        // *Room-restricted — the app half*): staged through its own setter, it
        // sends the shared room arm of `prepareGatedBlob` to the room's key, and
        // the staged sidecar below names its class — no branch of its own.
        if (sent.gateRoom) {
          manager.updateComposeRoom(sent.gateRoom, sent.gatePreview);
        } else {
          manager.updateComposeGate(sent.gateTier, sent.gatePreview);
        }
        let gatedAttached: any = restoredAttachment();
        if (sent.file && sent.fileData) {
          // The manager owns this seal: the tier's period key never crosses
          // the wasm boundary, so the SPA only transports bytes it cannot
          // open. `mime` comes back as the PLAINTEXT's type — the sealed
          // sidecar says `application/octet-stream`, and `File.type`
          // describes bytes the nest never sees.
          const sealedFile = await sealComposeAttachment(manager, sent.fileData);
          if (sealedFile.thumbnail) {
            try {
              await uploadBlobMultipart(id.secretHex, sealedFile.thumbnail.sidecar, sealedFile.thumbnail.bytes);
            } catch (e) {
              logMessage('warn', 'fauna_web::feed', `gated blob upload: thumbnail failed (non-fatal): ${e}`);
            }
          }
          const fileHash = await uploadBlobMultipart(id.secretHex, sealedFile.sidecar, sealedFile.bytes);
          gatedAttached = {
            name: sent.file.name,
            size: sent.fileData.length,
            blob_hash: fileHash,
            media_type: sealedFile.mime,
          };
        }
        manager.updateCompose(sent.body.trim(), sent.tags, gatedAttached);
        let sealed: Uint8Array | null = null;
        try {
          sealed = await manager.prepareGatedBlob();
        } catch (e) {
          // A validation error (empty preview / this device lacks the tier key)
          // is already stamped on compose-error by the manager; nothing was
          // staged, so there is nothing to abort.
          logMessage('warn', 'fauna_web::feed', `gated compose rejected: ${e}`);
        }
        if (sealed) {
          try {
            // The class comes off the staged post (`gatedUploadSidecar`) —
            // `GroupRestrictedPost` for a room post, the tier class otherwise —
            // never decided here.
            const sidecar = manager.gatedUploadSidecar();
            const hash = await uploadBlobMultipart(id.secretHex, sidecar, sealed);
            await manager.submitGatedPost(hash);
            succeeded = true;
          } catch (e) {
            // Upload/create failed after the seal: abort the staged submit onto
            // compose-error, keeping the composer text for a manual retry.
            manager.abortGatedSubmit(String(e));
            logMessage('warn', 'fauna_web::feed', `gated post upload failed: ${e}`);
          }
        }
      } else {
        let attached: any = restoredAttachment();
        if (sent.file && sent.fileData) {
          // The blob upload stays client glue (the SPA signs/uploads in WASM); the
          // manager builds the media post from the staged metadata. Feed posts are
          // public, so the blob rides the PublicPost audience (plaintext passthrough).
          const data = sent.fileData;
          const mimeHint = sent.file.type || '';
          const hasC2pa = mimeHint.startsWith('image/')
            ? (await readProvenanceFromBytes(data, mimeHint)) !== null
            : false;
          const { sidecar, bytes, mime, thumbnail } = await processAndSealPublicPost(data, mimeHint, hasC2pa);
          // The thumbnail goes up FIRST and is best-effort — the same order and
          // error policy as native `fauna_client::upload_public_post_blob`
          // (priority #1/#3). `sidecar` already routes at these bytes, and the
          // nest records that pointer when it ingests the primary below, so
          // landing the thumbnail first means `?thumb=1` never sees a pointer to
          // a blob that is not in the store. A thumbnail failure must not fail
          // the post: the nest simply serves the full-size original.
          if (thumbnail) {
            try {
              await uploadBlobMultipart(id.secretHex, thumbnail.sidecar, thumbnail.bytes);
            } catch (e) {
              logMessage('warn', 'fauna_web::feed', `blob upload: thumbnail failed (non-fatal): ${e}`);
            }
          }
          const hash = await uploadBlobMultipart(id.secretHex, sidecar, bytes);
          attached = { name: sent.file.name, size: data.length, blob_hash: hash, media_type: mime };
        }
        manager.updateCompose(sent.body.trim(), sent.tags, attached);
        await manager.submitPost();
        succeeded = true;
      }
    } catch (e) {
      // The manager's own failures ARE on snapshot.compose.error (→
      // compose-error): `FeedManager::submit_post` stamps every path it owns
      // (manager.rs § the `Err(e)` arm). What that assumption misses is a throw
      // on the way **IN** — `updateCompose`/`submitPost` rejecting before the
      // manager ever ran, a stale wasm interface after a torn rebuild, a plain
      // TypeError. Then compose.error stays EMPTY and this catch is the whole
      // of the user's feedback… except `logMessage` writes to the WASM log
      // ring, which no page renders and no e2e can read. Net effect: the click
      // lands, the post never sends, and NOTHING anywhere says so — the silent
      // drop convention 11 forbids, wearing the costume of "the snapshot has
      // it". Both halves are closed here:
      //   * `console.warn` so the reason reaches the bridge-captured console
      //     ring a failing test dumps (`drivers/web.py::console_log`);
      //   * the page's own error surface, but ONLY when the manager did not
      //     already stamp its own — read straight off the manager rather than
      //     the `composeError` derived, whose recompute we would be racing.
      console.warn('[feed] submit post failed:', e);
      logMessage('warn', 'fauna_web::feed', `submit post failed: ${e}`);
      let stamped = false;
      try {
        stamped = !!manager?.snapshot()?.compose?.error;
      } catch {
        /* snapshot itself is unreadable — then it certainly did not stamp */
      }
      if (!stamped) loadError = t.feed.error_submit({ message: String(e) });
    } finally {
      if (succeeded) {
        // Clear what was SENT, not the composer as it stands now (a gated
        // failure keeps everything for a retry). This used to reset every field
        // unconditionally, and this `finally` runs only when the whole submit
        // resolves — after the create AND the post-submit reload. A reload the
        // page itself started later can put the new post on screen first, and
        // this submit then waits out its own superseded fetch; anything the user
        // typed or picked for their NEXT post in that gap was erased, silently.
        // `test_feed.py`'s second compose in a journey measured it: the button
        // disabled for the full 90 s, `compose-file-ready` gone, no error.
        const next = clearSent(readCompose(), sent, EMPTY_COMPOSE, COMPOSE_STICKY);
        writeCompose(next);
        if (next.file === null) {
          const fileInput = document.querySelector(`[data-testid="${IDS.COMPOSE_FILE}"]`) as HTMLInputElement | null;
          if (fileInput) fileInput.value = '';
        }
        // Hand the result back to the manager and persist it. `submitPost`
        // reset the manager's ComposeState at the create, so a kept edit is
        // otherwise text the manager never sees until the next keystroke. And
        // (draft-persistence v2, feed.md § Persistence) web's save is
        // chokepoint-driven, not observer-driven off every manager change the
        // way tui/linux/android's is — without this, the pre-submit draft
        // stays the persisted `__drafts` blob and a restart right after posting
        // would restore already-published text. `syncCompose` does both, and
        // `stageAudience` hands back the audience answer the clear kept.
        syncCompose();
        stageAudience();
      }
      refreshFeed();
      composing = false;
    }
  }

  async function handleDialogDrop(e: DragEvent): Promise<void> {
    e.preventDefault();
    dialogDragging = false;
    const file = e.dataTransfer?.files?.[0] ?? null;
    if (!file) return;
    const buf = await file.arrayBuffer();
    stageComposeFile(file, new Uint8Array(buf));
  }

  // ── Interactions (like/reply/repost/quote) ──
  //
  // Through the manager, NOT a direct `interactWithPost` RPC. The four counts on
  // the bar render from the snapshot (`PostSummary.{like,reply,repost,quote}
  // _count`, feed.md § Interaction bar), and `manager.interact` is what folds
  // the nest's post-act counters back into it — the old direct call threw the
  // reply away, which is why a tapped ♥ stayed at 0 here. `refreshFeed()` pulls
  // the re-notified snapshot in, mirroring `handleDeletePost`.
  //
  // `reply`/`quote` are COMPOSED, not recorded (feed.md § Implementation status
  // today): `interact(id, "reply"/"quote", body)` looks identical and creates
  // NOTHING — the nest's native arm discards `body` outright. `manager.quote`
  // fires immediately with empty commentary (the quote-commentary composer is a
  // deferred fleet-wide follow-on, § Layout & flow); `manager.reply` needs typed
  // text, so `reply` opens the compose dialog instead of firing immediately
  // (linux twin: `client.rs::interact_with_post` + `build_reply_dialog`).
  async function handleInteract(postId: string, action: string): Promise<void> {
    if (!manager) { loadError = t.common.still_loading; return; }
    if (action === 'reply') {
      replyTargetId = postId;
      replyText = '';
      return;
    }
    try {
      // Two verbs, not one (feed.md § User actions). `like` is the manager's
      // TOGGLE off `viewer_liked`: both directions ride the same interact door
      // on the same post id, so nothing is composed — but the bare `interact`
      // call below can only ever like, never un-like, because the nest's like
      // arm is idempotent per (actor, post). That made a like permanent here.
      // `quote` COMPOSES a post carrying `Reference::Quote`; routed through
      // `interact` it creates nothing at all on a native post.
      if (action === 'like') {
        await manager.like(postId);
      } else if (action === 'quote') {
        await manager.quote(postId, '');
      } else if (action === 'repost') {
        // `repost` is the manager's TOGGLE off `viewer_repost_id` (feed.md
        // § Interaction bar → Repost, ratified 2026-08-10): off → composes the
        // caller's empty-body `Reference::Repost` post; on → un-reposts it —
        // never routed through the bare `interact` below, which creates
        // nothing on a native post.
        await manager.repost(postId);
      } else {
        await manager.interact(postId, action, null);
      }
      loadError = '';
    } catch (e) {
      logMessage('warn', 'fauna_web::feed', `interact error: ${e}`);
      loadError = verbErrorCopy(e);
    } finally {
      refreshFeed();
    }
  }

  // ── Gated post unlock (feed.md § Encryption at rest; monetization.md § Pillars
  //    2+3). Web twin of linux `client.rs::unlock_gated_post`: resolve the sealed
  //    blob hash (one lazy fauna.posts.get + decode), fetch its bytes on the bulk
  //    plane, and hand them to the manager to decrypt + swap the full body into the
  //    snapshot; the refresh repaints the open detail. Best-effort — a locked post
  //    (not entitled / rotated-out period) just keeps its teaser. ──
  async function unlockGated(postId: string): Promise<void> {
    const id = $identity;
    if (!manager || !id) return; // manager-gate-ok: best-effort detail augmentation — the post keeps its teaser
    try {
      const hash = await manager.gatedBlobHash(postId);
      if (!hash) return;
      const bytes = await fetchBlob(id.secretHex, hash);
      await manager.unlockGatedPost(postId, bytes);
    } catch (e) {
      logMessage('warn', 'fauna_web::feed', `gated unlock (post stays teased): ${e}`);
    } finally {
      refreshFeed();
    }
  }

  // ── The self-serve teaser purchase (gap (2c), monetization.md § Per-post
  //    pay-to-unlock → the buyer's price read is post-addressed):
  //    `gated-post-buy-button` subscribes against the resolved offer's
  //    tier_name — the existing subscribe flow, no claim code needed. ──
  async function buyUnlockOffer(postId: string): Promise<void> {
    if (!manager) { loadError = t.common.still_loading; return; }
    try {
      await manager.buyUnlockOffer(postId);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      loadError = t.feed.error_buy_unlock({ message: msg });
    } finally {
      // The manager clears the post's `unlock_offer` and re-notifies on
      // success, so a landed purchase un-renders its own affordance (nothing
      // left to buy; re-showing the price invites a duplicate subscribe).
      // Web is the only client that must pull that in by hand — tui and linux
      // register a `FeedSnapshotObserver` and repaint off `notify()`, while
      // the wasm face has no observer seam at all and the browser owns the
      // loop instead (see the snapshot-derived view block at the top of this
      // file: every async manager method is followed by `refreshFeed()`).
      // Without it the buy button stays on screen forever after a *successful*
      // buy — the shape every sibling handler here already avoids with this
      // same `finally`.
      refreshFeed();
    }
  }

  // The payment link is nest/author-supplied; refuse to open a non-https
  // scheme (security.md § Transport trust) —
  // the same check the profile page's subscription-offer-payment-link
  // applies before opening a payment link.
  function openUnlockPaymentLink(url: string): void {
    if (typeof window === 'undefined') return;
    if (!isSafeNavUrl(url)) {
      loadError = t.subscriptions.unsafe_payment_url;
      return;
    }
    window.open(url, '_blank', 'noopener');
  }

  // ── Create-feed form ───────────────────────────────────────────────────────

  function addRule(): void {
    const input = newRuleInput.trim();
    let rule: FilterRuleInput;
    switch (newRuleType) {
      case 'HasMedia':
      case 'IsReply':
        rule = { rule_type: newRuleType, value: '', required: newRuleRequired };
        break;
      case 'MinReplies':
      case 'MinReposts':
      case 'CreatedAfter':
        rule = { rule_type: newRuleType, value: String(newRuleCount), required: false };
        break;
      case 'LabelBelow':
      case 'LabelAbove':
        // The shared encoder packs the label rule as "category:threshold" (0–10).
        rule = { rule_type: newRuleType, value: `${input || 'spam'}:${newRuleCount}`, required: false };
        break;
      default:
        // HasHashtag / Source / BodyContains / BodyExcludes — comma-separated.
        rule = { rule_type: newRuleType, value: input, required: false };
        break;
    }
    newFeedRules = [...newFeedRules, rule];
    newRuleInput = '';
    newRuleCount = 1;
    newRuleRequired = true;
  }

  function removeRule(i: number): void {
    newFeedRules = newFeedRules.filter((_, idx) => idx !== i);
  }

  async function handleCreateFeed(): Promise<void> {
    if (!manager) { loadError = t.common.still_loading; return; }
    if (!newFeedName.trim()) return;
    creatingFeed = true;
    try {
      await manager.createFeed(
        newFeedName.trim(), newFeedRules, newFeedCombination,
        undefined, undefined, newFeedFactors,
      );
      newFeedName = '';
      newFeedRules = [];
      newFeedFactors = [];
      newFeedCombination = 'all';
      showCreateForm = false;
    } catch (e) {
      logMessage('warn', 'fauna_web::feed', `create feed failed: ${e}`);
    } finally {
      refreshFeed();
      creatingFeed = false;
    }
  }

  /** Own-post delete (feed-post-delete-confirm-button; feed.md § State & data
   *  shape → Post deletion). The manager drops the post from its loaded window
   *  and re-notifies on success — `refreshFeed()` just pulls that in, mirroring
   *  `handleDeleteFeed`. */
  async function handleDeletePost(postId: string): Promise<void> {
    if (!manager) { loadError = t.common.still_loading; return; }
    try {
      await manager.deletePost(postId);
    } catch (e) {
      logMessage('warn', 'fauna_web::feed', `delete post failed: ${e}`);
    } finally {
      refreshFeed();
    }
  }

  // ── Own-post web-publishing verbs (web-content-hosting.md
  //    § Published-post management; `$lib/web-publish` is the shared
  //    origin/posts store the `web-settings` section also reads/writes). ──

  // The last web/paywall link copied from the ⋯ menu, keyed by post so a
  // re-render can tell which card's `copied` attr + confirmation line to show.
  let webLinkCopied = $state<{ postId: string; kind: 'web' | 'paywall'; url: string } | null>(
    null,
  );

  async function publishPostToWeb(postId: string): Promise<void> {
    try {
      // `null` slug: the default is the nest's to mint, never a client-chosen one.
      await webPublishSet($identity?.secretHex ?? '', hexToBytes(postId), null);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      loadError = t.web_publish.error_publish({ message: msg });
    } finally {
      // Re-read rather than editing locally: the card's `web_slug` — which
      // drives the whole verb family — repaints from the nest's own answer.
      refreshFeed();
    }
  }

  async function unpublishPostFromWeb(postId: string): Promise<void> {
    try {
      await webPublishUnset($identity?.secretHex ?? '', hexToBytes(postId));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      loadError = t.web_publish.error_unpublish({ message: msg });
    } finally {
      refreshFeed();
    }
  }

  // Purely local — the origin and the slug are both already resolved on
  // screen, so the public link costs no round trip.
  async function copyPostWebLink(postId: string): Promise<void> {
    const origin = $siteLink?.origin;
    const post = posts.find((p: any) => p.post_id === postId);
    if (!origin || !post?.web_slug) return;
    const url = webPostPageUrl(origin, post.web_slug);
    await bestEffortCopyToClipboard(url);
    webLinkCopied = { postId, kind: 'web', url };
  }

  // A fresh mint per click: the token is short-lived by ratified design and
  // re-minting is free, so re-copying always yields a link that works from
  // now rather than a cached one that already expired.
  async function copyPostPaywallLink(postId: string): Promise<void> {
    const post = posts.find((p: any) => p.post_id === postId);
    const origin = $siteLink?.origin;
    if (!origin || !post?.web_slug) return;
    try {
      const minted = await webPaywallMintToken($identity?.secretHex ?? '', post.web_slug);
      const url = webTokenedUrl(origin, minted.path, minted.token);
      await bestEffortCopyToClipboard(url);
      webLinkCopied = { postId, kind: 'paywall', url };
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      loadError = t.web_publish.error_paywall_link({ message: msg });
    }
  }

  async function handleDeleteFeed(feedId: string): Promise<void> {
    if (!manager) { loadError = t.common.still_loading; return; }
    try {
      await manager.deleteFeed(feedId);
      // The manager re-selects nothing automatically; if the deleted feed was
      // active, fall back to the local feed.
      if (selectedFeed === feedId) await manager.selectFeed(undefined);
    } catch (e) {
      logMessage('warn', 'fauna_web::feed', `delete feed failed: ${e}`);
    } finally {
      refreshFeed();
    }
  }

  // ── Bridge subscription ──────────────────────────────────────────────────────

  async function handleSubscribeBridgeFeed(): Promise<void> {
    // Reachable with a null manager for the same reason as submitReply: the
    // subscribe button gates on the two form fields only. The refusal goes on
    // the form's own error paragraph, which is already rendered right above
    // the button, rather than the page-level surface.
    if (!manager) { bridgeFormError = t.common.still_loading; return; }
    if (!bridgeFormUri.trim() || !bridgeFormName.trim()) return;
    bridgeFormSaving = true;
    try {
      await manager.subscribeBridge(bridgeFormBridge, bridgeFormUri.trim(), bridgeFormName.trim());
      bridgeFormUri = '';
      bridgeFormName = '';
      showBridgeSubscribeForm = false;
    } catch (e) {
      // bridge_form.error (→ the inline error) is set in the snapshot.
      logMessage('warn', 'fauna_web::feed', `subscribe bridge failed: ${e}`);
    } finally {
      refreshFeed();
      bridgeFormSaving = false;
    }
  }

  async function handleUnsubscribeBridgeFeed(id_: number): Promise<void> {
    if (!manager) { loadError = t.common.still_loading; return; }
    try {
      await manager.unsubscribeBridge(id_);
    } catch (e) {
      logMessage('warn', 'fauna_web::feed', `unsubscribe bridge failed: ${e}`);
    } finally {
      refreshFeed();
    }
  }

  // ── Lifecycle ──────────────────────────────────────────────────────────────

  /// One guarded step of [`loadAll`]. Each step is isolated so a single failure
  /// can neither abort the rest of the load nor vanish silently.
  ///
  /// `loadAll` used to be a bare chain of `await`s, called from the async
  /// actor-change callback in `onMount` (then a raw `identity.subscribe`, now
  /// `onActorChange`). Nobody awaits that callback's
  /// promise, so a throw in ANY step rejected into nothing: no `error-message`, no
  /// console path the harness reads, and — because the feed query is the LAST
  /// step — no posts either. The only observable was "the manager rebuilt but its
  /// first query came back empty" (0 posts, `error-message=''`), which reads as a
  /// product bug in whatever test happens to hit it. The asymmetry that made it
  /// fire for a second actor and not the first is the point: the four pre-reads
  /// are actor-scoped (own tiers, subscribed bridges), the feed query is not.
  /// E2E convention 2 — a failure must reach `error-message`.
  async function loadStep(run: () => Promise<void>): Promise<void> {
    try {
      await run();
    } catch (e) {
      const msg = e instanceof Error ? e.message : t.common.load_failed;
      loadError = msg;
      logMessage('warn', 'fauna_web::feed', `feed load step failed: ${e}`);
    }
  }

  async function loadAll(): Promise<void> {
    const m = manager;
    if (!m) return;
    loadError = '';
    await loadStep(() => m.refreshFeeds());
    await loadStep(() => m.refreshBridgeFeeds());
    await loadStep(() => m.refreshAvailableBridges());
    // Refresh the composer's gate-to-tier options (`compose-gate-tier-select`).
    // `selectFeed` → `reload` also refreshes own_tiers, but call it explicitly so
    // a tier the user just minted on the Subscriptions page is in the option set
    // on the next feed load — the feed route remounts on nav, re-running loadAll
    // (feed.md § Encryption at rest).
    await loadStep(() => m.refreshOwnTiers());
    // Unconditional, and deliberately last-but-guarded: the feed query is the
    // page's whole purpose, so none of the pre-reads above may suppress it.
    await loadStep(() => m.selectFeed(undefined)); // the nest's local feed
    refreshFeed();
  }

  onMount(async () => {
    // REGISTERED BEFORE THE FIRST AWAIT, deliberately. `onActorChange` also
    // fires the handler immediately when an identity is already present, and
    // that fallback is what made a late registration look correct — but it
    // only ever covers "the identity was already there", never "the identity
    // arrived while I was booting". Awaiting first opens a window the width of
    // a whole wasm module instantiation, in which an identity change fires no
    // drop and no rebuild for this page and NOTHING says so — the silent-by-
    // construction outcome `account-scoping.md` § The scoping taxonomy bans.
    // `media/+page.svelte` and `+layout.svelte` already register this early;
    // this page is held to the same shape by
    // `$lib/actor-scope-registration-contract.test.ts`. Nothing below needs
    // wasm to REGISTER — the handlers await it themselves where they need it.
    //
    // The three `[feed] mount` lines below are the page's boot witness, and they
    // are permanent for the same reason the `[launch]` / `[actor-scope]` /
    // `[succession]` lines are: the failure this page had
    // left `feedReady` AND `loadError` both unset, which is what "the handler
    // never ran" looks like from the outside — indistinguishable, without them,
    // from "the handler ran and the feed is genuinely empty". The e2e harness
    // captures the browser console ring, so a future red says which of the two
    // it is in its own failure output rather than needing another run to ask.
    console.debug('[feed] mount: registering the actor-scope seam');
    identity.init();

    // Build the manager for whoever is signed in, now and on every later actor
    // CHANGE — not once per mount.
    //
    // The guard here used to be `if (id && !manager)`, written for "build once the
    // identity is first known" — correct only while this component is guaranteed
    // to unmount between actors. It is not: when the route does NOT remount, the
    // component-local `manager` is still set, so a second actor's login skipped
    // this whole body — no `getFeedManager()`, no `loadAll()`. Combined with the
    // module-level manager reset that a real switch performs, the snapshot is
    // cleared and never refilled, so the page renders 0 posts with NO error —
    // indistinguishable from "this actor has an empty feed", and the reason the
    // multi-actor e2e arms read as product bugs.
    //
    // The `secretHex` keying and the cache drop that fixed that are now the SPA's
    // one shape rather than this page's private one (`$lib/actorScope` +
    // `onActorChange`, account-scoping.md § The scoping taxonomy's in-memory
    // corollary): `resetActorScopedAugmentation` is registered as a drop, so it
    // runs — with every other surface's drop — before any rebuild handler.
    unsubActorReset = registerActorScopedReset(resetActorScopedAugmentation);
    unsubIdentity = onActorChange(async (id) => {
      // GUARDED, because `store.ts` VOIDS this promise — its own `onActorChange`
      // doc says so: "Its promise is not awaited — guard each step that can
      // throw, or the rejection vanishes". Unguarded, a rejected build took the
      // whole rebuild with it: no `feedReady`, no `loadAll()`, and NOTHING said
      // so — `feed-view` rendered, `error-message` stayed empty, and
      // `post-submit-button` sat disabled for the full 90 s harness ceiling. That
      // is the silent shape `web.md` § Async manager readiness bans for a
      // handler's catch ("a throw on the way IN … leaves that surface empty"),
      // and it is why the cluster read as a composer bug for three runs.
      //
      // Both halves the goal doc requires: the page's own writable error (the
      // manager never existed, so its snapshot cannot have stamped one), and the
      // BROWSER console — `logMessage` alone writes to the wasm log ring, which
      // no page renders and no e2e can read, so it would have left this exactly
      // as silent as no catch at all.
      try {
        manager = await getFeedManager();
      } catch (e) {
        loadError = t.feed.error_load({ message: String(e) });
        console.warn('[feed] manager build failed; page not ready:', e);
        logMessage('warn', 'fauna_web::feed', `feed manager build failed: ${e}`);
        return;
      }
      feedReady = true;
      void startCueCapture(manager);
      // Draft-persistence v2, posts rail (feed.md § Persistence): `composeBody`
      // / `composeTags` are plain page-local state (never snapshot-derived, the
      // way the conversations page's composer is), so the restore that
      // `getFeedManager()` already ran on this manager needs an explicit
      // read-back here or the textarea stays empty despite the manager holding
      // the restored draft. `getFeedManager()`'s promise only resolves after
      // its own restore attempt (success or swallowed failure) completes, so
      // this snapshot is always the final restored (or empty, on a fresh
      // manager / first run) state — never a mid-restore read. Runs on every
      // actor change so a switch shows THAT actor's own draft, never a leaked
      // previous one. `snapshot()` can throw (see `refreshFeed`'s own guard);
      // failure here just leaves an empty composer, matching the swallowed
      // restore failure above it.
      try {
        const restoredCompose = manager.snapshot().compose;
        composeBody = restoredCompose.text;
        composeTags = restoredCompose.tags;
        // The restored audience, painted into the page-local controls the
        // way the text and tags are (feed.md § Persistence).
        const restoredSell = restoredCompose.sell ?? null;
        sellSelected = restoredSell !== null;
        sellPrice = restoredSell?.price ?? '';
        sellAskingPrice = restoredSell?.asking_price ?? '';
        sellSubscribersFree = restoredSell?.subscribers_get_it_free ?? true;
        gateRoom = restoredCompose.gate_room ?? '';
        gateTier = restoredCompose.gate_tier ?? '';
        gatePreview = restoredCompose.gate_preview ?? '';
      } catch (e) {
        logMessage('warn', 'fauna_web::feed', `read restored compose failed: ${e}`);
      }
      // Hydrate the content-policy render state (guardian floor + own spam
      // thresholds) BEFORE the first feed load, so posts render with the right
      // verdict on their first paint (family-safety.md § Content policy).
      await hydrateContentPolicy(id.secretHex);
      notifyBuffer.setIdentity(id.secretHex);
      // Lazily resolves the SAME origin the ⋯-menu's copy affordances need
      // (web-content-hosting.md § Published-post management) — fired here
      // rather than only on first menu-open, so an own post's card doesn't
      // need a new "menu opened" callback just to trigger it. Failure is not
      // fatal: the verbs simply render as "no origin" until a later visit to
      // Settings → Web succeeds.
      void ensureWebPageHydrated().catch((e) =>
        logMessage('warn', 'fauna_web::feed', `web-publish origin hydrate failed: ${e}`),
      );
      // The room-post seam (feed.md § Encryption at rest → *Room-restricted —
      // the app half*): installed BEFORE the first load, so `refreshFeeds`'
      // own-rooms read already sees the user's rooms. A no-op until the
      // conversations manager exists; its own tick installs it then.
      syncFeedRoomPosts();
      await loadAll();
      // A `SearchNav::Post` deep-link left by the Search page
      // (`$lib/search`'s `consumePendingSearchNav`) — after `loadAll()` so
      // `openPostDetail`'s own `resolvePost` call (which it runs regardless)
      // sees the freshest loaded window before deciding whether a fetch is
      // needed.
      const pendingNav = consumePendingSearchNav();
      if (pendingNav?.kind === 'post') await openPostDetail(pendingNav.postId);
    });

    console.debug('[feed] mount: seam registered, awaiting wasm');
    // Only now the module itself: the seam above is live, so an identity that
    // lands during this instantiation is already heard.
    await ensureWasm();
    ruleTypeOptionsList = ruleTypeOptions();
    factorOptions = builtinFactorEntries();
    console.debug('[feed] mount: complete');

    // Re-hydrate the feed on every reconnect — the wasm twin of native
    // `subscribe_reconnects` (transport.md § Push events). The feed has no push
    // event + no poll backstop, so a post that arrived while disconnected stays
    // invisible until a re-query. Skip the store's seed value.
    // Re-pull the CURRENT SOURCE via the shared `refreshCurrentFeed` seam —
    // never `selectFeed(selectedFeed)`: that field is undefined both for the
    // local feed and while the built-in Trending virtual feed is selected, so
    // re-selecting it drops a Trending viewer into Local on every reconnect
    // (trending.md § The Trending feed).
    let firstTick = true;
    unsubReconnect = reconnectTick.subscribe(() => {
      if (firstTick) { firstTick = false; return; }
      if (!manager) return; // manager-gate-ok: reconnect-tick subscription, not a user action
      manager.refreshCurrentFeed().then(() => refreshFeed());
    });
  });

  // The store-change notice (`$lib/store-change`): the sealed scorers — muted
  // keywords, trained factors — load only inside the manager's reload, so the
  // open feed re-pulls its current source, the reconnect arm's own re-drive.
  const unsubStoreChange = onStoreChange(() => {
    if (!manager) return; // manager-gate-ok: store-change subscription, not a user action
    manager.refreshCurrentFeed().then(() => refreshFeed());
  });

  onDestroy(() => {
    destroyed = true;
    stopCueCapture();
    unsubReconnect?.();
    unsubStoreChange();
    unsubIdentity?.();
    unsubActorReset?.();
    clearTimeout(searchTimer);
    // Opened post media (the conversations-attachment rule): a reactive re-render
    // must not leak a fresh blob URL each tick, and these hold decrypted bytes.
    for (const u of Object.values(sealedMediaUrls)) URL.revokeObjectURL(u);
    for (const u of Object.values(proxiedMediaUrls)) URL.revokeObjectURL(u);
    notifyBuffer.destroy();
  });
</script>

{#if pageError}
  <p class="error-text page-error" data-testid={IDS.ERROR_MESSAGE}>{pageError}</p>
{/if}

<div class="feed-shell" data-testid={IDS.FEED_VIEW}>
  <!-- Left panel: feed sidebar -->
  <aside class="feed-sidebar">
    <div class="sidebar-header">
      <span class="sidebar-title" data-testid={IDS.PAGE_HEADING}>{t.feed.list.title}</span>
      <button data-testid={IDS.FEED_CREATE_FEED_BUTTON} class="icon-btn" title={t.feed.post.create_tooltip} onclick={() => { showCreateForm = !showCreateForm; if (showCreateForm) ensureFactorOptions(); }}>+</button>
    </div>

    {#if showCreateForm}
      <div class="create-form">
        <h3 class="create-form-title">{t.feed.create.title}</h3>
        <label class="field-label" for="feed-create-feed-name-input">{t.feed.create.feed_name}</label>
        <input
          id="feed-create-feed-name-input"
          data-testid={IDS.FEED_CREATE_FEED_NAME}
          class="text-input"
          type="text"
          placeholder={t.feed.create.name_placeholder}
          bind:value={newFeedName}
        />
        <select class="select-input" data-testid={IDS.FEED_COMBINATION_SELECT} bind:value={newFeedCombination}>
          <option value="all">{t.feed.create.mode_all}</option>
          <option value="any">{t.feed.create.mode_any}</option>
        </select>

        {#each newFeedRules as rule, i}
          <div class="rule-chip">
            <span>{resolveLocalized(ruleSummaryLabel(rule.rule_type, rule.value, rule.required))}</span>
            <button class="remove-btn" onclick={() => removeRule(i)}>✕</button>
          </div>
        {/each}

        <div class="add-rule-row">
          <select class="select-input" data-testid={IDS.FEED_RULE_TYPE_SELECT} bind:value={newRuleType}>
            {#each ruleTypeOptionsList as rt}
              <option value={rt.value}>{resolveLocalized(rt.label)}</option>
            {/each}
          </select>
          {#if newRuleInputKind === 'Toggle'}
            <label class="combo-label">
              <input type="checkbox" data-testid={IDS.FEED_RULE_REQUIRED_TOGGLE} bind:checked={newRuleRequired} /> {resolveLocalized(ruleRequiredLabel(newRuleRequired))}
            </label>
          {:else}
            {#if newRuleInputKind === 'Text' || newRuleInputKind === 'TextAndNumber'}
              <input class="text-input small" data-testid={IDS.FEED_RULE_VALUE_INPUT} placeholder={t.feed.create.rule_value_placeholder} bind:value={newRuleInput} />
            {/if}
            {#if newRuleInputKind === 'Number'}
              <input class="text-input small" data-testid={IDS.FEED_RULE_VALUE_INPUT} type="number" min="1" bind:value={newRuleCount} />
            {/if}
            {#if newRuleInputKind === 'TextAndNumber'}
              <input class="text-input small" data-testid={IDS.FEED_RULE_THRESHOLD_INPUT} type="number" min="0" max="10" step="1" placeholder={t.feed.create.rule_threshold} bind:value={newRuleCount} />
            {/if}
          {/if}
          <button class="btn-secondary small" data-testid={IDS.FEED_ADD_RULE_BUTTON} disabled={!canAddRuleNow} onclick={addRule}>{t.feed.create.add_rule}</button>
        </div>

        <h4 class="field-label">{t.feed.create.factors}</h4>
        {#each newFeedFactors as factor, i}
          <div class="rule-chip">
            <span>{factorLabel(factor)}</span>
            <button class="remove-btn" onclick={() => removeFactor(i)}>✕</button>
          </div>
        {/each}
        <div class="add-rule-row">
          <select class="select-input" data-testid={IDS.FEED_FACTOR_SELECT} bind:value={newFactorKey}>
            {#each factorOptions as opt}
              <option value={opt.key}>{opt.label}</option>
            {/each}
          </select>
          <input
            class="text-input small"
            data-testid={IDS.FEED_FACTOR_WEIGHT_INPUT}
            type="text"
            placeholder={t.feed.create.factor_weight_placeholder}
            bind:value={newFactorWeight}
          />
          <label class="combo-label">
            <input type="checkbox" data-testid={IDS.FEED_FACTOR_GLOBAL_TOGGLE} bind:checked={newFactorGlobal} /> {t.feed.create.factor_global_toggle}
          </label>
          <button class="btn-secondary small" data-testid={IDS.FEED_ADD_FACTOR_BUTTON} onclick={addFactor}>{t.feed.create.add_factor}</button>
        </div>

        <div class="form-actions">
          <button data-testid={IDS.CREATE_FEED} class="btn-primary" disabled={creatingFeed || !newFeedName.trim() || !feedReady} onclick={handleCreateFeed}>
            {creatingFeed ? t.common.saving : t.common.save}
          </button>
          <button class="btn-secondary" data-testid={IDS.FEED_CREATE_CANCEL} onclick={() => { showCreateForm = false; newFeedName = ''; newFeedRules = []; newFeedFactors = []; }}>
            {t.common.cancel}
          </button>
        </div>
      </div>
    {/if}

    <!-- Trending built-in virtual feed (trending.md § The Trending feed) —
         above the user's own feeds, mutually exclusive with Local/custom
         selection via snapshot.trending_selected. -->
    <button
      data-testid={IDS.FEED_TRENDING_ITEM}
      class="feed-entry"
      class:active={trendingSelected}
      onclick={selectTrendingFeed}
    >
      📈 {t.feed.list.trending}
    </button>

    <!-- Local built-in -->
    <button
      class="feed-entry"
      class:active={selectedFeed === null && !trendingSelected}
      onclick={() => selectFeed(null)}
    >
      🌿 Local
    </button>

    {#each feeds as feed}
      <div data-testid={IDS.FEED_ITEM} class="feed-entry-row" class:active={selectedFeed === feed.feed_id}>
        <button
          class="feed-entry-label"
          onclick={() => selectFeed(feed.feed_id)}
        >
          📡 {feed.name}
        </button>
        <button class="icon-btn muted" data-testid={IDS.FEED_DELETE_BUTTON} title={t.feed.list.delete_feed} onclick={() => handleDeleteFeed(feed.feed_id)}>✕</button>
      </div>
    {/each}

    <!-- Bridge Feeds section. The subscribe affordance appears only when the nest
         supports at least one bridge (bridgeOptions = snapshot.available_bridges,
         build+runtime gated) — Dim 3 capability consumption. -->
    <div class="sidebar-section-header">
      <span class="sidebar-section-title">{t.feed.list.bridge_feeds}</span>
      {#if bridgeOptions.length > 0}
        <button class="icon-btn" data-testid={IDS.BRIDGE_FEED_SUBSCRIBE_TOGGLE} title={t.feed.list.subscribe_bridge} onclick={() => (showBridgeSubscribeForm = !showBridgeSubscribeForm)}>+</button>
      {/if}
    </div>

    {#if showBridgeSubscribeForm && bridgeOptions.length > 0}
      <div class="create-form">
        <select class="select-input" data-testid={IDS.BRIDGE_FORM_BRIDGE_SELECT} bind:value={bridgeFormBridge}>
          {#each bridgeOptions as opt}
            <option value={opt.value}>{opt.label}</option>
          {/each}
        </select>
        <input
          class="text-input"
          data-testid={IDS.BRIDGE_FORM_URI_INPUT}
          type="text"
          placeholder={t.feed.bridge_form.uri}
          bind:value={bridgeFormUri}
        />
        <input
          class="text-input"
          data-testid={IDS.BRIDGE_FORM_NAME_INPUT}
          type="text"
          placeholder={t.feed.bridge_form.name}
          bind:value={bridgeFormName}
        />
        {#if bridgeFormError}
          <p class="error-text">{bridgeFormError}</p>
        {/if}
        <div class="form-actions">
          <button
            class="btn-primary"
            data-testid={IDS.BRIDGE_FORM_SUBSCRIBE_BUTTON}
            disabled={bridgeFormSaving || !bridgeFormUri.trim() || !bridgeFormName.trim()}
            onclick={handleSubscribeBridgeFeed}
          >
            {bridgeFormSaving ? t.common.saving : t.feed.list.subscribe_bridge}
          </button>
          <button class="btn-secondary" data-testid={IDS.BRIDGE_FORM_CANCEL_BUTTON} onclick={() => { showBridgeSubscribeForm = false; }}>
            {t.common.cancel}
          </button>
        </div>
      </div>
    {/if}

    {#each bridgeFeeds as sub}
      <div class="feed-entry-row">
        <span class="feed-entry-label bridge-feed-label">
          {sourceGlyphEmoji(classifySources(sub.bridge)[0]?.glyph ?? 'unknown')} {sub.name}
        </span>
        <button class="icon-btn muted" data-testid={IDS.BRIDGE_FEED_UNSUBSCRIBE_BUTTON} title={t.feed.list.unsubscribe} onclick={() => handleUnsubscribeBridgeFeed(sub.id)}>✕</button>
      </div>
    {/each}

    {#if bridgeFeeds.length === 0 && !showBridgeSubscribeForm}
      <p class="muted bridge-empty">No bridge feeds yet.</p>
    {/if}
  </aside>

  <!-- Right panel: compose + post list -->
  <div class="feed-main" bind:this={scrollContainer} onscroll={onScroll}>
    {#if $identity}
      <FeedComposeBar
        {composeBody} {composeTags} {composing} composeReady={feedReady} composeError={composeError}
        {attachedFile}
        {ownTiers} {gateTier} {gatePreview} {ownRooms} {gateRoom}
        {sellSelected} {sellPrice} {sellAskingPrice} {sellSubscribersFree}
        onsubmit={handleCompose}
        onbodychange={(v) => { composeBody = v; syncCompose(); }}
        ontagschange={(v) => { composeTags = v; syncCompose(); }}
        onfilechange={stageComposeFile}
        onremovefile={removeComposeFile}
        ongatetierchange={(v) => { gateTier = v; syncAudience(); }}
        ongateroomchange={(v) => { gateRoom = v; syncAudience(); }}
        ongatepreviewchange={(v) => { gatePreview = v; syncGatePreview(); }}
        onsellselectedchange={(v) => { sellSelected = v; syncAudience(); }}
        onsellpricechange={(v) => { sellPrice = v; syncAudience(); }}
        onsellaskingpricechange={(v) => { sellAskingPrice = v; syncAudience(); }}
        onsellsubscribersfreechange={(v) => { sellSubscribersFree = v; syncAudience(); }}
        ondialogopen={() => { showComposeDialog = true; }}
      />
    {/if}

    <div class="feed-search-bar">
      <input
        data-testid={IDS.FEED_SEARCH_FIELD}
        type="text"
        class="text-input"
        placeholder={t.common.search}
        value={searchInput}
        oninput={(e) => onSearchInput(e.currentTarget.value)}
      />
      {#if searchInput}
        <button
          data-testid={IDS.FEED_SEARCH_CLEAR}
          class="btn-secondary small"
          onclick={clearSearch}
          title={t.common.cancel}
        >✕</button>
      {/if}
    </div>

    {#if status === 'Loading' && posts.length === 0}
      <p class="muted">{t.common.loading}</p>
    {:else if posts.length === 0}
      <p class="muted">{searchInput ? t.feed.list.no_matching_posts : t.feed.list.no_posts}</p>
    {:else}
      {#each posts as post}
        <PostCard
          {post}
          decoded={decodedPosts[post.post_id]}
          {blobUrl}
          {mediaUrl}
          {proxiedMediaUrl}
          {playbackUrl}
          contentLabel={contentLabelFor(post)}
          oninteract={handleInteract}
          onrevealremote={(id) => { manager?.revealRemoteImages(id); refreshFeed(); }}
          onbuyunlockoffer={buyUnlockOffer}
          onopenpaymentlink={openUnlockPaymentLink}
          muted={isPostMuted(post.post_id)}
          onrevealmuted={revealMuted}
          contentBlocked={isContentBlocked(post)}
          contentCollapsed={isContentCollapsed(post)}
          onrevealcontent={revealContent}
          regionPlaceholder={regionPlaceholderFor(post)}
          trainTarget={trainTarget}
          trainMarker={trainMarkerFor(post.post_id)}
          ontrain={trainPost}
          isOwn={post.author === $identity?.actorId}
          ondelete={handleDeletePost}
          onpublishweb={publishPostToWeb}
          onunpublishweb={unpublishPostFromWeb}
          oncopyweblink={copyPostWebLink}
          oncopypaywalllink={copyPostPaywallLink}
          webLinkOrigin={$siteLink?.origin ?? null}
          webLinkCopied={webLinkCopied?.postId === post.post_id ? webLinkCopied : null}
          onselect={() => {
            // A REPOST ROW's own detail would be blank (the repost post is empty
            // by construction) — activation opens the ORIGINAL's detail instead
            // (feed.md § Interaction bar → Repost). `openPostDetail` resolves it
            // (a no-op round trip for a post already in the loaded window).
            void openPostDetail(post.reposted_post_id ?? post.post_id);
          }}
        />
      {/each}
      {#if loadingMore}
        <p class="muted center">{t.common.loading}</p>
      {:else if !hasMore && posts.length > 0 && !searchInput}
        <p class="muted center">{t.feed.list.end_of_feed}</p>
      {/if}
    {/if}
  </div>
</div>

{#if replyTargetId}
  <!-- Reply compose dialog (feed.md § Implementation status today — reply is
       COMPOSED via FeedManager::reply, never `interact`, whose native arm
       discards `body`). Prior art: linux's build_reply_dialog; no cancel
       button needed beyond click-outside/Escape, matching the train-target
       sheet below. -->
  <div
    class="compose-overlay"
    role="dialog"
    aria-modal="true"
    aria-label={t.common.reply}
    tabindex="-1"
    onclick={(e) => { if (e.target === e.currentTarget) closeReplyDialog(); }}
    onkeydown={(e) => { if (e.key === 'Escape') closeReplyDialog(); }}
  >
    <div class="compose-dialog" data-testid={IDS.FEED_REPLY_DIALOG}>
      <h3>{t.feed.post.replying_to_user({ user: replyTargetAuthor })}</h3>
      <textarea
        class="compose-dialog-textarea"
        data-testid={IDS.FEED_REPLY_TEXT_FIELD}
        placeholder={t.feed.post.write_reply}
        rows="4"
        bind:value={replyText}
      ></textarea>
      <div class="compose-dialog-actions">
        <button class="btn-secondary" onclick={closeReplyDialog}>{t.common.cancel}</button>
        <button
          class="btn-primary"
          data-testid={IDS.FEED_REPLY_SUBMIT_BUTTON}
          disabled={!replyText.trim()}
          onclick={submitReply}
        >{t.common.reply}</button>
      </div>
    </div>
  </div>
{/if}

{#if trainSheetFor}
  <!-- Factor-target sheet (topic-factors.md § Authoring surface): shown ONLY when
       the current feed has no dominant trained factor, so *more/less like this*
       cannot train in context. Picking a topic trains this post into it. -->
  <div
    class="compose-overlay"
    role="dialog"
    aria-modal="true"
    aria-label={t.feed.train_target_title}
    tabindex="-1"
    onclick={(e) => { if (e.target === e.currentTarget) trainSheetFor = null; }}
    onkeydown={(e) => { if (e.key === 'Escape') trainSheetFor = null; }}
  >
    <div class="compose-dialog" data-testid={IDS.FEED_POST_TRAIN_TARGET_SHEET}>
      <h3>{t.feed.train_target_title}</h3>
      {#if trainSheetTopics.length === 0}
        <p class="muted">{t.personalization.trained_topics_empty}</p>
      {:else}
        {#each trainSheetTopics as topic (topic.id)}
          <button
            class="btn-secondary"
            onclick={async () => {
              const target = trainSheetFor;
              trainSheetFor = null;
              if (target && topic.factor_key) {
                await trainInto(target.post_id, target.verb, topic.factor_key);
              }
            }}
          >{topic.name}</button>
        {/each}
      {/if}
      <a href="/app/settings/personalization" class="btn-secondary">{t.personalization.trained_factor_create}</a>
    </div>
  </div>
{/if}

{#if showComposeDialog}
  <div class="compose-overlay" role="dialog" aria-modal="true" aria-label={t.feed.post.compose_post} tabindex="-1" onclick={(e) => { if (e.target === e.currentTarget) showComposeDialog = false; }} onkeydown={(e) => { if (e.key === 'Escape') showComposeDialog = false; }}>
    <div
      class="compose-dialog"
      class:drag-over={dialogDragging}
      data-testid={IDS.FEED_COMPOSE_DIALOG}
      role="region"
      aria-label={t.feed.post.compose_drop_hint}
      ondragover={(e) => { e.preventDefault(); dialogDragging = true; }}
      ondragleave={() => { dialogDragging = false; }}
      ondrop={handleDialogDrop}
    >
      <div class="compose-dialog-header">
        <h3>{t.feed.post.compose_post}</h3>
        <button class="icon-btn" aria-label={t.common.close} onclick={() => { showComposeDialog = false; }}>&times;</button>
      </div>
      <MarkdownToolbar textarea={dialogTextarea} value={composeBody} onchange={(v) => { composeBody = v; syncCompose(); }} />
      <textarea
        class="compose-dialog-textarea"
        data-testid={IDS.COMPOSE_TEXT_FIELD}
        placeholder={t.feed.post.whats_on_your_mind}
        rows="8"
        value={composeBody}
        oninput={(e) => { composeBody = e.currentTarget.value; syncCompose(); }}
        bind:this={dialogTextarea}
      ></textarea>
      <div class="compose-dialog-fields">
        <input
          class="text-input"
          data-testid={IDS.COMPOSE_TAGS_FIELD}
          type="text"
          placeholder={t.feed.post.tags_placeholder}
          value={composeTags}
          oninput={(e) => { composeTags = e.currentTarget.value; syncCompose(); }}
        />
      </div>
      <!-- Gate-to-tier controls live on the inline FeedComposeBar (the primary,
           always-visible composer), not here — the rich dialog gaining its own
           gate select is a follow-on (it would duplicate the compose-gate-tier-select
           id while both are in the DOM). -->
      <div class="compose-dialog-file">
        <input
          data-testid={IDS.COMPOSE_FILE}
          type="file"
          accept="image/*,video/*"
          onchange={async (e) => {
            const file = (e.target as HTMLInputElement).files?.[0] ?? null;
            if (file) {
              const buf = await new Promise<ArrayBuffer>((resolve, reject) => {
                const reader = new FileReader();
                reader.onload = () => resolve(reader.result as ArrayBuffer);
                reader.onerror = () => reject(reader.error);
                reader.readAsArrayBuffer(file);
              });
              stageComposeFile(file, new Uint8Array(buf));
            } else {
              stageComposeFile(null, null);
            }
          }}
        />
        {#if attachedFile}
          <span data-testid={IDS.COMPOSE_FILE_READY} class="file-ready">{attachedFile.name} ({byteSize(attachedFile.size)})</span>
          <button
            type="button"
            class="icon-btn"
            data-testid={IDS.COMPOSE_FILE_REMOVE}
            title={t.common.remove}
            onclick={removeComposeFile}
          >✕</button>
        {/if}
      </div>
      {#if composeError}
        <p class="compose-dialog-error" data-testid={IDS.COMPOSE_ERROR}>{composeError}</p>
      {/if}
      <div class="compose-dialog-actions">
        <button class="btn-secondary" onclick={() => { showComposeDialog = false; }}>{t.common.cancel}</button>
        <button
          class="btn-primary"
          data-testid={IDS.POST_SUBMIT_BUTTON}
          disabled={composing || !composeBody.trim() || !feedReady}
          onclick={async () => { await handleCompose(); showComposeDialog = false; }}
        >
          {composing ? t.feed.post.posting : t.common.post}
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- Post detail (ui.yaml feed transition `click post-card → post_detail`) -->
{#if detailPost}
  <div
    class="compose-overlay"
    role="dialog"
    aria-modal="true"
    aria-label={t.feed.post.post_detail}
    tabindex="-1"
    data-testid={IDS.FEED_POST_DETAIL_DIALOG}
    onclick={(e) => { if (e.target === e.currentTarget) detailPostId = null; }}
    onkeydown={(e) => { if (e.key === 'Escape') detailPostId = null; }}
  >
    <div class="compose-dialog post-detail-dialog">
      <div class="compose-dialog-header">
        {#if !detailPost.legal_takedown_ref}
          <span class="post-detail-author" data-testid={IDS.FEED_POST_DETAIL_AUTHOR}>{shortActor(detailPost.author)}</span>
        {/if}
        <button class="icon-btn" aria-label={t.common.close} onclick={() => { detailPostId = null; }}>&times;</button>
      </div>
      {#if detailPost.legal_takedown_ref}
        <!-- Legal-takedown tombstone (moderation.md § Legal takedown — "the
             post-detail body area when PostSummary.legal_takedown_ref is set";
             `feed.md` § The read model → *Opening a post the timeline never
             loaded*). The nest withholds the body from every viewer, so the
             whole detail collapses to the shared localized tombstone in place
             of body/tags/image/quoted-post/tips — never a blank or
             failed-decrypt-looking surface. Twin of the DM bubble's
             `msg.legal_takedown_ref` branch (`../conversations/+page.svelte`)
             and the quoted-post card's own paint. -->
        <div class="post-detail-body message-legal-takedown" data-testid={IDS.FEED_POST_DETAIL_BODY}>{resolveLocalized(legalTakedownTombstone(detailPost.legal_takedown_ref))}</div>
      {:else if detailRegion}
        <!-- A REGION verdict withholds the detail's body as it does the card's
             (`region-blocking.md` § The blocked render): the placeholder in place
             of body/tags/media, the collapse reveal sharing the card's reveal set
             (tui's post detail, which composes the same call). -->
        <RegionPlaceholder
          class="post-detail-body"
          placeholder={detailRegion}
          onreveal={() => { if (detailPost) revealContent(detailPost.post_id); }}
        />
      {:else}
        <!-- Detail-specific exception (render-model.md § D3): `detailDoc` is a client-built
             document the manager can't project `revealed` onto, so the body uses the per-call
             `documentToHtml` override driven by the local `detailRevealed` toggle. The reveal
             button sets the toggle AND dispatches to the manager so the underlying card stays
             consistent (its snapshot doc gets `revealed: true` projected). -->
        <div class="post-detail-body" data-testid={IDS.FEED_POST_DETAIL_BODY}>{@html documentToHtml(detailDoc, { revealed: detailRevealed })}</div>
        {#if !detailRevealed && documentHasBlockedRemoteImages(detailDoc)}
          <button
            data-testid={IDS.LOAD_REMOTE_CONTENT_BUTTON}
            class="load-remote-btn"
            onclick={() => { detailRevealed = true; if (detailPost) { manager?.revealRemoteImages(detailPost.post_id); refreshFeed(); } }}
          >{t.conversations.detail.load_remote_content}</button>
        {/if}
        {#if detailPost.tags && detailPost.tags.length > 0}
          <div class="post-tags">
            {#each detailPost.tags as tag}
              <span data-testid={IDS.TAG_CHIP} class="tag-chip">#{tag}</span>
            {/each}
          </div>
        {/if}
        {#if detailMediaHash}
          <!-- The detail is where a gated post is unlocked, so it is the first
               place a sealed item's bytes become openable: `mediaUrl` answers
               `null` for the tick between the unlock and the opened object URL,
               and the card paints nothing rather than a broken image. A public
               post's hash resolves synchronously, exactly as before. -->
          {@const detailMediaSrc = mediaUrl(detailMediaHash)}
          {#if detailMediaSrc}
            <div class="post-media">
              <img data-testid={IDS.POST_IMAGE} data-blob-hash={detailMediaHash} src={detailMediaSrc} alt="post media" class="detail-media-img" />
            </div>
          {/if}
        {:else if detailProxiedImage}
          <div class="post-media">
            <ProxiedImage
              path={detailProxiedImage.path}
              alt={detailProxiedImage.alt}
              src={proxiedMediaUrl(detailProxiedImage.path)}
              class="detail-media-img"
            />
          </div>
        {/if}
        {#if detailQuotedBlock}
          <!-- The detail pane reuses the shared `QuotedPost` card (priority #4 — was
               a hand-rolled near-duplicate), so the Slice 2b `unverified-source-badge`
               on the quoted embed paints here too, from `detailQuotedBlock.verification`. -->
          <QuotedPost post_id={detailQuotedBlock.post_id} view={detailQuotedBlock} />
        {/if}
        <!-- The post tip surface (`monetization.md` § Tips) — literally the same
             component the list card paints (it was a near-duplicate copy until
             the web `payments` excision leg), behind the same compile
             condition. -->
        {#if __FAUNA_PAYMENTS__}
          <!-- Keyed on the post: the attribution window's open state now lives
               inside the component, and this pane reuses ONE instance across
               detail navigations — without the key a window opened on one post
               would still be open on the next (the invariant `openPostDetail`'s
               `detailTipListOpen = false` reset used to hold). The list cards
               get this for free from their keyed `{#each}`. -->
          {#key detailPostId}
            <TipSurface tips={detailPost.tips} />
          {/key}
        {/if}
      {/if}
    </div>
  </div>
{/if}

<style>
  .feed-shell {
    display: flex;
    height: 100%;
    gap: 0;
  }

  /* Sidebar */
  .feed-sidebar {
    width: 220px;
    min-width: 180px;
    flex-shrink: 0;
    background: var(--bg-surface);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    padding-bottom: 1rem;
  }

  .sidebar-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.75rem 1rem 0.5rem;
    font-weight: 600;
    font-size: 0.9rem;
  }

  .sidebar-title {
    color: var(--text);
  }

  .create-form {
    padding: 0.5rem 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0.25rem;
  }

  .create-form-title {
    margin: 0;
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--text);
  }

  .field-label {
    font-size: 0.75rem;
    color: var(--text-muted, var(--text));
  }

  .combo-label {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    cursor: pointer;
    color: var(--text);
  }

  .add-rule-row {
    display: flex;
    gap: 0.35rem;
    flex-wrap: wrap;
  }

  .rule-chip {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    background: var(--bg-hover);
    border-radius: 4px;
    padding: 0.15rem 0.4rem;
    font-size: 0.75rem;
    color: var(--text);
  }

  .remove-btn {
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-muted);
    padding: 0;
    font-size: 0.7rem;
    line-height: 1;
  }

  .form-actions {
    display: flex;
    gap: 0.5rem;
  }

  .feed-entry {
    display: block;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    cursor: pointer;
    padding: 0.55rem 1rem;
    font-size: 0.9rem;
    color: var(--text-muted);
    transition: background 0.12s;
  }

  .feed-entry:hover,
  .feed-entry.active {
    background: var(--bg-hover);
    color: var(--accent);
  }

  .feed-entry-row {
    display: flex;
    align-items: center;
    padding: 0 0.25rem 0 0;
    transition: background 0.12s;
  }

  .feed-entry-row:hover,
  .feed-entry-row.active {
    background: var(--bg-hover);
  }

  .feed-entry-row.active .feed-entry-label {
    color: var(--accent);
  }

  .feed-entry-label {
    flex: 1;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    padding: 0.55rem 0.75rem;
    font-size: 0.9rem;
    color: var(--text-muted);
  }

  .bridge-feed-label {
    cursor: default;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.88rem;
    color: var(--text-muted);
  }

  .sidebar-section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.6rem 1rem 0.3rem;
    margin-top: 0.5rem;
    border-top: 1px solid var(--border);
  }

  .sidebar-section-title {
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .bridge-empty {
    padding: 0.3rem 1rem;
    font-size: 0.8rem;
  }

  /* Main feed */
  .feed-main {
    flex: 1;
    overflow-y: auto;
    padding: 1rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .feed-search-bar {
    display: flex;
    gap: 0.375rem;
    align-items: center;
  }
  .feed-search-bar .text-input { flex: 1; }

  /* Shared utilities */
  .text-input {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.3rem 0.5rem;
    font-size: 0.85rem;
    color: var(--text);
    width: 100%;
    box-sizing: border-box;
  }

  .text-input.small {
    width: auto;
    flex: 1;
    min-width: 80px;
  }

  .select-input {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.3rem 0.4rem;
    font-size: 0.85rem;
    color: var(--text);
  }

  .btn-primary {
    background: var(--accent);
    color: #fff;
    border: none;
    border-radius: 4px;
    padding: 0.35rem 0.8rem;
    font-size: 0.85rem;
    cursor: pointer;
    transition: opacity 0.15s;
  }

  .btn-primary:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn-secondary {
    background: none;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.35rem 0.8rem;
    font-size: 0.85rem;
    cursor: pointer;
    color: var(--text);
    transition: background 0.12s;
  }

  .btn-secondary:hover {
    background: var(--bg-hover);
  }

  .btn-secondary.small {
    padding: 0.25rem 0.5rem;
    font-size: 0.8rem;
  }

  .icon-btn {
    background: none;
    border: none;
    cursor: pointer;
    font-size: 1.1rem;
    color: var(--accent);
    padding: 0 0.25rem;
    line-height: 1;
  }

  .icon-btn.muted {
    color: var(--text-muted);
    font-size: 0.85rem;
  }

  .muted {
    color: var(--text-muted);
    font-size: 0.85rem;
  }

  .center {
    text-align: center;
  }

  .error-text {
    color: #e55;
    font-size: 0.85rem;
    margin: 0;
  }
  .page-error {
    padding: 0.5rem 1.25rem;
  }

  @media (max-width: 768px) {
    .feed-shell {
      flex-direction: column;
    }

    .feed-sidebar {
      width: 100%;
      flex-direction: row;
      overflow-x: auto;
      overflow-y: hidden;
      border-right: none;
      border-bottom: 1px solid var(--border);
      padding-bottom: 0;
      height: auto;
      min-width: unset;
    }

    .create-form {
      display: none;
    }

    .sidebar-header {
      display: none;
    }

    .feed-entry,
    .feed-entry-label {
      white-space: nowrap;
    }
  }

  /* Compose dialog overlay */
  .post-detail-body {
    white-space: pre-wrap;
    word-break: break-word;
    padding: 0.5rem 0;
    font-size: 0.9rem;
    line-height: 1.5;
  }
  .post-detail-author { font-weight: 600; }
  /* Legal-takedown tombstone (mirrors conversations' `.message-deleted`). */
  .message-legal-takedown { font-style: italic; color: var(--text-muted); }
  .post-tags { display: flex; flex-wrap: wrap; gap: 0.25rem; margin-top: 0.5rem; }
  .tag-chip {
    font-size: 0.7rem;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--accent);
  }
  .post-media { margin-top: 0.5rem; }
  .detail-media-img { max-width: 100%; border-radius: 4px; }
  /* The tip surface's styles moved with its markup into
     `payments/TipSurface.svelte` (see PostCard's matching note). */
  /* The detail-pane quoted-post card now reuses the shared `QuotedPost` component
     (Slice 2b consolidation), which carries its own scoped styles, so the former
     `.quoted-post` / `.quoted-author` / `.quoted-body` rules here are removed. */
  .compose-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .compose-dialog {
    background: var(--bg-surface);
    border: 2px solid var(--border);
    border-radius: 8px;
    padding: 1.25rem;
    max-width: 600px;
    width: 90%;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    transition: border-color 0.15s, background 0.15s;
  }

  .compose-dialog.drag-over {
    border-style: dashed;
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, var(--bg-surface));
  }

  .compose-dialog-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .compose-dialog-header h3 {
    margin: 0;
    font-size: 1rem;
  }

  .compose-dialog-textarea {
    width: 100%;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.5rem;
    font-size: 0.9rem;
    resize: vertical;
    color: var(--text);
    font-family: inherit;
    box-sizing: border-box;
  }

  .compose-dialog-fields {
    display: flex;
    gap: 0.5rem;
  }

  .compose-dialog-fields .text-input {
    flex: 1;
  }

  .compose-dialog-file {
    font-size: 0.8rem;
  }

  .compose-dialog-file .file-ready {
    color: var(--accent);
    font-weight: 600;
    margin-left: 0.25rem;
  }

  .compose-dialog-error {
    color: var(--error, #e74c3c);
    font-size: 0.8rem;
    margin: 0;
  }

  .compose-dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }
</style>
