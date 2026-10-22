<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { onStoreChange } from '#lib/store-change';
  import { identity, reconnectTick, onActorChange } from '#lib/store';
  import { registerActorScopedReset } from '#lib/actorScope';
  import { get } from 'svelte/store';
  import { getFeedManager, feedSnapshot, refreshFeed, scheduleDraftSave } from '#lib/feed';
  import { CueCapture, hydrateCuesOnce } from '#lib/feed-cues';
  import { clearSent } from '#lib/compose-sent';
  import { hydrateContentPolicy } from '#lib/contentPolicy.svelte';
  import { registerRegionBlockCounter } from '#lib/region.svelte';
  import { registerNestBlockCounter } from '#lib/nestRules.svelte';
  import RegionPlaceholder from '#lib/components/RegionPlaceholder.svelte';
  import { notifyBuffer } from '#lib/familyNotify';
  import { syncFeedRoomPosts } from '#lib/conversations';
  import { ensureWasm, processAndSealPublicPost, sealComposeAttachment, logMessage, markdownToDocument, classifySources, type BridgeIdentity, parseWeightPermille, formatWeightPermille, ruleTypeOptions, builtinFactorOptions, ruleSummaryLabel, ruleRequiredLabel, canAddRule, legalTakedownTombstone, adminBlockNotice, type RuleTypeOption } from '#lib/wasm';
  import { consumePendingSearchNav } from '#lib/search';
  import { documentToHtml, documentHasBlockedRemoteImages, quotedPostBlock, mediaImageHash, proxiedPostImage } from '#lib/document';
  import { readProvenanceFromBytes } from '#lib/c2pa';
  import { uploadBlobMultipart } from '#lib/api';
  import { trainedTopicsList, sharedRpcPort } from '#lib/rpc';
  import { ensureWebPageHydrated } from '#lib/web-publish';
  import { resolveLocalized, resolveLocalizedNested } from '#lib/i18n/localized';
  import { postOffersThreadView, threadPostAuthorLabel } from '#lib/wasm';
  import { byteSize } from '#lib/value-format';
  import { createLabelerCatalogMachine } from '#lib/wasm-labeler-catalog';
  import FeedComposeBar from '#lib/components/FeedComposeBar.svelte';
  import MarkdownToolbar from '#lib/components/MarkdownToolbar.svelte';
  import FeedPostCard from '#lib/components/FeedPostCard.svelte';
  import FeedReplyDialog from '#lib/components/FeedReplyDialog.svelte';
  import FeedTrainSheet from '#lib/components/FeedTrainSheet.svelte';
  import { createFeedPostHost, authorName, contentLabelFor } from '#lib/feed-post-host.svelte';
  import ContentLabelBadge from '#lib/components/ContentLabelBadge.svelte';
  import ReportHost from '#lib/components/ReportHost.svelte';
  import { type ReportTarget } from '#lib/wasm';
  import ProxiedImage from '#lib/components/ProxiedImage.svelte';
  import QuotedPost from '#lib/components/QuotedPost.svelte';
  import TipSurface from '#lib/components/payments/TipSurface.svelte';
  import { contactsRevision } from '#lib/conversations';
  import { sourceGlyphEmoji } from '#lib/source-glyph';
  import { t } from '#lib/i18n/strings';
  import type { WasmFeedManager } from '../../../static/fauna_wasm.js';
  import type { LabelerCatalogMachine } from '../../../static/fauna_wasm_labeler_catalog.js';
  import type { LabelerCatalogSnapshot } from '#lib/labeler-catalog-machine';
  import { IDS } from '#lib/generated/uiIds';

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

  let manager: WasmFeedManager | null = null;

  // The feed card's host over the timeline manager (`#lib/feed-post-host`):
  // post media, the reveal sets, the content-policy render, the card's verbs,
  // the reply dialog and the training sheet — the one wiring the profile's
  // Posts tab shares over its author window (`profile.md` § The Posts tab).
  // The closures read this page's state late, so their declaration order
  // below is irrelevant.
  const host = createFeedPostHost({
    manager: () => manager,
    posts: () => posts,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    latestPosts: () => (get(feedSnapshot) as any)?.posts ?? [],
    refreshFeed,
    setError: (message) => { loadError = message; },
    destroyed: () => destroyed,
  });
  const { mediaUrl, proxiedMediaUrl, revealContent, regionPlaceholderFor, postRender, unlockGated } = host;
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
  // The bridges roster a post's source token resolves against (`ui/feed.md`
  // § Implementation status today, `SourceKind::Bridged`).
  let bridgeRoster = $derived<BridgeIdentity[]>(snap?.bridge_roster ?? []);
  let selectedFeed = $derived<string | null>(snap?.selected_feed ?? null);
  let trendingSelected = $derived<boolean>(snap?.trending_selected ?? false);
  let posts = $derived(snap?.posts ?? []);
  // The shared `FeedSnapshot::empty_state` answer the wasm snapshot carries —
  // 'NoPosts' / 'NoMatches' / null (feed.md § Errors & edge cases): the page
  // never re-derives it from `status`, `posts` or its own search field.
  let emptyState = $derived<'NoPosts' | 'NoMatches' | null>(snap?.empty_state ?? null);
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
  // The report sheet's own failure line (a failed send, or a block/hide that
  // did not land beside a landed report) paints on the same `error-message`.
  let reportError = $state('');
  // The Bluesky thread page (ui.yaml `feed.sub_pages.thread`, `bridges.md`
  // § Bluesky-native thread view) — the shared `FeedSnapshot::thread`, open
  // over the post detail it was opened from; `null` when no page is open.
  let threadPage = $derived<any | null>(snap?.thread ?? null);
  // While the thread page is open its own fetch error leads on `error-message`
  // (tui's `page_error`), ahead of the feed's.
  let pageError = $derived(
    resolveLocalized(threadPage?.error) || resolveLocalized(snap?.error) || loadError || reportError,
  );

  // The shared report sheet (`report-sheet`), opened by `feed-post-report-button`.
  let reportTarget = $state<ReportTarget | null>(null);
  function openReport(postId: string): void {
    const target = host.reportTargetFor(postId);
    if (!target) return;
    reportError = '';
    reportTarget = target;
  }
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
  // (`#lib/conversations`' `syncFeedRoomPosts`). Exclusive with a tier and a
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
  // fields that still hold what was sent (`#lib/compose-sent`). Both halves exist
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

  // ── Muted-keyword collapse, the content-policy render and the trained-topic
  //    verbs live on the card host (`#lib/feed-post-host`); the page keeps the
  //    region-block walk and the in-context trained topic, both read off its
  //    own snapshot. ──
  // Convention 17's verdict-side walk (`region-block-never-silent`): the loaded
  // cards the region blocks, plus an open detail it blocks.
  const regionBlocks = (r: ReturnType<typeof postRender>) =>
    !!r.placeholder && 'region' in r.placeholder && r.placeholder.verb === 'block';
  const nestBlocks = (r: ReturnType<typeof postRender>) =>
    !!r.placeholder && 'domain' in r.placeholder && r.placeholder.verb === 'block';
  onMount(() =>
    registerRegionBlockCounter('feed', () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      let n = posts.filter((p: any) => regionBlocks(postRender(p))).length;
      if (detailPost && regionBlocks(postRender(detailPost))) n += 1;
      return n;
    }),
  );
  // The nest's twin (`nest-block-never-silent`): the loaded cards a nest rule
  // blocks, plus an open detail it blocks.
  onMount(() =>
    registerNestBlockCounter('feed', () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      let n = posts.filter((p: any) => nestBlocks(postRender(p))).length;
      if (detailPost && nestBlocks(postRender(detailPost))) n += 1;
      return n;
    }),
  );

  /** The composed feed's single trained topic, or null (then the verbs open the
   *  target sheet). Re-derived from `snap` so switching feeds re-reads it. */
  let trainTarget = $derived.by<string | null>(() => {
    void snap; // re-run whenever the manager pushes a new snapshot
    return host.trainTargetFactor();
  });

  let detailAuthor = $derived.by<string>(() => {
    void $contactsRevision;
    return detailPost ? authorName(detailPost) : '';
  });

  /// The card host's augmentation caches are ACTOR-SCOPED and must die with the actor.
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
    host.resetAugmentation();
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

  // The thread page shows over the post detail it was opened from — only while
  // the snapshot's page names that post.
  let threadShown = $derived(!!detailPost && threadPage?.post_id === detailPost.post_id);
  // `feed-post-view-thread-button` — the shared predicate decides, never a
  // page-side source check.
  let detailOffersThread = $derived(!!detailPost && postOffersThreadView(detailPost.source ?? ''));

  /** Open the thread page on the open post detail's post. The page state —
   *  loading, then loaded or failed — rides the snapshot's `thread`, so the
   *  manager's notifications repaint it; nothing here awaits the fetch. */
  function openThread(): void {
    if (!detailPost) return;
    if (!manager) { loadError = t.common.still_loading; return; }
    void manager.openThread(detailPost.post_id);
    refreshFeed();
  }

  /** Back from the thread page to the post detail it was opened from. */
  function closeThread(): void {
    manager?.closeThread();
    refreshFeed();
  }

  /** Close the post detail — and a thread page open over it. */
  function closeDetail(): void {
    if (threadPage) closeThread();
    detailPostId = null;
  }

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
  async function openPostDetail(postId: string, home?: string | null): Promise<void> {
    detailPostId = postId;
    if (!manager) { loadError = t.common.still_loading; return; }
    try {
      // A post homed on another nest (a room-search hit on a foreign-homed
      // room) opens through the relay door with the home handed back
      // unchanged (`feed.md` § The cross-nest deep-link relay).
      if (home) await manager.resolvePostFrom(postId, home);
      else await manager.resolvePost(postId);
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
  let detailDecoded = $derived(detailPost ? host.decodedPosts[detailPost.post_id] : null);
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
  //    derivation): the shell is `#lib/feed-cues`; this page only builds it over
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
    for (const p of list) host.augmentPost(p);
  });

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
    //
    // Re-pull the manager's CURRENT source, never `selectFeed(undefined)`. The
    // manager is module-level and outlives this route, which remounts on every
    // nav back to the Feed page: re-selecting Local here dropped the reader out
    // of the feed they had open (a composed feed, Trending) every time they
    // visited another page — so a return from Personalization showed the local
    // feed, unranked by the topic they had just renamed. A fresh manager (first
    // mount, an actor change) starts on Local, so this is still the nest's local
    // feed there (`feed.md` § The user's selected feed; the re-pull seam is
    // `trending.md`'s `refresh_current_feed` row).
    await loadStep(() => m.refreshCurrentFeed());
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
    // `#lib/actor-scope-registration-contract.test.ts`. Nothing below needs
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
    // one shape rather than this page's private one (`#lib/actorScope` +
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
      // (`#lib/search`'s `consumePendingSearchNav`) — after `loadAll()` so
      // `openPostDetail`'s own `resolvePost` call (which it runs regardless)
      // sees the freshest loaded window before deciding whether a fetch is
      // needed.
      const pendingNav = consumePendingSearchNav();
      if (pendingNav?.kind === 'post') await openPostDetail(pendingNav.postId, pendingNav.home);
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

  // The store-change notice (`#lib/store-change`): the sealed scorers — muted
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
    host.destroy();
    notifyBuffer.destroy();
  });
