package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContentPolicyInputs
import com.fauna.app.core.ContentPolicyStore
import com.fauna.app.core.FamilyNotifyStore
import com.fauna.app.core.HexUtil
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.WebPublishStore
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.app.core.feed.PostMediaOpen
import com.fauna.app.payments.resolvePostTipsIfBuilt
import com.fauna.ffi.FfiFeedManager
import com.fauna.ffi.actorIdFromSecret
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_core.ContentLabelEntry
import uniffi.fauna_feed.AttachedFile
import uniffi.fauna_feed.FactorWeightInput
import uniffi.fauna_feed.FeedSnapshot
import uniffi.fauna_feed.FilterRuleInput
import uniffi.fauna_feed.QuotedPostView
import uniffi.fauna_feed.SellComposeState
import uniffi.fauna_feed.TrainVerb
import javax.inject.Inject

/**
 * Thin observer over the shared `FeedManager` (UniFFI [com.fauna.ffi.FfiFeedManager],
 * via [FeedManagerHost]) — the single observable surface the Feed list / compose /
 * post-detail screens render off, per docs/goal/ui/feed.md § State & data shape /
 * § Architectural rules. All post-list / search / feed-rule / compose / bridge
 * state lives in shared Rust; this VM exposes the shared [snapshot] and routes
 * gestures to manager mutators.
 *
 * The manager is built lazily over the post-auth WS-RPC connection and shared
 * across the three Feed routes via [FeedManagerHost] (so post-detail renders the
 * `PostSummary` the list loaded). A null manager (nest not yet connected) leaves
 * an empty page; the next gesture retries.
 *
 * Post interactions (like / repost / reply / quote) are NOT part of the
 * `FeedManager` snapshot surface — they mutate no local post-list state, the same
 * decision the Rust-native Linux lead made — so they stay thin client glue over
 * [ApiClient.interactWithPost]. Blob upload is likewise client glue (the picker +
 * upload are platform-native); the staged-file metadata + validation are shared.
 */