</script>

{#if pageError}
  <p class="error-text page-error" data-testid={IDS.ERROR_MESSAGE}>{pageError}</p>
{/if}

<ReportHost bind:target={reportTarget} onerror={(m) => (reportError = m)} />

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
          {sourceGlyphEmoji(classifySources(sub.bridge, bridgeRoster)[0]?.glyph ?? 'unknown')} {sub.name}
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
    {:else if emptyState === 'NoMatches'}
      <p class="muted" data-testid={IDS.FEED_NO_RESULTS}>{t.feed.list.no_matching_posts}</p>
    {:else if emptyState === 'NoPosts'}
      <p class="muted" data-testid={IDS.FEED_EMPTY_STATE}>{t.feed.list.no_posts}</p>
    {:else if posts.length === 0}
      <!-- Not yet authoritative (a read in flight or an error): neither empty state. -->
    {:else}
      {#each posts as post (post.post_id)}
        <FeedPostCard
          {post}
          {host}
          {bridgeRoster}
          {trainTarget}
          onreport={openReport}
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

<!-- The card's reply compose dialog and factor-target sheet, over the
     timeline's card host (`#lib/components/FeedReplyDialog.svelte`,
     `FeedTrainSheet.svelte` — the profile's Posts tab paints the same two). -->
<FeedReplyDialog {host} />
<FeedTrainSheet {host} />

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
    onclick={(e) => { if (e.target === e.currentTarget) closeDetail(); }}
    onkeydown={(e) => {
      // Escape on the thread page steps back to the post detail (tui's posture —
      // ui.yaml gives the page no dismiss element); on the detail it closes it.
      if (e.key === 'Escape') { if (threadShown) closeThread(); else closeDetail(); }
    }}
  >
    <div class="compose-dialog post-detail-dialog">
      {#if threadShown}
        <!-- The Bluesky thread page (ui.yaml `feed.sub_pages.thread`): one
             `feed-thread-post` per post in thread order — ancestors, the opened
             post, its replies — the opened post `selected` as the nest named it
             (`is_focal`). A failed fetch paints no posts; its reason leads on
             the page's `error-message` (`pageError`). -->
        <div class="compose-dialog-header">
          <button class="icon-btn" aria-label={t.common.back} onclick={closeThread}>&larr;</button>
          <span class="post-detail-author">{t.feed.post.thread}</span>
          <button class="icon-btn" aria-label={t.common.close} onclick={closeDetail}>&times;</button>
        </div>
        <div class="thread-page" data-testid={IDS.FEED_THREAD_PAGE}>
          {#if threadPage.status === 'Loading'}
            <p class="thread-status">{t.common.loading}</p>
          {/if}
          {#each threadPage.posts ?? [] as p, i (i)}
            <div
              class="thread-post"
              class:thread-post-focal={p.is_focal}
              data-testid={IDS.FEED_THREAD_POST}
              data-selected={p.is_focal ? 'true' : 'false'}
            ><span class="thread-post-author">{threadPostAuthorLabel(p)}</span> {p.text}</div>
          {/each}
        </div>
      {:else}
      <div class="compose-dialog-header">
        {#if !detailPost.legal_takedown_ref}
          <span class="post-detail-author" data-testid={IDS.FEED_POST_DETAIL_AUTHOR}>{detailAuthor}</span>
        {/if}
        <button class="icon-btn" aria-label={t.common.close} onclick={closeDetail}>&times;</button>
      </div>
      <!-- `content-label-badge`: the shared highest-confidence pick over the opened
           post's labels. A room's relayed verdicts fold onto a deep-linked post at
           unlock, so this is where a member sees them with no timeline card
           (feed-read-model.md § the cross-nest deep-link relay). -->
      {#if contentLabelFor(detailPost)}
        <div class="post-detail-label"><ContentLabelBadge label={contentLabelFor(detailPost) ?? ''} /></div>
      {/if}
      {#if detailPost.admin_block_reason && detailPost.body}
        <!-- The author's own read keeps the body; the shared admin-block notice
             (moderation.md § Admin block of a reported item) paints beside it.
             No dedicated id is approved, so it is chrome. -->
        <p class="muted">{resolveLocalizedNested(adminBlockNotice(detailPost.admin_block_reason))}</p>
      {/if}
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
      {:else if detailPost.admin_block_reason && !detailPost.body}
        <!-- Admin-blocked post (moderation.md § Admin block of a reported item): the
             nest withheld the body from every viewer but its author, so the shared
             notice stands in the body area exactly as the tombstone above does. -->
        <div class="post-detail-body message-legal-takedown" data-testid={IDS.FEED_POST_DETAIL_BODY}>{resolveLocalizedNested(adminBlockNotice(detailPost.admin_block_reason))}</div>
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
      {:else if host.isContentBlocked(detailPost)}
        <!-- The family arm withholds the detail's body as it does the card's
             (`family-safety.md` § Content policy): the fixed frame in place of
             body/tags/media, NO reveal, the guardian rule's reason beside it
             (`nest-rules.md` § The shared rule editor → *The reason is the
             member's to read*). Without this arm a guardian-blocked post opened
             from its placeholder card painted its body. -->
        <div class="post-detail-body content-policy-collapse">
          <span
            class="muted"
            data-testid={IDS.CONTENT_POLICY_BLOCKED_NOTICE}
            data-state="blocked"
            data-source={host.isContentReported(detailPost) ? 'reported' : undefined}
          >{host.isContentReported(detailPost) ? t.moderation.report.hidden_placeholder : t.family.content_blocked_notice}</span>
          {#if host.guardianReasonFor(detailPost)}
            <span class="muted" data-testid={IDS.CONTENT_POLICY_BLOCKED_REASON}>{host.guardianReasonFor(detailPost)}</span>
          {/if}
        </div>
      {:else if host.isContentCollapsed(detailPost)}
        <div class="post-detail-body content-policy-collapse">
          <span
            class="muted"
            data-testid={IDS.CONTENT_POLICY_BLOCKED_NOTICE}
            data-state="collapsed"
          >{t.family.content_collapsed_notice}</span>
          {#if host.guardianReasonFor(detailPost)}
            <span class="muted" data-testid={IDS.CONTENT_POLICY_BLOCKED_REASON}>{host.guardianReasonFor(detailPost)}</span>
          {/if}
          <button
            class="btn-secondary small"
            data-testid={IDS.CONTENT_POLICY_COLLAPSED_REVEAL_BUTTON}
            onclick={() => { if (detailPost) revealContent(detailPost.post_id); }}
          >{t.family.content_reveal_button}</button>
        </div>
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
          {@const detailMediaSrc = mediaUrl(detailMediaHash, detailPost)}
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
      {#if detailOffersThread && !detailPost.legal_takedown_ref}
        <!-- *View Thread* — a Bluesky post has a conversation to open
             (`bridges.md` § Bluesky-native thread view). -->
        <div class="post-detail-actions">
          <button class="btn-secondary" data-testid={IDS.FEED_POST_VIEW_THREAD_BUTTON} onclick={openThread}>{t.feed.post.view_thread}</button>
        </div>
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
  .post-detail-actions { display: flex; gap: 0.5rem; margin-top: 0.75rem; }
  /* The Bluesky thread page — the opened post marked. */
  .thread-page { display: flex; flex-direction: column; gap: 0.5rem; padding: 0.5rem 0; }
  .thread-status { color: var(--text-muted); font-size: 0.85rem; }
  .thread-post {
    white-space: pre-wrap;
    word-break: break-word;
    font-size: 0.9rem;
    line-height: 1.5;
    padding: 0.4rem 0.6rem;
    border-left: 3px solid transparent;
  }
  .thread-post-focal { border-left-color: var(--accent); background: var(--bg-surface); }
  .thread-post-author { font-weight: 600; }
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