@HiltViewModel
class FeedVM @Inject constructor(
    private val host: FeedManagerHost,
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    private val contentPolicyStore: ContentPolicyStore,
    private val familyNotifyStore: FamilyNotifyStore,
    private val webPublishStore: WebPublishStore,
) : ViewModel() {

    /** The whole renderable Feed page (shared across the list/compose/detail routes). */
    val snapshot: StateFlow<FeedSnapshot?> get() = host.snapshot

    /**
     * The shared content-policy render inputs (guardian floor + the viewer's own
     * spam/phishing thresholds) — the feed card resolves each post's block/collapse
     * verdict off this via [ContentPolicyInputs.verdictFor] (family-safety.md
     * § Content policy). One shared [ContentPolicyStore] backs both the feed and
     * conversations so the two social surfaces can never drift on enforcement.
     */
    val contentPolicyInputs: StateFlow<ContentPolicyInputs> get() = contentPolicyStore.inputs

    /** Count a rendered post's guardian-floor enforcement for **Guardian Notify**
     *  (family-safety.md § Guardian Notify) — a no-op unless the ward's
     *  `content_notify` knob is on and the guardian floor bites. */
    fun noteContentEnforcement(postId: String, labels: List<ContentLabelEntry>) =
        familyNotifyStore.record(postId, labels)

    /**
     * This device's own actor id as hex, or null when not yet authenticated —
     * the `is_own` gate for own-post delete (`feed-post-delete-button`;
     * feed.md § State & data shape → *Post deletion*). Derived off the stored
     * secret exactly as the other VMs do (ContactsVM/ProfileVM/PrivacySettingsVM):
     * `actorIdFromSecret` is the same identity the shared manager signs the
     * `Tombstone` with, so the client-side gate and the nest's three author
     * checks agree by construction.
     */
    fun ownActorIdHex(): String? =
        secureStorage.secretHex?.let {
            try { HexUtil.bytesToHex(actorIdFromSecret(HexUtil.hexToBytes(it))) }
            catch (_: Exception) { null }
        }

    /**
     * Engagement-cue failures (hydrate / observe / flush).
     *
     * A separate channel from `snapshot.error` because the cue calls are
     * fire-and-forget side-effects of scrolling — they mutate no page state, so
     * the shared manager doesn't route their failures through the snapshot. They
     * still MUST reach the user's `error-message`: a cue call that fails
     * silently yields no error and no effect, which is indistinguishable from a
     * real product bug and sends the next session hunting one that doesn't
     * exist.
     */
    private val _cueError = MutableStateFlow<String?>(null)
    val cueError: StateFlow<String?> = _cueError.asStateFlow()

    /**
     * Own-post delete failures (`feed-post-delete-confirm-button`).
     *
     * A separate channel — like [cueError] — because a successful delete drops
     * the post from the loaded window (the snapshot re-emits), but a *failure*
     * mutates no page state, so the shared manager doesn't route it through
     * `snapshot.error`. It still MUST reach the user's `error-message`: a
     * silently-dropped delete yields no error and no effect, indistinguishable
     * from a real product bug (testing.md § Cross-app e2e conventions,
     * point 11). Carries the raw manager message; the screen wraps it in the
     * localized `feed_error_delete` template.
     */
    private val _deleteError = MutableStateFlow<String?>(null)
    val deleteError: StateFlow<String?> = _deleteError.asStateFlow()

    /**
     * Training-gesture failures (`feed-post-more-like-this` / `-less-like-this`).
     *
     * A separate channel — like [deleteError] — because a successful train/untrain
     * mutates no page state directly (the manager re-seals + re-ranks and the
     * snapshot re-emits), so the shared manager doesn't route a failure through
     * `snapshot.error`. It still MUST reach the user's `error-message`: a
     * silently-dropped train is indistinguishable from a real product bug
     * (testing.md § Cross-app e2e conventions, point 11).
     */
    private val _trainError = MutableStateFlow<String?>(null)
    val trainError: StateFlow<String?> = _trainError.asStateFlow()

    /**
     * Self-serve teaser-buy failures (`gated-post-buy-button`, `monetization.md`
     * § Per-post pay-to-unlock, gap (2c)) — a separate channel, like
     * [deleteError]/[trainError]: a successful buy clears
     * `PostSummary.unlock_offer` back to `null` as its own completion signal
     * (the teaser un-renders), so the shared manager never routes a *failure*
     * through `snapshot.error` either. Never swallowed, same reasoning as
     * [deleteError] (testing.md § Cross-app e2e conventions, point 11).
     */
    private val _buyUnlockError = MutableStateFlow<String?>(null)
    val buyUnlockError: StateFlow<String?> = _buyUnlockError.asStateFlow()

    /**
     * Composed-verb failures (reply / quote / repost) — a separate channel like
     * [deleteError]: a refused reference mutates no page state, so it never
     * rides `snapshot.error`, yet it MUST reach the page's `error-message`
     * (convention 11). Carries the raw manager message; the screen maps it
     * through the shared `feed_refusal_i18n_key` (a restricted post's
     * `feed.reference_restricted` refusal, `feed.md` § Encryption at rest →
     * *A reply, quote or repost of a restricted post*) — web's `verbErrorCopy`.
     */
    private val _verbError = MutableStateFlow<String?>(null)
    val verbError: StateFlow<String?> = _verbError.asStateFlow()

    /** Drop a [verbError] the page has shown, so the same refusal again repaints. */
    fun clearVerbError() {
        _verbError.value = null
    }

    /**
     * The own-post web-publishing verbs' shared state (origin resolution,
     * copy confirmation, mutation failures) — [WebPublishStore], the SAME
     * singleton `WebSettingsVM` reads/writes, so the ⋯-menu and the
     * `web-settings` Published-posts section can never disagree about a
     * creator's address (`web-content-hosting.md` § Published-post
     * management). A publish/unpublish mutation re-reads the feed
     * ([reload]) so the card's `webSlug` repaints from the nest's own answer.
     */
    val webPublishError: StateFlow<Pair<String, String>?> get() = webPublishStore.error
    val webLinkOrigin: StateFlow<String?> get() = webPublishStore.origin
    val webLinkCopied: StateFlow<Triple<String, String, String>?> get() = webPublishStore.copied

    fun ensureWebOrigin() = webPublishStore.ensureOrigin()

    fun publishPostToWeb(postId: String) {
        viewModelScope.launch {
            webPublishStore.publish(postId)
            reload()
        }
    }

    fun unpublishPostFromWeb(postId: String) {
        viewModelScope.launch {
            webPublishStore.unpublish(postId)
            reload()
        }
    }

    fun copyPostWebLink(postId: String, slug: String) = webPublishStore.copyWebLink(postId, slug)

    fun copyPostPaywallLink(postId: String, slug: String) = webPublishStore.copyPaywallLink(postId, slug)

    init {
        // Re-hydrate on each WS reconnect (transport.md § Push events) — the feed
        // has no poll backstop, so a post that arrived while disconnected would
        // stay invisible until a manual refresh. Mirrors linux WsEvent::Reconnected.
        viewModelScope.launch { api.reconnectTick.collect { reload() } }
        // The store-change notice: the sealed scorers (muted keywords, trained
        // factors) load only inside the manager's reload, so a word muted on
        // another device re-scores the open feed ([ApiClient.storeChangedTick]).
        // The narrow entry — the current source only, Trending preserved.
        viewModelScope.launch { api.storeChangedTick.collect { refreshCurrent() } }
    }

    /** Enter the Feed page: build the manager if needed, then load the selector,
     *  the bridge feeds, and the current feed's first page. */
    fun start() = reload()

    private fun reload() {
        // Re-pull the ward's guardian floor + own spam thresholds on entry and on
        // reconnect, so a guardian's policy change (or a just-completed login)
        // reaches the feed's render enforcement (family-safety.md § Content policy).
        contentPolicyStore.refresh()
        val m = host.manager() ?: return
        viewModelScope.launch {
            m.refreshFeeds()
            m.refreshBridgeFeeds()
            m.refreshAvailableBridges()
            reselectCurrent(m)
        }
        // Fetch-on-session-start for the sealed cue rollup — once per feed entry
        // AND per reconnect: re-auth rebuilds the manager, so its cue engine is
        // fresh and must re-hydrate before any put (an un-hydrated put would
        // overwrite this user's other devices' cues with this device's partial
        // state). The shared manager suppresses puts until hydration, so the
        // ordering is enforced there, not here.
        hydrateCues()
    }

    /**
     * Re-pull whatever source is current, without changing it — the refresh
     * path (pull-to-refresh, reconnect, page enter). Rides the shared
     * `refresh_current_feed` seam, which re-runs the current query (Local /
     * Trending / a custom feed, same search term) and reloads the sealed
     * scorers — never `select_feed(selected_feed)`, which resolves to
     * `select_feed(null)` = the LOCAL feed while Trending is selected and
     * would silently drop the user's Trending selection on every refresh
     * (trending.md § The Trending feed).
     */
    private suspend fun reselectCurrent(m: FfiFeedManager) {
        m.refreshCurrentFeed()
    }

    /** Re-select the current source (Trending or a feed) — the pull-to-refresh
     *  entry point; see [reselectCurrent] for why Trending needs its own branch. */
    fun refreshCurrent() {
        val m = host.manager() ?: return
        viewModelScope.launch { reselectCurrent(m) }
    }

    // ── Feed selector / search / pagination (shared Rust) ─────────────────────

    /** Select a feed (`feed-item`); `null` ⇒ the nest's local feed. */
    fun selectFeed(feedId: String?) {
        val m = host.manager() ?: return
        viewModelScope.launch { m.selectFeed(feedId) }
    }

    /** Select the built-in Trending virtual feed (`feed-trending-item`,
     *  trending.md § The Trending feed) — the scored sibling of the local feed
     *  over `fauna.feed.trending.posts`, mirroring [selectFeed] `null` = local.
     *  The manager owns the mutual exclusion: this sets `trending_selected` and
     *  clears `selected_feed`, so the two are never both set. */
    fun selectTrendingFeed() {
        val m = host.manager() ?: return
        viewModelScope.launch { m.selectTrendingFeed() }
    }

    fun setSearchQuery(term: String?) {
        val m = host.manager() ?: return
        viewModelScope.launch { m.setSearchQuery(term) }
    }

    fun clearSearch() {
        val m = host.manager() ?: return
        viewModelScope.launch { m.clearSearch() }
    }

    fun loadMore() {
        val m = host.manager() ?: return
        viewModelScope.launch { m.loadMore() }
    }

    /** Resolve the first media blob hash for a `has_media` post (lazy; the
     *  feed-index projection never reads the body, so the hash is per-post). */
    fun resolveMedia(postId: String) {
        val m = host.manager() ?: return
        viewModelScope.launch { m.resolveMedia(postId) }
    }

    /** Opt [postId]'s remote images into loading (`load-remote-content-button`,
     *  render-model.md § D3). Routes to the shared manager, which owns the reveal set
     *  in one audited place: it flips the flag and re-emits, so the next [snapshot]
     *  carries `RemoteImage.revealed = true` on this post's `document` (covering both
     *  its list card and the detail) and the feed screens recompose to paint the
     *  images. No client-side reveal state. */
    fun revealRemoteImages(postId: String) {
        host.manager()?.revealRemoteImages(postId)
    }

    /** Project the embedded quoted-post card for `quotedPostId` (from the loaded
     *  set with no fetch when possible, else one `fauna.posts.get`). Also folds a
     *  `RenderBlock.QuotedPost` into the post `document` (idempotently re-emitting),
     *  which the feed screens paint. */
    suspend fun resolveQuotedPost(quotedPostId: String): QuotedPostView? =
        host.manager()?.resolveQuotedPost(quotedPostId)

    /**
     * Make [postId] renderable through `find_post`'s union even when the
     * timeline never loaded it — the deep-link door `SearchNav.Post` needs
     * (`ui/search.md` § Where logic lives → *Result navigation (deep link)*).
     * Cheap and idempotent (`FfiFeedManager.resolvePost`'s own doc): a post
     * already in [snapshot]'s `posts`, or already parked in `deepLinkedPost`,
     * costs no round trip. Unlike [resolvePostTips]/[resolvePostUnlockOffer]
     * (fire-and-forget enrichment of an already-visible post), this is
     * **awaited** by [com.fauna.app.ui.screen.feed.PostDetailScreen] before it
     * treats a missing `find_post` lookup as "not found" — the resolve must
     * land before there is anything to enrich. The manager notifies on every
     * outcome that changes state, so [snapshot] (and therefore `find_post`'s
     * result) is already current by the time this suspend call returns.
     */
    suspend fun resolvePost(postId: String) {
        host.manager()?.resolvePost(postId)
    }

    /** Resolve this post's tip surface (`monetization.md` § Tips) — fire-once
     *  via the `tips == null` guard, since nothing in the feed-index
     *  projection says whether a post has tips (unlike [resolveMedia]'s
     *  `hasMedia` data trigger). Every outcome writes a view, including "no
     *  tips", so the guard closes and the caller's pump settles. */
    fun resolvePostTips(postId: String) {
        val m = host.manager() ?: return
        // Through the payments variant seam, not `m.resolvePostTips` directly:
        // it is the one `payments`-gated member of `FfiFeedManager`, so a
        // store-safe build's bindings do not carry it and this shared file
        // could not name it (com.fauna.app.payments.PaymentsGlue).
        viewModelScope.launch { m.resolvePostTipsIfBuilt(postId) }
    }

    /** Resolve this post's self-serve teaser offer (`monetization.md` § Per-post
     *  pay-to-unlock, gap (2c)) — fire-once via the caller's `gatedTier != null
     *  && offer == null` guard, mirroring [resolvePostTips]. Unlike
     *  [resolvePostTips], NOT payments-variant-gated: `resolve_post_unlock_offer`
     *  is not a `#[cfg(feature = "payments")]` member of `FfiFeedManager` and
     *  ships in the storeSafe build's bindings too, so this calls it directly. A
     *  no-op unless `gated_tier` names a `post-unlock-*` tier; every outcome
     *  (including "no offer") writes through `PostSummary.unlock_offer` and
     *  re-emits, so the guard closes. */
    fun resolvePostUnlockOffer(postId: String) {
        val m = host.manager() ?: return
        viewModelScope.launch { m.resolvePostUnlockOffer(postId) }
    }

    /** Buy the self-serve teaser's offer (`gated-post-buy-button`) — subscribes
     *  against the offer's `tier_name` (the existing pay-to-unlock subscribe,
     *  no new nest write). Success clears `PostSummary.unlock_offer` back to
     *  `null`, the completion signal the teaser's re-render reads; a failure
     *  surfaces on [buyUnlockError], never swallowed (mirrors [deletePost]). */
    fun buyUnlockOffer(postId: String) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching { m.buyUnlockOffer(postId) }
                .onFailure { _buyUnlockError.value = it.message ?: "buy failed" }
        }
    }

    /** Resolve link-preview metadata for [url] (render-model.md § D4) — calls
     *  `fauna.linkpreview.resolve` once per url (cached), folds the `Resolved`/`Failed`
     *  state onto the post document's `LinkPreview` block, and re-emits so the feed
     *  screens paint the card. Fire-once: the screen triggers this only while the block
     *  is still `Resolving`, the same discipline as [resolveMedia] / [resolveQuotedPost]. */
    fun resolveLinkPreview(url: String) {
        val m = host.manager() ?: return
        viewModelScope.launch { m.resolveLinkPreview(url) }
    }

    /** Fetch the bytes for a feed media blob (the `RenderBlock.Image` hash folded
     *  into `PostSummary.document` by `resolve_media`, or by a gated post's unlock) —
     *  the feed image painter — and open them for decoding: a tier-restricted post's
     *  sealed photo opens under its per-post key, anything else passes through
     *  unchanged ([PostMediaOpen], `media.md` § Encryption at rest). Client glue
     *  (async byte load stays per-platform, render-model.md § boundary). */
    suspend fun fetchBlobBytes(hash: String): ByteArray? =
        PostMediaOpen.fetchAndOpen(api, host.manager(), hash)

    /** Fetch a bridged post's `ProxiedImage` bytes from its nest-relative path
     *  (render-model.md § D6c) — the proxy's answer is plaintext, so there is no
     *  open step; `null` on any failure (the placeholder stays). */
    suspend fun fetchProxiedBytes(path: String): ByteArray? =
        runCatching { api.fetchNestPathBytes(path) }.getOrNull()

    /** Check whether a feed media blob carries a verified C2PA provenance
     *  manifest (`x-c2pa` server header on the public blob route) — drives the
     *  `c2pa-badge` on post_detail. Client glue, mirrors [fetchBlobBytes]. */
    suspend fun checkBlobC2pa(hash: String): Boolean =
        runCatching { api.checkBlobC2pa(hash) }.getOrDefault(false)

    // ── Engagement cues (engagement-cues.md § Cue vocabulary & derivation) ────
    // The capture shell (ui/screen/feed/CueViewport.kt) measures honest viewport
    // dwell; ALL derivation (what is a skip / a watch-complete), the sealed
    // rollup, the put-debounce and the Layer-A/Layer-B consequences live in
    // shared Rust behind these three calls. This VM only routes.

    /** Fetch-on-session-start for the sealed `cues:v1` rollup — once per feed
     *  entry, before any observation. An absent rollup is a fresh capture; an
     *  unopenable one is an ERROR the user must see (never a silent fresh
     *  rollup, which would erase this user's other devices' cues on the next
     *  put), so it surfaces on `error-message`. */
    fun hydrateCues() {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching { m.hydrateCues() }
                .onFailure { _cueError.value = it.message ?: "cue hydrate failed" }
        }
    }

    /** Report one per-exposure observation (a card left the viewport). Failures
     *  surface on `error-message` — a dropped observation that logs nothing and
     *  does nothing is indistinguishable from a real product bug. */
    fun recordObservation(
        contentId: String,
        isMedia: Boolean,
        mediaPlayedPm: UInt?,
        dwellMsAtSkipVisibility: ULong,
        dwellMsAtLongVisibility: ULong,
        observedAtMs: ULong,
    ) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching {
                m.recordObservation(
                    contentId,
                    isMedia,
                    mediaPlayedPm,
                    dwellMsAtSkipVisibility,
                    dwellMsAtLongVisibility,
                    observedAtMs,
                )
            }.onFailure { _cueError.value = it.message ?: "cue observation failed" }
        }
    }

    /** Force a put of any unsaved cues — the background / app-close flush. The
     *  cue put is debounced (`CUE_PUT_DEBOUNCE_S`), so without this the tail of
     *  a session is lost when android kills the process after `ON_STOP`. */
    fun flushCues() {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching { m.flushCues() }
                .onFailure { _cueError.value = it.message ?: "cue flush failed" }
        }
    }

    // ── Create / delete feed, bridge subscribe (shared Rust) ──────────────────

    /** Create a custom feed; [rules] are the `(type, value, required)` triples
     *  the manager encodes via the shared `encode_filter_rule`. [factors] are
     *  the `feed-factor-*` editor's entries (empty until the android factor-
     *  weight editor lands — content-moderation-and-ranking.md § Composition). */
    fun createFeed(
        name: String,
        rules: List<FilterRuleInput>,
        combination: String,
        factors: List<FactorWeightInput> = emptyList(),
    ) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching { m.createFeed(name, rules, combination, null, null, factors) }
        }
    }

    fun deleteFeed(feedId: String) {
        val m = host.manager() ?: return
        viewModelScope.launch { runCatching { m.deleteFeed(feedId) } }
    }

    /**
     * Delete an own post (`feed-post-delete-confirm-button`; the two-step
     * confirm affordance is client glue). The shared manager builds + signs a
     * `Tombstone` over `fauna.posts.delete` and drops the post from the loaded
     * window on success, so no local list mutation is needed here — the
     * snapshot re-emits and the card vanishes (feed.md § State & data shape →
     * *Post deletion*). Only offered on the caller's own posts, gated in the
     * card on [ownActorIdHex]. A failure surfaces on [deleteError] → the page
     * `error-message`; it is never swallowed (unlike the fire-and-forget
     * interactions), because a deleted-but-still-shown post reads as at-rest
     * data loss to the next debugger.
     */
    fun deletePost(postId: String) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching { m.deletePost(postId) }
                .onFailure { _deleteError.value = it.message ?: "delete failed" }
        }
    }

    // ── Trained topic factors (topic-factors.md § Authoring surface) ─────────

    /** The trained factor a gesture on this feed trains **in context** — the
     *  feed's single `topic:*` factor, if it has exactly one. `null` ⇒ the
     *  card opens `feed-post-train-target-sheet` instead of guessing. Resolved
     *  fresh on every call (recomputed at card-build time, mirrors linux
     *  `build_post_actions_button`'s `manager.train_target_factor()`) since it
     *  can change when the user switches feeds. */
    fun trainTargetFactor(): String? = host.manager()?.trainTargetFactor()

    /** This post's current toggle state for [factor] — what paints the
     *  more/less-like-this menu items as active; survives restarts (the
     *  markers live inside the sealed model, reaching every device). */
    fun exampleLabelFor(postId: String, factor: String): TrainVerb? =
        host.manager()?.exampleLabelFor(postId, factor)

    /** Does [postId] match one of the user's muted words (topic-factors.md
     *  § Scoring) — the canonical per-post collapse signal, applying even in
     *  a chronological feed where a mute cannot sink a post. */
    fun isMuted(postId: String): Boolean = host.manager()?.isMuted(postId) ?: false

    /**
     * Run a training gesture (`feed-post-more-like-this` / `-less-like-this`):
     * the marked verb again ⇒ un-train (the exact inverse delta); anything
     * else ⇒ train (the manager applies forward/flip semantics itself —
     * `TrainResult.DuplicateSignal` on a redundant tap writes nothing, no
     * UI-side duplicate guard needed). A successful call mutates the shared
     * manager's own snapshot + notifies, so the card repaints via the normal
     * observer path — no local list mutation here. Failures surface on
     * [trainError] → the page `error-message`, mirroring [deletePost].
     */
    fun dispatchTrain(postId: String, factor: String, verb: TrainVerb, alreadyMarked: Boolean) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching {
                if (alreadyMarked) m.untrainPost(postId, factor) else m.trainPost(postId, factor, verb)
            }.onFailure { _trainError.value = it.message ?: "train failed" }
        }
    }

    /** The user's trained factors (stable `topic:<hex>` key + chosen name) —
     *  the factor-target sheet's list (`feed-post-train-target-sheet`), shown
     *  when [trainTargetFactor] resolves to `null`. */
    suspend fun trainedTopicsList(): List<com.fauna.ffi.FfiTrainedTopicRow> =
        runCatching { api.trainedTopicsList() }.getOrDefault(emptyList())

    fun subscribeBridge(kind: String, uri: String, name: String) {
        val m = host.manager() ?: return
        viewModelScope.launch { runCatching { m.subscribeBridge(kind, uri, name) } }
    }

    fun unsubscribeBridge(id: Long) {
        val m = host.manager() ?: return
        viewModelScope.launch { runCatching { m.unsubscribeBridge(id) } }
    }

    // ── Compose (validation + build in shared Rust; picker + upload are glue) ──

    /** Stage an uploaded attachment (or clear it with `null`) onto the composer,
     *  carrying the latest text/tags so `update_compose` doesn't wipe them. */
    fun stageAttachment(text: String, tags: String, file: AttachedFile?) {
        host.manager()?.updateCompose(text, tags, file)
    }

    // ── The composer's audience (`compose-gate-tier-select` and the fields it
    //    reveals) — forwarded to the shared manager as each pick is made, the
    //    screen painting it back from `snapshot.compose`. The audience never
    //    lives in the screen: it survives a restart with the draft
    //    (`ui/feed.md` § Persistence → *Only user-authored input rests*), and
    //    [submitPost] submits the manager's, so a restored tier-gated draft
    //    posts gated or is refused, never re-staged Public. The same shape as
    //    linux's forwarding, apple `FeedVM.setComposeGate`/`setComposeGateRoom`/
    //    `setComposeSell`/`setComposeGatePreview` and tui's
    //    `Action::SetGateTier`. Each answer's setter clears the other two
    //    shared-Rust-side; each keeps the manager's current teaser. ──────────

    /** Stage a tier answer (`null` = Public). */
    fun setComposeGate(tier: String?) {
        val m = host.manager() ?: return
        m.updateComposeGate(tier, m.snapshot().compose.gatePreview)
    }

    /** Stage a room answer by its hex channel id (from `snapshot.ownRooms`). */
    fun setComposeRoom(room: String) {
        val m = host.manager() ?: return
        m.updateComposeRoom(room, m.snapshot().compose.gatePreview)
    }

    /** Stage the "Sell this post…" answer with its fields. */
    fun setComposeSell(sell: SellComposeState) {
        val m = host.manager() ?: return
        m.updateComposeSell(sell, m.snapshot().compose.gatePreview)
    }

    /**
     * Stage the teaser (`compose-gate-preview-field`) ALONE — never through
     * the selected answer's setter, which would re-assert that answer (and
     * `update_compose_gate` would drop a room).
     */
    fun setComposePreview(preview: String) {
        host.manager()?.updateComposePreview(preview)
    }

    /**
     * Stage a picked file: hold its EXIF-stripped [bytes] on [host] for
     * [submitPost] — never uploaded here, see its doc for why — and stage
     * [file]'s metadata via [stageAttachment]. The one call both the real
     * picker (`FeedComposeScreen`'s file-picker callback) and the e2e
     * `compose.file`[compose-file] TestAgent injection make, so a later real
     * `post-submit-button` click's [submitPost] — on whichever `FeedVM`
     * instance is current — sees the same staged pick either way.
     */
    fun attachComposeFile(text: String, tags: String, file: AttachedFile, bytes: ByteArray) {
        host.stageAttachmentBytes(bytes)
        stageAttachment(text, tags, file)
    }

    /** Drop a staged pick before submit (the chip's remove affordance) —
     *  clears both the held bytes and the shared composer's metadata. */
    fun clearComposeAttachment(text: String, tags: String) {
        host.stageAttachmentBytes(null)
        stageAttachment(text, tags, null)
    }

    /**
     * Seal one picked attachment **for the composer's current audience** and
     * upload its parts, returning the [AttachedFile] to stage.
     *
     * **Call it at submit, after the audience is staged — never at pick time.**
     * Until 2026-09-08 android POSTed the bytes from the file-picker callback
     * under a hard-coded `PublicPost` audience, so attaching a photo and *then*
     * picking a tier left a readable copy of a restricted post's picture on the
     * nest under a hash anyone can fetch — blob GET is unauthenticated by design
     * and the nest exposes no blob DELETE, so the only fix is to never upload
     * one (`ui/media.md` § Encryption at rest: "The seal is resolved BEFORE the
     * attachment is uploaded, never after").
     *
     * The door is [FfiFeedManager.sealComposeAttachment], **not**
     * `processAndSealUpload`: that binding expresses only the two client-key
     * audiences on purpose, because a tier's period key must never cross the FFI
     * boundary. A public compose still passes through as plaintext, byte-identical
     * to the old shape.
     *
     * [staged] carries the picker's name/size for display; the hash and MIME come
     * from this call.
     */
    private suspend fun sealAndUploadAttachment(
        m: FfiFeedManager,
        raw: ByteArray,
        staged: AttachedFile?,
    ): AttachedFile {
        val prepared = m.sealComposeAttachment(raw)
        // Thumbnail first, best-effort: the primary's sidecar already names its
        // hash, and the nest does not gate the primary on the thumbnail, so a
        // thumbnail failure must never fail the post.
        prepared.thumbnail?.let { thumb ->
            runCatching { api.uploadBlobWithSidecar(thumb.sidecarCbor, thumb.bytes) }
        }
        val hash = api.uploadBlobWithSidecar(
            prepared.primary.sidecarCbor, prepared.primary.bytes
        ).hash
        return AttachedFile(
            name = staged?.name ?: "attachment",
            size = staged?.size ?: raw.size.toULong(),
            blobHash = hash,
            // The sealed class's sidecar says `application/octet-stream`; the real
            // MIME rides inside the seal, so the `MediaItem` must take it from the
            // seal's own answer — never from the sidecar, and never from the
            // picker's `contentResolver.getType(uri)` guess, which is what this
            // read before the seal existed.
            mediaType = prepared.mediaType,
        )
    }

    /**
     * Submit the composed post — gated, sold, room-restricted, or plain, in
     * the same seal→upload→create order the Rust-native Linux lead runs
     * (`client.rs::submit_post`; feed.md § Encryption at rest). Stage the latest
     * text/tags + the snapshot's staged file, then submit **the audience the
     * manager already holds** — never one passed in: the picks were forwarded
     * as they were made ([setComposeGate]/[setComposeRoom]/[setComposeSell]),
     * and a draft restored after a restart carries its audience in the manager
     * alone, so re-staging a screen's copy here had published a restored
     * tier-gated draft public (`ui/feed.md` § Persistence). Then:
     *   • a staged sale ⇒ **"Sell this post…"** (`monetization.md` § Per-post
     *     pay-to-unlock) — `prepare_sell_post` auto-mints a degenerate single-post
     *     tier and seals in one call; checked first, because `prepare_gated_blob`
     *     never reads `sell` and would take a sale down the plain path;
     *   • otherwise `prepare_gated_blob` seals against the staged tier or room,
     *     failing closed (`feed.compose_gate_no_key`) on a tier this device
     *     holds no key for;
     *   • either sealed body then uploads under the staged post's own
     *     [FfiFeedManager.gatedUploadSidecar] — a tier's, a sale's and a room's
     *     body alike, the class decided off the staged post and never by an app
     *     — and creates referencing the uploaded hash. An upload failure aborts
     *     — drops the staged post, surfaces the error on `compose-error`, keeps
     *     the composer text for a manual retry;
     *   • none set ⇒ the normal `submit_post`.
     * Validation (empty text/preview, a tier or room this device holds no key
     * for) is stamped on `compose.error` by the shared manager. Returns true
     * on success.
     *
     * The picked file's EXIF-stripped bytes — [FeedManagerHost.pendingAttachmentBytes],
     * staged by [attachComposeFile] since the pick — are sealed **here**, see
     * [sealAndUploadAttachment] for why the upload cannot happen at pick time.
     * `null` when the compose carries no attachment, which leaves every step
     * below exactly as it was; cleared on a successful submit (kept on failure,
     * so a retry still has the pick).
     */
    suspend fun submitPost(text: String, tags: String): Boolean {
        val m = host.manager() ?: return false
        val compose = m.snapshot().compose
        val staged = compose.attachedFile
        val sell = compose.sell
        val attachmentBytes = host.pendingAttachmentBytes()
        val sealed = try {
            // ── 1. The audience is already final ──
            // It decides the seal, and it was staged on the manager pick by pick,
            // so nothing is re-staged here (a re-stage would also drop a sale's
            // staged unlock tier and any stashed seal id). A SOLD post's photo
            // seals under the tier the sale itself mints, which does not exist
            // when the file is picked — so the mint is split in two and its first
            // half runs here, before the seal (a second call while a stage is
            // live is a no-op, so a retry is safe). Only when there IS an
            // attachment: with none, `prepare_sell_post` runs it inline.
            if (sell != null && attachmentBytes != null) {
                m.stageSellTier(sell.subscribersGetItFree, sell.askingPriceSats())
            }

            // ── 2. Seal the attachment for that audience, THEN upload it ──
            val attached = if (attachmentBytes != null) {
                sealAndUploadAttachment(m, attachmentBytes, staged)
            } else {
                staged
            }
            m.updateCompose(text, tags, attached)

            // ── 3. Let the SHARED manager decide gated-vs-normal ──
            if (sell != null) {
                m.prepareSellPost(
                    sell.price.trim().ifEmpty { null },
                    sell.subscribersGetItFree,
                    // The same asking price `stage_sell_tier` got above: phase one
                    // decides the tier's rank, so the two must agree.
                    sell.askingPriceSats(),
                )
            } else {
                m.prepareGatedBlob()
            }
        } catch (e: Exception) {
            // Validation, seal or attachment upload failed. The shared manager
            // stamps its own validation failures on compose.error; either way the
            // post does NOT go out without its picture — the user asked for an
            // image, and silently publishing the caption alone is the failure this
            // whole path exists to end.
            return false
        }
        return if (sealed != null) {
            val hash = try {
                api.uploadBlobWithSidecar(m.gatedUploadSidecar(), sealed).hash
            } catch (e: Exception) {
                m.abortGatedSubmit(e.message ?: "gated blob upload failed")
                return false
            }
            runCatching { m.submitGatedPost(hash) }.isSuccess
                .also { if (it) host.stageAttachmentBytes(null) }
        } else {
            runCatching { m.submitPost() }.isSuccess
                .also { if (it) host.stageAttachmentBytes(null) }
        }
    }

    /**
     * `compose-sell-asking-price` as whole sats, or `null` for "no machine
     * price" (the minted tier stays a tip target forever, never a purchase —
     * `monetization.md` § The asking price). Empty or unparseable both mean
     * none, exactly as tui's `asking_price.trim().parse::<u64>().ok()` and
     * windows' `s.AskingPrice` do; `prepare_sell_post` owns the sats→msat
     * conversion and the overflow refusal, so this is a plain text→u64 parse.
     *
     * One helper because [submitPost] must hand the SAME value to
     * `stage_sell_tier` and `prepare_sell_post` — phase one decides the tier's
     * rank, so a disagreement between the two would let the sale change arms
     * after the photo had already sealed.
     */
    private fun SellComposeState.askingPriceSats(): ULong? =
        askingPrice.trim().toULongOrNull()

    // ── Gated-post authoring / unlock (feed.md § Encryption at rest) ──────────

    /**
     * Refresh the composer's gate-to-tier options (`compose-gate-tier-select`)
     * from the local actor's own tiers. Android's feed manager persists across
     * nav (the singleton [FeedManagerHost]), so — unlike the Rust-native Linux
     * client that reloads on every feed entry — the composer refreshes explicitly
     * when it opens, so a tier just minted on the Profile page shows up in the
     * gate select. The web SPA does the same over the wasm twin. Best-effort.
     */
    fun refreshOwnTiers() {
        val m = host.manager() ?: return
        viewModelScope.launch { m.refreshOwnTiers() }
    }

    /**
     * Refresh the composer's room options (`compose-gate-tier-select`'s room
     * answers) from the installed room-post seam (`ui/feed.md` § Encryption
     * at rest → *Room-restricted — the app half*) — the room sibling of
     * [refreshOwnTiers], same entry-time trigger. [FeedManagerHost] also
     * re-reads on the conversations plane's own change tick, so this call is
     * belt-and-suspenders for the composer's own open, not the only path.
     */
    fun refreshOwnRooms() {
        val m = host.manager() ?: return
        viewModelScope.launch { m.refreshOwnRooms() }
    }

    /**
     * Detail-open unlock for a gated post: resolve the sealed full-body blob's
     * hash, fetch its bytes, and hand them to the shared manager to decrypt
     * (author custody incl. prior periods, or the reader's KeyBlob wrap entry)
     * and swap the full body into the snapshot, which re-emits so the detail
     * repaints. Fire-once from the detail screen while `gated_tier` is set and
     * `gated_unlocked` is false; a non-entitled reader's unlock fails and the
     * teaser stays (the same best-effort discipline as the media / link-preview
     * painters).
     */
    fun unlockGatedPost(postId: String) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            val hash = m.gatedBlobHash(postId) ?: return@launch
            val bytes = runCatching { api.fetchBlobBytes(hash) }.getOrNull() ?: return@launch
            runCatching { m.unlockGatedPost(postId, bytes) }
        }
    }

    // ── Post interactions — the shared FeedManager surface ───────────────────
    // Through `FfiFeedManager.interact`, never a thin `fauna.posts.interact`
    // call. The interaction bar's four counts render from the manager snapshot
    // (`PostSummary.{like,reply,repost,quote}_count`, feed.md § Interaction
    // bar), and the manager is what writes the nest's post-act counters back
    // into it, so the tapped count moves at once.
    //
    // This code used to say the raw call was "the same decision the Linux lead
    // made". It was not, by then: linux re-queried the whole feed afterwards to
    // pick the new number up. A comment naming a sibling app's behaviour is a
    // claim with no test behind it — `test_like_moves_its_own_count` is the test
    // this one lacked.

    // Like/unlike is a TOGGLE, not a one-way interact (feed.md § Interaction
    // bar → Repost's viewer_liked carrier; "the like button became a toggle
    // 2026-08-11"). The nest's like arm is idempotent per (actor, post), so
    // the plain interact(id,"like") call this used to make could record a
    // like but never take it back — call the manager's own toggle verb, which
    // rides the same interact door on the same post id in both directions.
    fun like(postId: String) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching { m.like(postId) }
                .onFailure { android.util.Log.w("FeedVM", "like failed: ${it.message}") }
        }
    }

    // Repost/un-repost is a TOGGLE off `viewer_repost_id`, not a one-way
    // interact (feed.md § Interaction bar → Repost, ratified 2026-08-10):
    // the manager's `repost` door composes the caller's empty-body
    // `Reference::Repost` post (off → on) or un-reposts it (on → off) through
    // the same `build_referencing_post` machinery `reply`/`quote` use — the
    // raw `interact(id, "repost")` this used to call creates nothing on a
    // native post, exactly the bug reply/quote were already fixed for above.
    fun repost(postId: String) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching { m.repost(postId) }
                .onFailure {
                    android.util.Log.w("FeedVM", "repost failed: ${it.message}")
                    _verbError.value = it.message ?: "repost failed"
                }
        }
    }

    // Reply/quote are COMPOSED posts, not one-way interact calls: the nest's
    // generic interact arm discards `body` entirely for a native post (it only
    // returns target info "so the client can compose a post" —
    // `FeedManager::reply`'s doc), so `interact(id, "reply"/"quote", text)`
    // accepted a typed reply/quote and silently dropped it. Call the manager's
    // dedicated `reply`/`quote` doors instead, which actually create the
    // referencing post (feed.md § Interaction bar).
    fun reply(postId: String, text: String) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching { m.reply(postId, text) }
                .onFailure {
                    android.util.Log.w("FeedVM", "reply failed: ${it.message}")
                    _verbError.value = it.message ?: "reply failed"
                }
        }
    }

    fun quote(postId: String, text: String) {
        val m = host.manager() ?: return
        viewModelScope.launch {
            runCatching { m.quote(postId, text) }
                .onFailure {
                    android.util.Log.w("FeedVM", "quote failed: ${it.message}")
                    _verbError.value = it.message ?: "quote failed"
                }
        }
    }
}
