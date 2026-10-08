package com.fauna.app.ui.screen.feed

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshContainer
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.navigation.NavController
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.core.ContentPolicyInputs
import com.fauna.app.ui.components.ContentLabelBadge
import com.fauna.app.ui.components.DocumentBlocks
import com.fauna.app.ui.components.GatedPostBadge
import com.fauna.app.ui.components.ProtocolBadge
import com.fauna.app.ui.components.DelegatedOriginBadge
import com.fauna.app.ui.components.UnverifiedSourceBadge
import com.fauna.app.ui.components.documentHasBlockedRemoteImage
import com.fauna.app.ui.components.documentMediaImageHash
import com.fauna.app.ui.components.documentProxiedPostImagePath
import com.fauna.app.ui.components.documentProxiedPostVideoPath
import com.fauna.app.ui.components.documentMediaVideoHash
import com.fauna.app.ui.components.documentQuotedPost
import com.fauna.app.ui.components.documentResolvedLinkPreviews
import com.fauna.app.ui.components.documentResolvingLinkPreviewUrls
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.FeedVM
import com.fauna.ffi.legalTakedownTombstone
import com.fauna.ffi.shortId
import com.fauna.ffi.urlHost
import uniffi.fauna_core.RenderDocument
import uniffi.fauna_core.ContentLabelEntry
import uniffi.fauna_core.isSafePaymentUrl
import uniffi.fauna_feed.FactorWeightInput
import uniffi.fauna_feed.FilterRuleInput
import uniffi.fauna_feed.PostSummary
import uniffi.fauna_feed.TipView
import uniffi.fauna_feed.TrainVerb
import uniffi.fauna_feed.UnlockOfferView
import social.fauna.generated.Ids

/**
 * The Feed page, rendered **entirely** from the shared `FeedManager` snapshot
 * (`docs/goal/ui/feed.md` § State & data shape; the Rust-native Linux lead's
 * render rewrite is the reference). No client-side post-list / search
 * / feed-rule state: every read is a snapshot field and every gesture forwards to
 * a manager mutator on [FeedVM]. Post interactions (like/repost/reply) are client
 * glue — the manager snapshot doesn't carry them.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FeedScreen(navController: NavController, vm: FeedVM = hiltViewModel()) {
    val snapshot by vm.snapshot.collectAsState()
    val contentPolicyInputs by vm.contentPolicyInputs.collectAsState()
    // The post author's name is the shared overlay projection's — the viewer's
    // nickname where one is set; re-read whenever the projection moves.
    val overlaysVm: com.fauna.app.ui.viewmodel.ContactOverlaysVM = hiltViewModel()
    val overlayEpoch by overlaysVm.epoch.collectAsState()
    val appMessages = LocalAppMessages.current
    val listState = rememberLazyListState()
    var showCreateFeedDialog by remember { mutableStateOf(false) }
    var showBridgeDialog by remember { mutableStateOf(false) }

    val feeds = snapshot?.feeds ?: emptyList()
    val bridgeFeeds = snapshot?.bridgeFeeds ?: emptyList()
    // The bridges the nest can actually serve (build+runtime gated) — the
    // `bridge-form-bridge-select` option set. Driving the selector (and the +
    // affordance) from this set, not a hard-coded list, is the Dim 3 capability
    // consumption (version-compatibility.md): never offer an unsupported protocol.
    val availableBridges = snapshot?.availableBridges ?: emptyList()
    val selectedFeedId = snapshot?.selectedFeed
    // The built-in Trending virtual feed's selection (trending.md § The Trending
    // feed) — additive beside `selectedFeed`, never both set.
    val trendingSelected = snapshot?.trendingSelected ?: false
    val posts = snapshot?.posts ?: emptyList()
    val isLoading = snapshot?.status == uniffi.fauna_feed.FeedStatus.LOADING
    val hasMore = snapshot?.hasMore ?: false
    val errorText = localized(snapshot?.error)

    // Guardian Notify (family-safety.md § Guardian Notify): count every
    // currently-rendered post's guardian-floor enforcement. A no-op per post
    // unless the ward's content_notify knob is on and the guardian floor bites;
    // deduped per post per local day inside the store, so a re-render (e.g. a
    // fresh snapshot with the same posts) never re-counts. Mirrors web's
    // feed/+page.svelte effect over the whole posts list.
    LaunchedEffect(posts) { posts.forEach { vm.noteContentEnforcement(it.postId, it.labels) } }

    val pullToRefreshState = rememberPullToRefreshState()
    if (pullToRefreshState.isRefreshing) {
        LaunchedEffect(true) {
            // Re-select the CURRENT source, not `selectFeed(selectedFeedId)`:
            // the latter drops a Trending selection to Local (`selectedFeed` is
            // null while Trending is selected).
            vm.refreshCurrent()
            pullToRefreshState.endRefresh()
        }
    }

    LaunchedEffect(Unit) { vm.start() }

    LaunchedEffect(errorText) { appMessages.showError(errorText) }

    // Cue hydrate/observe/flush failures reach the same `error-message` banner
    // as the page error — a silently-dropped cue call is indistinguishable from
    // a real product bug.
    val cueError by vm.cueError.collectAsState()
    LaunchedEffect(cueError) { cueError?.let { appMessages.showError(it) } }

    // Own-post delete failures reach the same `error-message` banner (feed.md
    // § State & data shape → Post deletion). The raw manager message is wrapped
    // in the localized `feed_error_delete` template here — the VM is Context-free
    // (the same split as the page/cue errors). A success needs no handler: the
    // shared manager drops the post from the loaded window and the snapshot
    // re-emits, so the card just vanishes.
    val deleteError by vm.deleteError.collectAsState()
    val deleteErrorText = deleteError?.let { stringResourceFmt(R.string.feed_error_delete, it) }
    LaunchedEffect(deleteErrorText) { deleteErrorText?.let { appMessages.showError(it) } }

    // Training-gesture failures (feed-post-more-like-this / -less-like-this)
    // reach the same `error-message` banner, wrapped in the localized
    // feed_error_train template — mirrors the delete-error handling above.
    val trainError by vm.trainError.collectAsState()
    val trainErrorText = trainError?.let { stringResourceFmt(R.string.feed_error_train, it) }
    LaunchedEffect(trainErrorText) { trainErrorText?.let { appMessages.showError(it) } }

    // Self-serve teaser-buy failures (`gated-post-buy-button`, monetization.md
    // § Per-post pay-to-unlock) reach the same `error-message` banner, wrapped
    // in the localized feed_error_buy_unlock template — mirrors the delete/
    // train error handling above. A success needs no handler: the shared
    // manager clears `unlock_offer` and the snapshot re-emits, so the teaser
    // just vanishes.
    val buyUnlockError by vm.buyUnlockError.collectAsState()
    val buyUnlockErrorText = buyUnlockError?.let { stringResourceFmt(R.string.feed_error_buy_unlock, it) }
    LaunchedEffect(buyUnlockErrorText) { buyUnlockErrorText?.let { appMessages.showError(it) } }

    // Own-post web-publishing verb failures (web-content-hosting.md
    // § Published-post management) reach the same `error-message` banner,
    // wrapped in the `kind`-matched `web_publish_error_*` template.
    val webPublishError by vm.webPublishError.collectAsState()
    val webPublishErrorText = webPublishError?.let { (kind, message) ->
        when (kind) {
            "unpublish" -> stringResourceFmt(R.string.web_publish_error_unpublish, message)
            "paywall" -> stringResourceFmt(R.string.web_publish_error_paywall_link, message)
            else -> stringResourceFmt(R.string.web_publish_error_publish, message)
        }
    }
    LaunchedEffect(webPublishErrorText) { webPublishErrorText?.let { appMessages.showError(it) } }

    // Reply / quote / repost failures — a restricted post's refusal among them.
    VerbErrorEffect(vm)

    // This device's own actor id (hex), resolved once — the `is_own` gate for
    // the per-card delete affordance (feed.md § Post deletion). Stable for the
    // authenticated session; FeedScreen is only reachable post-auth.
    val ownActorId = remember { vm.ownActorIdHex() }

    // Muted-post reveal set (topic-factors.md § Scoring — a mute collapses
    // everywhere): session-local, per-post — the mute itself is untouched by a
    // reveal. Mirrors ConversationDetailScreen's `revealedMuted` precedent.
    val revealedMuted = remember { mutableStateListOf<String>() }

    // Engagement-cue capture (engagement-cues.md § Cue vocabulary & derivation):
    // measure honest viewport dwell while the feed list is on screen. The
    // observer's loop is scoped to this composition, so navigating away (detail,
    // compose, tab switch) cancels it and flushes what it measured.
    CueViewportObserver(listState = listState, posts = posts, vm = vm)

    // The put is debounced (CUE_PUT_DEBOUNCE_S), so the tail of a session would
    // be lost when android kills a backgrounded process. ON_STOP is the last
    // guaranteed callback — flush there.
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) { vm.flushCues() }

    // Pagination: load more when near the bottom (the snapshot supplies the order
    // and `has_more`; the manager dedups + appends — no client-side paging state).
    val shouldLoadMore by remember {
        derivedStateOf {
            val lastVisible = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            val totalItems = listState.layoutInfo.totalItemsCount
            lastVisible >= totalItems - 3 && hasMore && !isLoading
        }
    }
    LaunchedEffect(shouldLoadMore) { if (shouldLoadMore) vm.loadMore() }

    Scaffold(
        floatingActionButton = {
            FloatingActionButton(
                onClick = { navController.navigate("feed/compose") },
                modifier = Modifier.testTag(Ids.COMPOSE_BUTTON)
            ) {
                Icon(Icons.Default.Add, stringResource(R.string.composer_new_post))
            }
        },
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.feed_list_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING)
                    )
                },
                actions = {
                    IconButton(
                        onClick = { navController.navigate("feed/compose") },
                        modifier = Modifier.testTag(Ids.COMPOSE_DIALOG_BUTTON)
                    ) {
                        Icon(Icons.Default.Edit, stringResource(R.string.composer_new_post))
                    }
                }
            )
        }
    ) { padding ->
        Box(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .nestedScroll(pullToRefreshState.nestedScrollConnection)
        ) {
            Column(modifier = Modifier.fillMaxSize()) {
                // Feed selector: local + custom feeds, then create / delete / bridge.
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    LazyRow(
                        modifier = Modifier.weight(1f),
                        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                        horizontalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        // The built-in Trending virtual feed (trending.md § The
                        // Trending feed) — above the user's own feeds, mutually
                        // exclusive with Local/custom via `trendingSelected`
                        // (the manager clears one when the other is set).
                        item {
                            FilterChip(
                                selected = trendingSelected,
                                onClick = { vm.selectTrendingFeed() },
                                label = { Text(stringResource(R.string.feed_list_trending)) },
                                modifier = Modifier.testTag(Ids.FEED_TRENDING_ITEM)
                            )
                        }
                        item {
                            FilterChip(
                                selected = selectedFeedId == null && !trendingSelected,
                                onClick = { vm.selectFeed(null) },
                                label = { Text(stringResource(R.string.conflicts_local)) }
                            )
                        }
                        items(feeds) { feed ->
                            FilterChip(
                                selected = selectedFeedId == feed.feedId,
                                onClick = { vm.selectFeed(feed.feedId) },
                                label = { Text(feed.name) },
                                modifier = Modifier.testTag(Ids.FEED_ITEM)
                            )
                        }
                    }
                    // Delete the selected custom feed (no-op for the local feed).
                    IconButton(
                        onClick = { selectedFeedId?.let { vm.deleteFeed(it) } },
                        enabled = selectedFeedId != null,
                        modifier = Modifier.testTag(Ids.FEED_DELETE_BUTTON)
                    ) {
                        Icon(Icons.Default.Delete, stringResource(R.string.common_delete))
                    }
                    // Opens the bridge-subscribe form (no ui.yaml ID of its own —
                    // the form's elements are the bridge-form-* IDs). Shown only
                    // when the nest supports at least one bridge (Dim 3 gating).
                    if (availableBridges.isNotEmpty()) {
                        IconButton(
                            onClick = { showBridgeDialog = true },
                            modifier = Modifier.testTag(Ids.BRIDGE_FEED_SUBSCRIBE_TOGGLE),
                        ) {
                            Icon(Icons.Default.Link, stringResource(R.string.feed_list_subscribe_bridge))
                        }
                    }
                    IconButton(
                        onClick = { showCreateFeedDialog = true },
                        modifier = Modifier
                            .padding(end = 8.dp)
                            .testTag(Ids.FEED_CREATE_FEED_BUTTON)
                    ) {
                        Icon(Icons.Default.Add, stringResource(R.string.feed_create_title))
                    }
                }

                // Search bar (a nest re-query via set_search_query — never a
                // client-side filter over the loaded list).
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    OutlinedTextField(
                        value = snapshot?.searchQuery ?: "",
                        onValueChange = { vm.setSearchQuery(it.ifBlank { null }) },
                        placeholder = { Text(stringResource(R.string.feed_post_search_placeholder)) },
                        singleLine = true,
                        modifier = Modifier
                            .weight(1f)
                            .testTag(Ids.FEED_SEARCH_FIELD),
                    )
                    IconButton(
                        onClick = { vm.clearSearch() },
                        modifier = Modifier.testTag(Ids.FEED_SEARCH_CLEAR)
                    ) {
                        Icon(Icons.Default.Close, stringResource(R.string.common_close))
                    }
                }

                // Subscribed bridge feeds — the unsubscribe targets (a distinct
                // nest table from `feeds`).
                if (bridgeFeeds.isNotEmpty()) {
                    LazyRow(
                        modifier = Modifier.fillMaxWidth(),
                        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 4.dp),
                        horizontalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        items(bridgeFeeds) { bf ->
                            InputChip(
                                selected = false,
                                onClick = {},
                                label = { Text(bf.name) },
                                trailingIcon = {
                                    IconButton(
                                        onClick = { vm.unsubscribeBridge(bf.id) },
                                        modifier = Modifier
                                            .size(18.dp)
                                            .testTag(Ids.BRIDGE_FEED_UNSUBSCRIBE_BUTTON)
                                    ) {
                                        Icon(Icons.Default.Close, stringResource(R.string.common_remove), modifier = Modifier.size(14.dp))
                                    }
                                },
                            )
                        }
                    }
                }

                if (isLoading && posts.isEmpty()) {
                    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                        CircularProgressIndicator()
                    }
                } else if (posts.isEmpty()) {
                    // The two empty states (feed.md § Errors & edge cases) are
                    // the shared `FeedSnapshot::empty_state` decision — painted
                    // only once a read has LANDED, the variant picked by whether
                    // a search is active; never re-derived here. Idle / Error
                    // with no posts paints neither (the error has its banner).
                    val emptyState = snapshot?.let { uniffi.fauna_feed.feedEmptyState(it) }
                    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                        if (emptyState != null) {
                            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                Icon(
                                    Icons.Default.Star,
                                    contentDescription = null,
                                    modifier = Modifier.size(48.dp),
                                    tint = MaterialTheme.colorScheme.onSurfaceVariant
                                )
                                Spacer(Modifier.height(8.dp))
                                when (emptyState) {
                                    uniffi.fauna_feed.FeedEmptyState.NO_POSTS -> Text(
                                        stringResource(R.string.feed_list_no_posts),
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        modifier = Modifier.testTag(Ids.FEED_EMPTY_STATE),
                                    )
                                    uniffi.fauna_feed.FeedEmptyState.NO_MATCHES -> Text(
                                        stringResource(R.string.feed_list_no_matching_posts),
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        modifier = Modifier.testTag(Ids.FEED_NO_RESULTS),
                                    )
                                }
                            }
                        }
                    }
                } else {
                    LazyColumn(
                        state = listState,
                        modifier = Modifier.fillMaxSize().testTag(Ids.FEED_VIEW),
                        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        // `key` is load-bearing, not cosmetic: it stamps each
                        // row's post identity onto `LazyListItemInfo.key`, which
                        // is how the cue observer names the post it measured.
                        // Without it Compose falls back to POSITION keys and a
                        // list update would attribute one post's dwell to
                        // another (engagement-cues.md § Cue vocabulary —
                        // identity rides the measured element, never an
                        // index-into-snapshot read).
                        items(posts, key = { it.postId }) { post ->
                            PostCard(
                                post = post,
                                authorLabel = remember(post.author, post.authorDisplay, overlayEpoch) {
                                    postAuthorLabel(overlaysVm.overlays(), post)
                                },
                                vm = vm,
                                navController = navController,
                                ownActorId = ownActorId,
                                muteRevealed = post.postId in revealedMuted,
                                onRevealMuted = { if (post.postId !in revealedMuted) revealedMuted.add(post.postId) },
                                contentPolicyInputs = contentPolicyInputs,
                            )
                        }
                        if (hasMore) {
                            item {
                                Box(
                                    modifier = Modifier.fillMaxWidth().padding(16.dp),
                                    contentAlignment = Alignment.Center
                                ) {
                                    CircularProgressIndicator(modifier = Modifier.size(24.dp))
                                }
                            }
                        }
                    }
                }
            }

            PullToRefreshContainer(
                state = pullToRefreshState,
                modifier = Modifier.align(Alignment.TopCenter)
            )
        }
    }

    if (showCreateFeedDialog) {
        FeedCreateDialog(
            onDismiss = { showCreateFeedDialog = false },
            onCreate = { name, combination, rules, factors ->
                vm.createFeed(name, rules, combination, factors)
                showCreateFeedDialog = false
            },
            fetchTrainedFactors = { vm.trainedTopicsList() },
        )
    }

    if (showBridgeDialog) {
        BridgeSubscribeDialog(
            bridges = availableBridges,
            onDismiss = { showBridgeDialog = false },
            onSubscribe = { kind, uri, name ->
                vm.subscribeBridge(kind, uri, name)
                showBridgeDialog = false
            },
        )
    }
}

/**
 * This post's `ContentLabelBadge` label, `"category:confidence"` — the
 * highest-confidence entry of [PostSummary.labels], picked by the shared
 * `com.fauna.ffi.primaryContentLabel` (moderation.md § Per-row badge data
 * path: "one shared decision so a feed post-card ... agree on which of
 * several `labels` wins" — not re-derived per client). `null` when unlabelled.
 */
private fun contentLabelFor(labels: List<ContentLabelEntry>): String? {
    val top = com.fauna.ffi.primaryContentLabel(labels) ?: return null
    return "${top.category}:${top.confidencePerMille.toInt() / 1000.0}"
}

/**
 * The per-card ⋯ actions affordance: a `feed-post-actions-button` opening the
 * `feed-post-actions-menu` dropdown. Hosts the trained-topic-factor training
 * verbs (`feed-post-more-like-this` / `-less-like-this`, topic-factors.md
 * § Authoring surface, IDs user-approved 2026-07-09) — shown on EVERY post,
 * own or not — and own-post delete (`feed-post-delete-button` →
 * `feed-post-delete-confirm-button`, a destructive two-step inside the flyout,
 * feed.md § State & data shape → *Post deletion*, IDs user-approved
 * 2026-07-16) — mirroring the shipped conversations `dm-message-actions-menu`
 * / `dm-message-delete-button` pattern, and linux `build_post_actions_button`.
 *
 * A pure Compose leaf — no FFI seam, no `PostSummary`, no `.so` — so it runs
 * Robolectric-tested on the host JVM exactly like [FeedInteractionBarTest].
 * [markedVerb] (this post's current `example_label_for` toggle state, or
 * `null` when there's no in-context target factor) and the `is_own` gate
 * ([isOwn], matching the nest's own author check by construction — both
 * derive from `actorIdFromSecret`) are resolved one layer up in [PostCard];
 * [onTrainVerbTapped] leaves the train-in-context-vs-open-sheet branch to the
 * caller too, since that decision needs the live `trainTargetFactor()` read.
 */
@Composable
internal fun PostActionsMenu(
    isOwn: Boolean,
    markedVerb: TrainVerb?,
    onTrainVerbTapped: (TrainVerb) -> Unit,
    onDelete: () -> Unit,
    // ── Own-post web-publishing verbs (web-content-hosting.md
    // § Published-post management). State-derived off webSlug/gatedTier,
    // which the caller already resolved from the post snapshot — never a
    // per-row query. `null` webSlug means unpublished. `webLinkOrigin` gates
    // the copy affordances; `null` disables them with the reason painted
    // beside (legal but unreachable, never a dead link).
    webSlug: String? = null,
    gatedTier: String? = null,
    webLinkOrigin: String? = null,
    webLinkCopied: Triple<String, String, String>? = null,
    onPublishWeb: () -> Unit = {},
    onUnpublishWeb: () -> Unit = {},
    onCopyWebLink: () -> Unit = {},
    onCopyPaywallLink: () -> Unit = {},
    // The report verb (moderation.md § User-initiated reporting → *App surface*):
    // `feed-post-report-button` paints only on ANOTHER author's post, and only
    // when the caller built a report target for it; it opens the shared sheet
    // the shell's ReportHost paints. `null` paints nothing.
    onReport: (() -> Unit)? = null,
) {
    var expanded by remember { mutableStateOf(false) }
    // The destructive delete is a two-step inside the same flyout: tapping
    // feed-post-delete-button reveals feed-post-delete-confirm-button. Reset
    // when the menu closes so it always reopens at the first step.
    var confirmingDelete by remember { mutableStateOf(false) }

    Box {
        IconButton(
            onClick = { expanded = true },
            modifier = Modifier.size(32.dp).testTag(Ids.FEED_POST_ACTIONS_BUTTON),
        ) {
            Icon(
                Icons.Default.MoreHoriz,
                contentDescription = stringResource(R.string.feed_delete_post),
                modifier = Modifier.size(18.dp),
            )
        }
        DropdownMenu(
            expanded = expanded,
            onDismissRequest = {
                expanded = false
                confirmingDelete = false
            },
            modifier = Modifier.testTag(Ids.FEED_POST_ACTIONS_MENU),
        ) {
            // Training verbs — every post, own or not. The marked verb again ⇒
            // untrain (handled by the caller via alreadyMarked); tapping the
            // other verb flips.
            DropdownMenuItem(
                text = {
                    Text(
                        stringResource(R.string.feed_more_like_this),
                        fontWeight = if (markedVerb == TrainVerb.MORE_LIKE_THIS) FontWeight.Bold else FontWeight.Normal,
                    )
                },
                onClick = {
                    expanded = false
                    onTrainVerbTapped(TrainVerb.MORE_LIKE_THIS)
                },
                modifier = Modifier.testTag(Ids.FEED_POST_MORE_LIKE_THIS),
            )
            DropdownMenuItem(
                text = {
                    Text(
                        stringResource(R.string.feed_less_like_this),
                        fontWeight = if (markedVerb == TrainVerb.LESS_LIKE_THIS) FontWeight.Bold else FontWeight.Normal,
                    )
                },
                onClick = {
                    expanded = false
                    onTrainVerbTapped(TrainVerb.LESS_LIKE_THIS)
                },
                modifier = Modifier.testTag(Ids.FEED_POST_LESS_LIKE_THIS),
            )

            // The own-post web-publishing verbs — deliberately OUTSIDE the
            // training verbs above, which have no early-return here to hide
            // behind (linux's build_web_publish_verbs precedent).
            if (isOwn) {
                if (webSlug == null) {
                    // Unpublished: one verb, no link affordances for a page
                    // that does not exist. A default slug is the nest's to
                    // mint, so this needs no origin.
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.web_publish_publish_to_web)) },
                        onClick = {
                            expanded = false
                            onPublishWeb()
                        },
                        modifier = Modifier.testTag(Ids.FEED_POST_PUBLISH_WEB_BUTTON),
                    )
                } else {
                    if (gatedTier != null && webLinkOrigin != null) {
                        Text(
                            stringResource(R.string.web_publish_paywall_link_note),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                        )
                    }
                    if (webLinkOrigin == null) {
                        Text(
                            stringResource(R.string.web_publish_menu_no_link_reason),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                        )
                    }
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.web_publish_copy_web_link)) },
                        onClick = onCopyWebLink,
                        enabled = webLinkOrigin != null,
                        modifier = Modifier.testTag(Ids.FEED_POST_COPY_WEB_LINK_BUTTON),
                    )
                    if (gatedTier != null) {
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.web_publish_copy_paywall_link)) },
                            onClick = onCopyPaywallLink,
                            enabled = webLinkOrigin != null,
                            modifier = Modifier.testTag(Ids.FEED_POST_COPY_PAYWALL_LINK_BUTTON),
                        )
                    }
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.web_publish_unpublish)) },
                        onClick = {
                            expanded = false
                            onUnpublishWeb()
                        },
                        modifier = Modifier.testTag(Ids.FEED_POST_UNPUBLISH_WEB_BUTTON),
                    )
                    if (webLinkCopied != null) {
                        Text(
                            if (webLinkCopied.second == "web") {
                                stringResourceFmt(R.string.web_publish_copied_link, webLinkCopied.third)
                            } else {
                                stringResourceFmt(R.string.web_publish_copied_paywall_link, webLinkCopied.third)
                            },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                        )
                    }
                }
            }

            if (isOwn) {
                if (!confirmingDelete) {
                    DropdownMenuItem(
                        text = {
                            Text(
                                stringResource(R.string.feed_delete_post),
                                color = MaterialTheme.colorScheme.error,
                            )
                        },
                        onClick = { confirmingDelete = true },
                        modifier = Modifier.testTag(Ids.FEED_POST_DELETE_BUTTON),
                    )
                } else {
                    DropdownMenuItem(
                        text = {
                            Text(
                                stringResource(R.string.feed_delete_post_confirm),
                                color = MaterialTheme.colorScheme.error,
                            )
                        },
                        onClick = {
                            expanded = false
                            confirmingDelete = false
                            onDelete()
                        },
                        modifier = Modifier.testTag(Ids.FEED_POST_DELETE_CONFIRM_BUTTON),
                    )
                }
            }

            if (!isOwn && onReport != null) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.feed_report_post)) },
                    onClick = {
                        expanded = false
                        onReport()
                    },
                    modifier = Modifier.testTag(Ids.FEED_POST_REPORT_BUTTON),
                )
            }
        }
    }
}

/**
 * The factor-target sheet (`feed-post-train-target-sheet`), shown when
 * [FeedVM.trainTargetFactor] resolves to `null` — the current feed's
 * composition doesn't single out one trained topic (topic-factors.md
 * § Authoring surface). Lists the user's trained factors (stable `topic:<hex>`
 * key; display = the user's chosen name, the `feed-factor-select` pattern)
 * plus a "New trained topic…" link out to the Personalization home — matching
 * web/windows' richer sheet over linux's existing-factors-only one (priority
 * #4). FFI-touching (fetches [FeedVM.trainedTopicsList] on open) — kept OUT of
 * [PostActionsMenu] so that composable stays Robolectric-testable FFI-free.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun TrainTargetSheet(
    vm: FeedVM,
    navController: NavController,
    onDismiss: () -> Unit,
    onSelectFactor: (String) -> Unit,
) {
    var topics by remember { mutableStateOf<List<com.fauna.ffi.FfiTrainedTopicRow>>(emptyList()) }
    LaunchedEffect(Unit) {
        topics = vm.trainedTopicsList()
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        modifier = Modifier.testTag(Ids.FEED_POST_TRAIN_TARGET_SHEET),
    ) {
        Column(
            modifier = Modifier
                .padding(horizontal = 24.dp, vertical = 16.dp)
                .fillMaxWidth(),
        ) {
            Text(stringResource(R.string.feed_train_target_title), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))
            topics.forEach { row ->
                val key = row.factorKey
                if (key != null) {
                    Text(
                        row.name,
                        style = MaterialTheme.typography.bodyLarge,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { onSelectFactor(key) }
                            .padding(vertical = 12.dp),
                    )
                }
            }
            TextButton(
                onClick = {
                    onDismiss()
                    navController.navigate("settings/personalization")
                },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(stringResource(R.string.personalization_trained_factor_create))
            }
        }
    }
}

/**
 * One post row, rendered from a snapshot [PostSummary]: author, source badge,
 * timestamp (already epoch-millis), body, media image ([FeedPostImage]), tag chips,
 * the embedded quoted-post card ([QuotedPostEmbed]), and the interaction bar (client
 * glue) — the media + quote both painted from the post `document` (render-model.md
 * § D6). The whole card opens `post_detail` on click.
 *
 * Muted-keyword collapse (topic-factors.md § Scoring — a mute collapses
 * everywhere, including this chronological/local feed where a mute cannot sink
 * a post): `FeedManager::is_muted` is resolved here, at card-build time; when
 * it answers true and [muteRevealed] (session-local, held by the caller) is
 * still false, NOTHING else in the card renders — author, badges, body, media
 * and actions all stay hidden behind the `feed-post-muted` placeholder, the
 * same scope web/linux/conversations collapse (mirrors `dm-message-muted`'s
 * early-return shape).
 */
/**
 * What a post names its author — the one shared resolver over the viewer's
 * nickname for them (`contacts.md` § The private overlay). A bridged author's
 * face (`PostSummary.authorDisplay`, the display name and handle the origin
 * bridge served) fills the resolver's name and handle slots, so the chain reads
 * nickname → display name → handle → short id. Card and detail both call this.
 */
internal fun postAuthorLabel(overlays: com.fauna.ffi.FfiContactOverlays, post: PostSummary): String =
    overlays.peerLabel(post.authorDisplay?.displayName, post.authorDisplay?.handle, post.author).primary

@Composable
private fun PostCard(
    post: PostSummary,
    authorLabel: String,
    vm: FeedVM,
    navController: NavController,
    ownActorId: String?,
    muteRevealed: Boolean,
    onRevealMuted: () -> Unit,
    contentPolicyInputs: ContentPolicyInputs,
) {
    // Reply composer visibility (`feed-reply-dialog`) — raised by
    // `feed-reply-button` below, dismissed on send/cancel.
    var showReplyDialog by remember { mutableStateOf(false) }
    // Non-null while the factor-target sheet is open (topic-factors.md
    // § Authoring surface) — set by a training-verb tap when no in-context
    // factor resolves; the verb it carries is what gets trained once the user
    // picks a factor in the sheet.
    var pendingSheetVerb by remember { mutableStateOf<TrainVerb?>(null) }
    // The content-policy render verdict for this post (family-safety.md § Content
    // policy) — the strictest-wins compose of the guardian floor + the viewer's
    // own thresholds, resolved entirely in shared Rust. Memoized on labels +
    // inputs so it re-resolves only when either changes.
    //
    // The viewer's OWN report is the arm ahead of all of them (moderation.md
    // § Corollary — block also hides): a post they reported, or one whose author
    // they reported, comes back `block` with `reported` set, so the card paints
    // "You reported this" and no body. Keyed by the report subject — the post's
    // cid and author.
    val reported = remember(post.postId, post.author, contentPolicyInputs) {
        contentPolicyInputs.isReported(post.postId, post.author)
    }
    val policyVerdict = remember(post.labels, contentPolicyInputs) {
        contentPolicyInputs.verdictFor(post.labels)
    }
    val contentVerdict = if (reported) "block" else policyVerdict
    // A `collapse` floor hides the body behind a one-tap reveal, session-local
    // (the floor persists) — mirrors linux `build_content_collapse`. A `block`
    // is never revealable.
    var contentRevealed by remember(post.postId) { mutableStateOf(false) }
    // A REPOST ROW (feed.md § Interaction bar → Repost, ratified 2026-08-10):
    // attribution + the embedded original, no interaction bar of its own — the
    // repost post is empty by construction, so its own bar would be all zeros
    // (mirrors linux `is_repost_row` / windows `IsRepostRow`).
    val isRepostRow = post.repostedPostId != null
    Card(
        modifier = Modifier.fillMaxWidth().testTag(Ids.POST_CARD).clickable {
            // A repost row's own detail would be blank (the repost post is
            // empty by construction) — activate the ORIGINAL's detail instead
            // (feed.md § Interaction bar → Repost, mirrors linux's
            // `open_post_detail_by_id` / windows' `OpenPostDetailByIdAsync`).
            val targetId = post.repostedPostId ?: post.postId
            navController.navigate(
                "feed/post/${java.net.URLEncoder.encode(targetId, "UTF-8")}?source=${post.source}"
            )
        }
    ) {
        // Content-policy render enforcement (family-safety.md § Content policy):
        // precedence is block → muted-keyword → content-collapse → normal (the
        // linux ordering). A guardian `block` floor replaces the WHOLE card body
        // with a notice, checked FIRST so a block is never revealable — not even
        // past a muted reveal.
        if (contentVerdict == "block") {
            ContentPolicyBlockedBody(reported = reported)
        } else if (!muteRevealed && vm.isMuted(post.postId)) {
            Column(modifier = Modifier.padding(16.dp)) {
                Text(
                    stringResource(R.string.feed_post_muted_placeholder),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.FEED_POST_MUTED),
                )
                TextButton(
                    onClick = onRevealMuted,
                    modifier = Modifier.testTag(Ids.FEED_POST_MUTED_REVEAL_BUTTON),
                ) {
                    Text(stringResource(R.string.feed_post_muted_reveal))
                }
            }
        } else if (contentVerdict == "collapse" && !contentRevealed) {
            // A `collapse` floor (guardian OR the viewer's own spam/phishing
            // threshold) sits AFTER the muted arm — both are revealable collapses.
            ContentPolicyCollapsedBody(onReveal = { contentRevealed = true })
        } else {
            Column(modifier = Modifier.padding(16.dp)) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text(
                            text = authorLabel,
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.testTag(Ids.POST_AUTHOR)
                        )
                        // The `repost-attribution` marker (id user-approved
                        // 2026-08-11) — what lets an e2e tell a repost card
                        // from an empty-commentary quote card BY ELEMENT
                        // rather than by reading the harness state dump.
                        if (isRepostRow) {
                            Icon(
                                Icons.Default.Repeat,
                                contentDescription = null,
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.size(12.dp),
                            )
                            Text(
                                text = stringResource(R.string.feed_post_reposted_marker),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.testTag(Ids.REPOST_ATTRIBUTION),
                            )
                        }
                    }
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        if (post.source.isNotEmpty()) {
                            ProtocolBadge(source = post.source)
                        }
                        // The "unverified source" caveat (`unverified-source-badge`),
                        // shown iff THIS client could not verify the signed envelope
                        // (`post.verification == VerificationStatus.FAILED`); mirrors
                        // web/linux (security.md § App display of unverified content).
                        UnverifiedSourceBadge(verification = post.verification)
                        // The D10 audit marker (`delegated-origin-badge`): an external
                        // app wrote this post as the account, through the delegated
                        // authoring sub-key (atproto-pds-full.md § D10 → Audit). Trails
                        // the unverified badge, the order every app paints these in.
                        DelegatedOriginBadge(authoringOrigin = post.authoringOrigin)
                        // Per-category content-label verdict (`content-label-badge`) —
                        // the highest-confidence entry of PostSummary.labels
                        // (moderation.md § Per-row badge data path). Absent when
                        // unlabelled, or talking to a nest that doesn't yet project labels.
                        contentLabelFor(post.labels)?.let { ContentLabelBadge(label = it) }
                        // The gated-post badge (`gated-post-badge`) naming the tier this
                        // post is gated to, or — for a room-restricted post whose room
                        // this reader sits on the floor of — the room instead
                        // (`post.roomLabel`, `Room-restricted — the app half`, the card
                        // bullet); the card shows only the teaser until the reader opens
                        // the detail and the manager unseals (feed.md § Encryption at
                        // rest).
                        post.gatedTier?.let { GatedPostBadge(it, roomLabel = post.roomLabel) }
                        // The self-serve teaser-buy affordance (monetization.md § Per-post
                        // pay-to-unlock, gap (2c)) — same row as the badge, mirrors
                        // windows' FeedPage.xaml / linux's post_list.rs top_line group.
                        PostUnlockOfferTeaser(
                            offer = post.unlockOffer,
                            gatedTier = post.gatedTier,
                            postId = post.postId,
                            vm = vm,
                        )
                        // post.timestamp is epoch-millis (the manager already divided
                        // the FeedPostItem micros by 1000).
                        val context = LocalContext.current
                        Text(
                            ValueFormat.relativeTime(context, post.timestamp),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                        // Per-card ⋯ overflow (feed-post-actions-button →
                        // feed-post-actions-menu): the trained-topic-factor
                        // training verbs (topic-factors.md § Authoring surface,
                        // every post) + own-post delete. targetFactor/markedVerb
                        // are resolved HERE, at card-build time (recomputed on
                        // every manager notify, mirrors linux
                        // build_post_actions_button), so the pure PostActionsMenu
                        // stays FFI-free. No in-context factor (targetFactor ==
                        // null) opens the target sheet instead of training blind.
                        val targetFactor = vm.trainTargetFactor()
                        val markedVerb = targetFactor?.let { vm.exampleLabelFor(post.postId, it) }
                        val isOwnPost = ownActorId != null && post.author == ownActorId
                        // The lazy hydrate: an own post whose menu is about to
                        // render needs the origin, and this fires at most once
                        // per session (WebPublishStore's own guard) — mirrors
                        // linux/web's "hydrate on first own-post menu build".
                        if (isOwnPost) vm.ensureWebOrigin()
                        val webLinkOrigin by vm.webLinkOrigin.collectAsState()
                        val webLinkCopied by vm.webLinkCopied.collectAsState()
                        PostActionsMenu(
                            isOwn = isOwnPost,
                            markedVerb = markedVerb,
                            onTrainVerbTapped = { verb ->
                                val factor = targetFactor
                                if (factor != null) {
                                    vm.dispatchTrain(post.postId, factor, verb, alreadyMarked = markedVerb == verb)
                                } else {
                                    pendingSheetVerb = verb
                                }
                            },
                            onDelete = { vm.deletePost(post.postId) },
                            webSlug = post.webSlug,
                            gatedTier = post.gatedTier,
                            webLinkOrigin = webLinkOrigin,
                            webLinkCopied = webLinkCopied?.takeIf { it.first == post.postId },
                            onPublishWeb = { vm.publishPostToWeb(post.postId) },
                            onUnpublishWeb = { vm.unpublishPostFromWeb(post.postId) },
                            onCopyWebLink = { post.webSlug?.let { vm.copyPostWebLink(post.postId, it) } },
                            onCopyPaywallLink = { post.webSlug?.let { vm.copyPostPaywallLink(post.postId, it) } },
                            // Another author's post only (`!isOwn` inside the menu).
                            // The shared constructor carries the sealed rule once, so a
                            // gated post offers the include-text checkbox.
                            onReport = if (isOwnPost) null else {
                                {
                                    vm.openReport(
                                        com.fauna.ffi.reportPostTarget(
                                            cid = post.postId,
                                            author = post.author,
                                            plaintext = post.body,
                                            gated = post.gatedTier != null || post.gatedRoom != null,
                                        ),
                                    )
                                }
                            },
                        )
                    }
                }

                Spacer(Modifier.height(8.dp))
                // Body — walk the shared RenderDocument the feed manager built from the post body
                // (PostSummary.document, render-model.md § D6), the SAME walker the Conversations
                // page uses (DocumentBlocks); no flat-text re-render. Remote images blocked until
                // the per-post load-remote-content-button (render-time only).
                if (post.document.blocks.isNotEmpty()) {
                    DocumentBlocks(
                        post.document,
                        modifier = Modifier.testTag(Ids.FEED_POST_TEXT),
                    )
                    // load-remote-content-button → shared FeedManager (render-model.md § D3):
                    // dispatch flips the reveal set + re-emits, so the next snapshot's
                    // post.document carries RemoteImage.revealed = true and this card recomposes.
                    if (documentHasBlockedRemoteImage(post.document)) {
                        TextButton(
                            onClick = { vm.revealRemoteImages(post.postId) },
                            modifier = Modifier.testTag(Ids.LOAD_REMOTE_CONTENT_BUTTON),
                        ) {
                            Text(stringResource(R.string.conversations_detail_load_remote_content))
                        }
                    }
                } else {
                    Text(
                        text = "Post: ${post.postId.take(24)}...",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.testTag(Ids.FEED_POST_TEXT)
                    )
                }

                // Media — paint the folded feed `Image`/`Video` block from the post
                // `document` (render-model.md § D6/D6b): `FeedPostImage` triggers
                // `resolve_media` fire-once to fold the block, then fetches + paints the
                // blob bytes (the feed image painter android previously lacked — its
                // placeholder was empty); `FeedPostVideo` reads the same fold's `Video`
                // sibling and renders nothing when this attachment isn't a video.
                if (post.hasMedia) {
                    Spacer(Modifier.height(8.dp))
                    FeedPostImage(document = post.document, postId = post.postId, mediaHash = post.mediaHash, vm = vm)
                    FeedPostVideo(document = post.document)
                    // `c2pa-badge` on the list card — the same
                    // `PostImageC2paBadge`/`FeedVM.checkBlobC2pa` pair `feed.post_detail`
                    // already uses (`PostDetailScreen.kt`), lifted onto the list card too.
                    PostImageC2paBadge(document = post.document, checkC2pa = { vm.checkBlobC2pa(it) })
                }

                // Link-preview cards (render-model.md § D4): one per Resolved `LinkPreview` block in
                // the post document. Not gated on `hasMedia` — a bare-url post carries a preview with
                // no media. Fires `resolveLinkPreview` fire-once for Resolving blocks; the og:image is
                // blocked-by-default (painted only when revealed, the D3 twin).
                FeedLinkPreviewCards(document = post.document, vm = vm)

                // Embedded quoted-post card — painted from the folded `QuotedPost` block
                // in the post `document` (render-model.md § D6), rendered identically here
                // and in post_detail (the uniform fix for the per-app divergence). A REPOST
                // ROW folds in its ORIGINAL through the exact same embed (feed.md §
                // Interaction bar → Repost) — `quotedPostId`/`repostedPostId` are mutually
                // exclusive on any one post.
                (post.quotedPostId ?: post.repostedPostId)?.let { qid ->
                    Spacer(Modifier.height(8.dp))
                    QuotedPostEmbed(document = post.document, quotedPostId = qid, vm = vm)
                }

                if (post.tags.isNotEmpty()) {
                    Spacer(Modifier.height(8.dp))
                    Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        post.tags.take(5).forEach { tag ->
                            SuggestionChip(
                                onClick = {},
                                label = { Text("#$tag", style = MaterialTheme.typography.labelSmall) },
                                modifier = Modifier.testTag(Ids.TAG_CHIP)
                            )
                        }
                    }
                }

                TipSurface(tips = post.tips, postId = post.postId, vm = vm)

                // Interaction bar — client glue (NOT the FeedManager surface). Each
                // button is icon + interaction count, the count hidden when 0 (feed.md
                // § Interaction bar, ratified 2026-06-27); the counts ride the shared
                // snapshot (PostSummary.{like,reply,repost,quote}_count), not computed
                // here. Order matches the ui.yaml `interaction-bar` component
                // (like / reply / repost / quote), identical on all seven apps. A REPOST
                // ROW renders NO bar at all — its own counters are structurally dark
                // (the wrapper post is empty by construction), and the original's live
                // bar is one activation away (mirrors linux/windows).
                if (!isRepostRow) {
                    Spacer(Modifier.height(8.dp))
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        InteractionButton(
                            testTag = Ids.FEED_LIKE_BUTTON,
                            icon = if (post.viewerLiked) Icons.Default.Favorite else Icons.Default.FavoriteBorder,
                            contentDescription = stringResource(R.string.feed_like_tooltip),
                            count = post.likeCount,
                            active = post.viewerLiked,
                        ) { vm.like(post.postId) }
                        InteractionButton(
                            testTag = Ids.FEED_REPLY_BUTTON,
                            icon = Icons.Default.ChatBubbleOutline,
                            contentDescription = stringResource(R.string.common_reply),
                            count = post.replyCount,
                        ) { showReplyDialog = true }
                        InteractionButton(
                            testTag = Ids.FEED_REPOST_BUTTON,
                            icon = Icons.Default.Repeat,
                            contentDescription = stringResource(R.string.feed_post_repost),
                            count = post.repostCount,
                            // The repost TOGGLE's lit state, off the `viewerRepostId`
                            // projection (feed.md § Interaction bar → Repost) — the
                            // same idiom as the like toggle above.
                            active = post.viewerRepostId != null,
                        ) { vm.repost(post.postId) }
                        // Quote is universal (feed.md § Interaction bar): one tap
                        // composes a direct quote-repost with empty commentary through
                        // `FeedManager::quote`, as on every app — the commentary
                        // composer is a deferred fleet-wide follow-on.
                        InteractionButton(
                            testTag = Ids.FEED_QUOTE_BUTTON,
                            icon = Icons.Default.FormatQuote,
                            contentDescription = stringResource(R.string.composer_quote),
                            count = post.quoteCount,
                        ) { vm.quote(post.postId, "") }
                    }
                }
            }
        }
    }

    if (showReplyDialog) {
        ReplyComposerDialog(post = post, vm = vm, onDismiss = { showReplyDialog = false })
    }

    // The factor-target sheet (feed-post-train-target-sheet) — opened by a
    // training-verb tap when trainTargetFactor() resolved to null (no single
    // in-context factor to guess). pendingSheetVerb carries which verb to
    // train once the user picks a factor.
    pendingSheetVerb?.let { verb ->
        TrainTargetSheet(
            vm = vm,
            navController = navController,
            onDismiss = { pendingSheetVerb = null },
            onSelectFactor = { factor ->
                pendingSheetVerb = null
                vm.dispatchTrain(post.postId, factor, verb, alreadyMarked = false)
            },
        )
    }
}

/**
 * The content-policy BLOCK placeholder — a policy-naming notice in place of the
 * WHOLE post-card body, no reveal (family-safety.md § Content policy; the feed
 * twin of linux `build_content_block` / web's `contentBlocked` arm). A `block` is
 * always a guardian floor (a viewer's own threshold only ever collapses).
 * `content-policy-blocked-notice` is the one ui.yaml id this pillar renders
 * (indexed, per post-card).
 */
@Composable
private fun ContentPolicyBlockedBody(reported: Boolean = false) {
    // `reported` is the same notice for the viewer's OWN report (moderation.md
    // § Corollary): "You reported this" in place of the family words, no body, no
    // reveal — a personal filter, so it names the viewer's own act, not a policy.
    Text(
        stringResource(
            if (reported) R.string.moderation_report_hidden_placeholder
            else R.string.family_content_blocked_notice,
        ),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(16.dp).testTag(Ids.CONTENT_POLICY_BLOCKED_NOTICE),
    )
}

/**
 * The content-policy COLLAPSE placeholder + one-tap reveal (family-safety.md
 * § Content policy) — session-local reveal; the floor itself persists (the
 * guardian relaxing it, or the viewer raising their own threshold, is what stops
 * future collapse). Presentation only, no test id: v1 e2e drives the block case
 * (the linux/web precedent — `build_content_collapse`).
 */
@Composable
private fun ContentPolicyCollapsedBody(onReveal: () -> Unit) {
    Column(
        modifier = Modifier.padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            stringResource(R.string.family_content_collapsed_notice),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        TextButton(onClick = onReveal) {
            Text(stringResource(R.string.family_content_reveal_button))
        }
    }
}

/**
 * One interaction-bar affordance: a recognizable [icon] plus its [count], the
 * count **hidden when 0** (feed.md § Interaction bar, ratified 2026-06-27 — a
 * clean icon-only button until the post has activity). The whole icon+count is a
 * single tap target carrying [testTag] (the ui.yaml `feed-*-button` id), so an
 * e2e read of the tag returns the count. No word labels (priority #1 uniformity);
 * mirrors the windows/apple icon+count layout.
 */
// The lit-state tint for a toggled interaction button (like/repost) —
// android's vocabulary in the per-platform set feed.md § Implementation status
// today names (tui's `state=on/off` attr, linux's `.liked` CSS class at
// `#f43f5e`, apple's SwiftUI `.red`). Reuses the app's own existing "declined"
// red (EventDetailScreen.kt) rather than inventing a new hex.
internal val InteractionActiveTint = Color(0xFFF44336)

@Composable
internal fun InteractionButton(
    testTag: String,
    icon: ImageVector,
    contentDescription: String,
    count: Long,
    active: Boolean = false,
    onClick: () -> Unit,
) {
    // The merged node's text would otherwise be the count Text alone — empty on
    // a post with no activity, since the glyph carries no text. Declare it: the
    // icon (by its label) and the count only while it is shown, so a read of a
    // fresh post finds an icon and no number (feed.md § Interaction bar; linux's
    // `set_test_text` twin). The count Text's own semantics are cleared so the
    // number is not read twice.
    val painted = if (count > 0L) "$contentDescription $count" else contentDescription
    Row(
        modifier = Modifier
            .clip(RoundedCornerShape(4.dp))
            .clickable(onClick = onClick)
            .semantics {
                selected = active
                text = AnnotatedString(painted)
            }
            .testTag(testTag)
            .padding(horizontal = 6.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val tint = if (active) InteractionActiveTint else LocalContentColor.current
        Icon(icon, contentDescription, tint = tint, modifier = Modifier.size(20.dp))
        if (count > 0L) {
            Text(
                "$count",
                style = MaterialTheme.typography.labelSmall,
                color = tint,
                modifier = Modifier.clearAndSetSemantics {},
            )
        }
    }
}

/**
 * The reply composer raised by `feed-reply-button` (ui.yaml `feed-reply-dialog`:
 * `feed-reply-text-field` + `feed-reply-submit-button`) — composes the reply
 * through `FeedManager::reply` ([FeedVM.reply]); never `interact`, whose native
 * arm discards the typed text (feed.md § Interaction bar). Submit is a no-op on
 * blank text. A refused reply surfaces through [VerbErrorEffect].
 */
@Composable
internal fun ReplyComposerDialog(post: PostSummary, vm: FeedVM, onDismiss: () -> Unit) {
    var replyText by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        modifier = Modifier.testTag(Ids.FEED_REPLY_DIALOG),
        title = { Text(stringResource(R.string.common_reply)) },
        text = {
            OutlinedTextField(
                value = replyText,
                onValueChange = { replyText = it },
                placeholder = { Text(stringResource(R.string.feed_post_write_reply)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_REPLY_TEXT_FIELD),
                minLines = 2,
            )
        },
        confirmButton = {
            TextButton(
                onClick = {
                    if (replyText.isNotBlank()) {
                        vm.reply(post.postId, replyText)
                        onDismiss()
                    }
                },
                modifier = Modifier.testTag(Ids.FEED_REPLY_SUBMIT_BUTTON),
            ) { Text(stringResource(R.string.common_send)) }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.common_cancel)) }
        },
    )
}

/**
 * Paint a composed verb's failure (reply / quote / repost) on the page's
 * `error-message` banner: the shared `feed_refusal_i18n_key` names the copy for
 * a refusal it knows — a restricted post's `feed.reference_restricted` (feed.md
 * § Encryption at rest → *A reply, quote or repost of a restricted post*) — and
 * anything else shows its own message (web's `verbErrorCopy`). Clears the
 * channel once shown, so the same refusal again repaints.
 */
@Composable
internal fun VerbErrorEffect(vm: FeedVM) {
    val appMessages = LocalAppMessages.current
    val raw by vm.verbError.collectAsState()
    val key = raw?.let { com.fauna.ffi.feedRefusalI18nKey(it) }
    val copy = key?.let { localized(uniffi.fauna_core.LocalizedText(key = it, args = emptyMap())) } ?: raw
    LaunchedEffect(copy) {
        copy?.let {
            appMessages.showError(it)
            vm.clearVerbError()
        }
    }
}

/**
 * The embedded quoted-post card (ui.yaml `quoted-post`) — painted from the folded
 * `RenderBlock.QuotedPost` in the post `document` (render-model.md § D6). A
 * `LaunchedEffect` triggers `resolve_quoted_post` **fire-once** (only while the
 * block isn't folded yet) to fold the quote into the document (from the loaded set
 * with no fetch when possible, else one `fauna.posts.get`); the manager re-emits
 * **idempotently**, so the keyed trigger can't drive a render loop. The card then
 * reads the block's author + body. Rendered identically in the feed list card and
 * `post_detail` (the uniform fix retiring the per-app divergence, feed.md
 * § Layout & flow → post_detail).
 */
@Composable
fun QuotedPostEmbed(document: RenderDocument, quotedPostId: String, vm: FeedVM) {
    LaunchedEffect(quotedPostId) {
        if (documentQuotedPost(document) == null) {
            runCatching { vm.resolveQuotedPost(quotedPostId) }
        }
    }
    documentQuotedPost(document)?.let { q ->
        Card(modifier = Modifier.fillMaxWidth().testTag(Ids.QUOTED_POST)) {
            Column(modifier = Modifier.padding(12.dp)) {
                val takedownRef = q.legalTakedownRef
                if (takedownRef != null) {
                    // The quoted post was taken down under a legal obligation
                    // (moderation.md § Categories & enforcement item 1): the body is
                    // withheld, so paint only the shared tombstone (no author /
                    // verification — there was no envelope to decode), never a
                    // blank/broken embed.
                    Text(
                        localized(legalTakedownTombstone(takedownRef)).orEmpty(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                } else if (q.notFound) {
                    // Its author deleted the quoted post (feed.md § Post
                    // deletion: references dangle by design) — the same one
                    // line in place of author + body, never a blank embed.
                    Text(
                        stringResource(R.string.feed_post_post_not_found),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                } else {
                    // Author row: the quoted author with the unverified-source badge
                    // trailing it (the post-card order) when THIS client could not
                    // verify the *quoted* post's envelope (Slice 2b; the badge self-hides
                    // unless `q.verification == FAILED`), scoped under this `quoted-post`.
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        Text(
                            shortId(q.author),
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.primary
                        )
                        UnverifiedSourceBadge(verification = q.verification)
                        // Keyed off the *quoted* post's own origin (independent of the
                        // focal post's), scoped under this `quoted-post`.
                        DelegatedOriginBadge(authoringOrigin = q.authoringOrigin)
                    }
                    if (q.body.isNotBlank()) {
                        Spacer(Modifier.height(4.dp))
                        Text(q.body, style = MaterialTheme.typography.bodySmall, maxLines = 3)
                    }
                }
            }
        }
    }
}

/**
 * The feed post media image (ui.yaml `post-image`) — painted from the folded
 * `RenderBlock.Image` in the post `document` (render-model.md § D6). A
 * `LaunchedEffect` triggers `resolve_media` **fire-once** (only while the block
 * isn't folded yet) to fold the media `Image` block (the post's first attachment
 * hash) into the document; once folded, the hash drives an async blob-bytes fetch
 * (`vm.fetchBlobBytes` → `BitmapFactory` → `asImageBitmap`) — the feed image painter
 * android previously lacked (its `post-image` was an empty placeholder). A
 * tier-restricted post's photo is sealed under its per-post key, and
 * `vm.fetchBlobBytes` opens it through the shared manager before this decodes it —
 * a public post's bytes pass through unchanged, so this composable carries no
 * is-this-post-gated branch (`media.md` § Encryption at rest); the async byte load
 * stays client-side (render-model.md § The boundary). The `post-image` element
 * always renders — a placeholder `Box` until the bytes arrive, and for good when a
 * sealed item does not open — so the test element is stable.
 *
 * The fire-once `resolve_media` guard keys on the snapshot's [mediaHash] being
 * `null` (render-model.md § D6c), never on the document's image hash: an
 * all-remote bridged post resolves to `mediaHash == ""` and never folds a blob
 * `Image`, so a document-keyed guard re-fired the resolve on every composition.
 * Such a post's own picture — the first `ProxiedImage` of a document with no blob
 * image — paints in the same slot from its nest-relative path, fetched with the
 * session bearer like a blob ([FeedVM.fetchProxiedBytes]); no open step, and its
 * placeholder carries the path as text until the bytes arrive.
 */
@Composable
fun FeedPostImage(document: RenderDocument, postId: String, mediaHash: String?, vm: FeedVM) {
    LaunchedEffect(postId, mediaHash) {
        if (mediaHash == null) vm.resolveMedia(postId)
    }
    val hash = documentMediaImageHash(document)
    val proxiedPath = documentProxiedPostImagePath(document)
    val bitmap by produceState<ImageBitmap?>(null, hash, proxiedPath) {
        val bytes = when {
            hash != null -> vm.fetchBlobBytes(hash)
            proxiedPath != null -> vm.fetchProxiedBytes(proxiedPath)
            else -> null
        }
        value = bytes?.let { BitmapFactory.decodeByteArray(it, 0, it.size)?.asImageBitmap() }
    }
    val bmp = bitmap
    if (bmp != null) {
        Image(
            bitmap = bmp,
            contentDescription = null,
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(max = 300.dp)
                .testTag(Ids.POST_IMAGE),
        )
    } else {
        Box(modifier = Modifier.fillMaxWidth().testTag(Ids.POST_IMAGE)) {
            if (proxiedPath != null) Text(proxiedPath, style = MaterialTheme.typography.labelSmall)
        }
    }
}

/**
 * The feed post video attachment (ui.yaml `video-thumbnail`) — painted from
 * the folded `RenderBlock.Video` in the post `document` (render-model.md §
 * D6b), the exact twin of [FeedPostImage]'s `RenderBlock.Image` read: one
 * fold makes the image-vs-video branch, so a document never carries both for
 * the same attachment. [FeedPostImage]'s own `LaunchedEffect` already
 * triggers the fire-once `resolve_media` fold regardless of which block it
 * resolves to, so this composable only needs to read the result. No poster
 * frame exists to paint (`MediaItem.thumbnail`/`dimensions` are `None` from
 * every writer, deliberately — a poster field would be dead on arrival), so
 * this paints a play glyph + hash, the same choice tui made
 * (`video_thumbnail_element`). Renders nothing when the document has no
 * video block, unlike `post-image`'s stable-placeholder shape — there is no
 * async load to keep the element stable across. A bridged post's `ProxiedVideo`
 * (render-model.md § D6c → *Proxied video*) paints here too when there is no
 * blob video, its nest-relative path in the hash's place — never byte-loaded.
 */
@Composable
fun FeedPostVideo(document: RenderDocument) {
    val hash = documentMediaVideoHash(document) ?: documentProxiedPostVideoPath(document) ?: return
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.testTag(Ids.VIDEO_THUMBNAIL),
    ) {
        Icon(Icons.Default.PlayCircleOutline, contentDescription = null, modifier = Modifier.size(20.dp))
        Spacer(Modifier.width(4.dp))
        Text(hash, style = MaterialTheme.typography.labelSmall)
    }
}

/**
 * The D4 link-preview cards (ui.yaml `link-preview-card`) — one per Resolved `LinkPreview`
 * block in the post `document` (render-model.md § D4). A `LaunchedEffect` fires
 * `resolveLinkPreview` **fire-once** for each `Resolving` block (the manager calls
 * `fauna.linkpreview.resolve`, folds the `Resolved` state, and re-emits). Each card shows
 * title / description / domain (the shared `urlHost` — host without scheme/port, the same
 * source of truth as linux/web/windows) and the og:image, which is **blocked-by-default**:
 * painted only when `revealed` (the D3 twin — the post's `load-remote-content-button`, driven
 * by `documentHasBlockedRemoteImage`, reveals it). A `Resolving`/`Failed` block paints no card
 * (the inline body link already shows).
 */
@Composable
fun FeedLinkPreviewCards(document: RenderDocument, vm: FeedVM) {
    val resolvingUrls = documentResolvingLinkPreviewUrls(document)
    LaunchedEffect(resolvingUrls) {
        resolvingUrls.forEach { vm.resolveLinkPreview(it) }
    }
    val uriHandler = LocalUriHandler.current
    documentResolvedLinkPreviews(document).forEach { lp ->
        Spacer(Modifier.height(8.dp))
        Card(
            onClick = { runCatching { uriHandler.openUri(lp.url) } },
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LINK_PREVIEW_CARD),
        ) {
            Column(modifier = Modifier.padding(12.dp)) {
                // og:image — blocked-by-default (render-model.md § D4): paint only when revealed.
                val hash = lp.imageHash
                if (lp.revealed && hash != null) {
                    val bitmap by produceState<ImageBitmap?>(null, hash) {
                        value = vm.fetchBlobBytes(hash)?.let { bytes ->
                            BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
                        }
                    }
                    val bmp = bitmap
                    if (bmp != null) {
                        Image(
                            bitmap = bmp,
                            contentDescription = null,
                            modifier = Modifier
                                .fillMaxWidth()
                                .heightIn(max = 180.dp)
                                .testTag(Ids.LINK_PREVIEW_IMAGE),
                        )
                    } else {
                        Box(modifier = Modifier.fillMaxWidth().testTag(Ids.LINK_PREVIEW_IMAGE))
                    }
                }
                if (lp.title.isNotEmpty()) {
                    Text(
                        text = lp.title,
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.testTag(Ids.LINK_PREVIEW_TITLE),
                    )
                }
                if (lp.description.isNotEmpty()) {
                    Text(
                        text = lp.description,
                        style = MaterialTheme.typography.bodySmall,
                        maxLines = 2,
                        modifier = Modifier.testTag(Ids.LINK_PREVIEW_DESCRIPTION),
                    )
                }
                Text(
                    text = urlHost(lp.url),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.LINK_PREVIEW_DOMAIN),
                )
            }
        }
    }
}

/**
 * The self-serve teaser-buy affordance for a sold post (ui.yaml
 * `gated-post-price` / `gated-post-payment-link` / `gated-post-buy-button`,
 * `monetization.md` § Per-post pay-to-unlock, gap (2c)) — shared by the list
 * card and `post_detail`, the same [TipSurface]/[QuotedPostEmbed] precedent.
 * A `LaunchedEffect` fires [FeedVM.resolvePostUnlockOffer] **fire-once**
 * (guarded on `gatedTier != null && offer == null`, mirroring windows'
 * `SyncPosts` trigger): the resolve is a no-op unless `gatedTier` names a
 * `post-unlock-*` tier, and every outcome (including "no offer") writes
 * through `PostSummary.unlock_offer` and re-emits, so the guard closes.
 *
 * Present iff [offer] resolved at all (`gated-post-price` +
 * `gated-post-buy-button`); `gated-post-payment-link` additionally needs a
 * non-empty `payment_url` — mirrors apple's `PostUnlockOfferTeaser` and
 * linux's `post_list.rs` `top_line` group. `None` — an
 * undesignated/foreign tier, a transport error — renders nothing here: the
 * unchanged priceless badge + claim-code fallback (Subscription settings)
 * stays the purchase path, never an error surface.
 *
 * The payment link's https-only guard ([isSafePaymentUrl], F-CL2
 * anti-phishing-redirect) fires on CLICK, not on visibility — a non-https
 * url still shows the button, matching every reference app; a refusal shows
 * `subscriptions_unsafe_payment_url` on the same banner [buyUnlockOffer]
 * failures use. The RENDER is gated on [BuildConfig.PAYMENTS] (the price-and-route
 * class, dynamic-features.md § Platform-family surface excision): the price,
 * the payment link and the buy button state a price or route money, so the
 * storeSafe build paints none of them. The glue
 * (`resolve_post_unlock_offer`/`buy_unlock_offer` are not
 * `#[cfg(feature = "payments")]` members of `FfiFeedManager`) stays shared, and
 * the early return sits after the resolve effect so both builds keep the same
 * state machine.
 */
@Composable
fun PostUnlockOfferTeaser(offer: UnlockOfferView?, gatedTier: String?, postId: String, vm: FeedVM) {
    LaunchedEffect(postId) {
        if (gatedTier != null && offer == null) vm.resolvePostUnlockOffer(postId)
    }
    if (offer == null || !BuildConfig.PAYMENTS) return
    val context = LocalContext.current
    val uriHandler = LocalUriHandler.current
    val appMessages = LocalAppMessages.current
    Text(
        text = offer.priceHint.orEmpty(),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.testTag(Ids.GATED_POST_PRICE),
    )
    offer.paymentUrl?.takeIf { it.isNotEmpty() }?.let { url ->
        TextButton(
            onClick = {
                if (isSafePaymentUrl(url)) {
                    uriHandler.openUri(url)
                } else {
                    appMessages.showError(context.getString(R.string.subscriptions_unsafe_payment_url))
                }
            },
            modifier = Modifier.testTag(Ids.GATED_POST_PAYMENT_LINK),
        ) {
            Text(stringResource(R.string.subscriptions_payment_url), style = MaterialTheme.typography.labelSmall)
        }
    }
    TextButton(
        onClick = { vm.buyUnlockOffer(postId) },
        modifier = Modifier.testTag(Ids.GATED_POST_BUY_BUTTON),
    ) {
        Text(stringResource(R.string.feed_post_buy_button), style = MaterialTheme.typography.labelSmall)
    }
}

/**
 * The post tip surface (ui.yaml `post-tip-total` / `post-tip-count` /
 * `post-tip-list-button` / `post-tip-list` / `post-tip-item`,
 * `monetization.md` § Tips) — shared by the list card and `post_detail`, the
 * same [QuotedPostEmbed] precedent. A `LaunchedEffect` fires `resolvePostTips`
 * **fire-once** (guarded on `tips == null`): unlike [FeedPostImage]'s
 * `hasMedia` data trigger, nothing in the feed-index projection says whether a
 * post has tips, so the guard is the resolved field alone — every outcome
 * writes a view, including "no tips", so the trigger closes and settles.
 *
 * **The two counters are guarded independently, and that is the whole
 * point.** `tipCount` counts every tip; `totalMsats` sums only those whose
 * receipt reported an amount, so a post whose every receipt carried an
 * unparseable invoice renders the count and no total — rendering "0 sats"
 * there would tell the reader nobody paid (`monetization.md` § Tips: a
 * missing amount is a real state, never coerced to 0).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TipSurface(tips: TipView?, postId: String, vm: FeedVM) {
    // android's family compile condition (`dynamic-features.md` § Platform-family
    // surface excision) — `false` in the `storeSafe` build type, where the release shrinker folds
    // the branch and strips every `post-tip-*` id below out of the artifact.
    //
    // ⚠ The condition has to be HERE, on the render, not only on the resolver.
    // `TipView` and `PostSummary.tips` are deliberately UNGATED inert records
    // (snapshot.rs's own posture), so an excised build type-checks this whole
    // function, takes the `tips == null` early return forever, paints nothing —
    // and still ships all five element ids as string literals. That is the
    // defect this gate exists for, and it bit tui and linux in exactly this
    // place before it bit android.
    if (!BuildConfig.PAYMENTS) return
    LaunchedEffect(postId) {
        if (tips == null) vm.resolvePostTips(postId)
    }
    if (tips == null || tips.tipCount == 0L) return
    var listOpen by remember(postId) { mutableStateOf(false) }
    val context = LocalContext.current
    Spacer(Modifier.height(8.dp))
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (tips.totalMsats != 0L) {
            Text(
                ValueFormat.tipAmount(context, tips.totalMsats),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.POST_TIP_TOTAL),
            )
        }
        Text(
            ValueFormat.tipCount(context, tips.tipCount),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.POST_TIP_COUNT),
        )
        TextButton(
            onClick = { listOpen = true },
            modifier = Modifier.testTag(Ids.POST_TIP_LIST_BUTTON),
        ) {
            Text(stringResource(R.string.tips_list_open))
        }
    }
    if (listOpen) {
        // The `post-tip-list` attribution window — every row the nest sent,
        // unfiltered. Authenticity is settled at ingest and never at read
        // (`monetization.md` § Zap receipts), so a client-side trust check
        // here would re-open exactly the per-reader re-checking that
        // discipline exists to prevent.
        ModalBottomSheet(
            onDismissRequest = { listOpen = false },
            modifier = Modifier.testTag(Ids.POST_TIP_LIST),
        ) {
            Column(
                modifier = Modifier
                    .padding(horizontal = 24.dp, vertical = 16.dp)
                    .fillMaxWidth(),
            ) {
                // The bounded window's tail, from the nest's own `hasMore` —
                // never inferred by comparing the rendered row count against
                // a cap this client hard-codes.
                val title = if (tips.hasMore) {
                    val more = (tips.tipCount - tips.senders.size).toLong()
                    "${stringResource(R.string.tips_list_title)} — ${ValueFormat.tipMore(context, more)}"
                } else {
                    stringResource(R.string.tips_list_title)
                }
                Text(title, style = MaterialTheme.typography.titleMedium)
                Spacer(Modifier.height(8.dp))
                tips.senders.forEach { tip ->
                    // Who: the local actor when the mechanism identity resolved to
                    // one, else the mechanism-native id it published, else the
                    // localized stand-in — an outside tip still counts and still
                    // displays.
                    val who = tip.sender ?: tip.senderRef ?: stringResource(R.string.tips_sender_unknown)
                    // How much, or the honest absence. NEVER "0 sats".
                    val amount = tip.amountMsats?.let { ValueFormat.tipAmount(context, it) }
                        ?: stringResource(R.string.tips_amount_unknown)
                    Text(
                        "$who — $amount",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier
                            .testTag(Ids.POST_TIP_ITEM)
                            .padding(vertical = 4.dp),
                    )
                }
            }
        }
    }
}

/**
 * The create-feed dialog: name, combination mode (all/any), a repeatable rule
 * builder (type + value + required toggle + add), and a repeatable factor-weight
 * builder (topic-factors.md § Authoring surface). Each rule Add accumulates a
 * [FilterRuleInput] `(ruleType, value, required)` triple; each factor Add
 * accumulates a [com.fauna.ffi.FfiTrainedTopicRow]-sourced
 * [uniffi.fauna_feed.FactorWeightInput] `(factor, weightPermille, global)` triple.
 * Create hands both lists to `FeedManager::create_feed`, which encodes rules via
 * the **shared** `encode_filter_rule` (priority #2 — the single encoder, no
 * open-coded Kotlin wire shape). Mirrors the linux rule builder + macOS's
 * `MacFeedFormView` factor half, and `ui.yaml`'s feed-create scope.
 *
 * [fetchTrainedFactors] is a suspend fetch rather than a `FeedVM` dependency —
 * mirrors [TrainTargetSheet]'s own FFI-touching fetch, kept out-of-line so this
 * composable stays a pure, Robolectric-testable Compose leaf.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun FeedCreateDialog(
    onDismiss: () -> Unit,
    onCreate: (
        name: String,
        combination: String,
        rules: List<FilterRuleInput>,
        factors: List<FactorWeightInput>,
    ) -> Unit,
    fetchTrainedFactors: suspend () -> List<com.fauna.ffi.FfiTrainedTopicRow> = { emptyList() },
) {
    var name by remember { mutableStateOf("") }
    var combinationIsAll by remember { mutableStateOf(true) }
    var combinationExpanded by remember { mutableStateOf(false) }
    var ruleTypeIndex by remember { mutableStateOf(0) }
    var ruleTypeExpanded by remember { mutableStateOf(false) }
    var ruleValue by remember { mutableStateOf("") }
    // The midpoint prefill — shared `fauna_client_feed::DEFAULT_RULE_THRESHOLD`,
    // not a private "5" literal (priority #2; tui/linux already consumed it).
    var ruleThreshold by remember { mutableStateOf(com.fauna.ffi.defaultRuleThreshold()) }
    var ruleRequired by remember { mutableStateOf(true) }
    val addedRules = remember { mutableStateListOf<FilterRuleInput>() }

    // The 11-type catalog (wire value + localized label + input kind), shared
    // with the encoder that reads these same rule types (feed.md § Where logic
    // lives → Feed rule-builder presentation) — no local rule-type map, no
    // `isLabelRule` predicate, no raw-wire-key chip.
    val ruleTypeOptions = remember { com.fauna.ffi.ruleTypeOptions() }
    val selectedOption = ruleTypeOptions[ruleTypeIndex]

    // Factor picker options: the shared built-ins (`builtinFactorOptions()` —
    // engagement, trending; feed.md § Where logic lives → Feed factor-picker
    // built-ins) plus every trained topic the fetch returns (apple's
    // `builtinFactors + trainedFactorOptions` shape) as (display label,
    // wire key) pairs — a trained row's key is always its `topic:<hex>`
    // factorKey, never its (renamable) display name. `factorKey` is `null`
    // only for a corrupt (non-16-byte) registry row, so those are dropped
    // rather than offered as an unreachable selection.
    var trainedFactors by remember { mutableStateOf<List<com.fauna.ffi.FfiTrainedTopicRow>>(emptyList()) }
    LaunchedEffect(Unit) { trainedFactors = fetchTrainedFactors() }
    val factorContext = LocalContext.current
    val factorOptions = remember(trainedFactors) {
        com.fauna.ffi.builtinFactorOptions().map { o ->
            (resolveLocalized(factorContext, o.label) ?: o.value) to o.value
        } +
            trainedFactors.mapNotNull { row -> row.factorKey?.let { row.name to it } }
    }
    var factorIndex by remember { mutableStateOf(0) }
    var newFactorWeight by remember { mutableStateOf("1.0") }
    var newFactorGlobal by remember { mutableStateOf(false) }
    val addedFactors = remember { mutableStateListOf<FactorWeightInput>() }
    val selectedFactorKey = factorOptions.getOrElse(factorIndex) { factorOptions[0] }.second
    val canAddFactor = newFactorWeight.toDoubleOrNull() != null

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.feed_create_title)) },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    placeholder = { Text(stringResource(R.string.feed_create_name_placeholder)) },
                    label = { Text(stringResource(R.string.feed_create_feed_name)) },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_CREATE_FEED_NAME),
                    singleLine = true,
                )

                // Combination mode (All match / Any match).
                ExposedDropdownMenuBox(
                    expanded = combinationExpanded,
                    onExpandedChange = { combinationExpanded = it },
                    modifier = Modifier.testTag(Ids.FEED_COMBINATION_SELECT),
                ) {
                    OutlinedTextField(
                        value = stringResource(
                            if (combinationIsAll) R.string.feed_create_mode_all
                            else R.string.feed_create_mode_any
                        ),
                        onValueChange = {},
                        readOnly = true,
                        label = { Text(stringResource(R.string.feed_create_combination)) },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = combinationExpanded) },
                        modifier = Modifier.fillMaxWidth().menuAnchor(),
                    )
                    ExposedDropdownMenu(
                        expanded = combinationExpanded,
                        onDismissRequest = { combinationExpanded = false },
                    ) {
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.feed_create_mode_all)) },
                            onClick = { combinationIsAll = true; combinationExpanded = false },
                        )
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.feed_create_mode_any)) },
                            onClick = { combinationIsAll = false; combinationExpanded = false },
                        )
                    }
                }

                Text(
                    stringResource(R.string.feed_create_filter_rules),
                    style = MaterialTheme.typography.labelLarge,
                )
                if (addedRules.isNotEmpty()) {
                    // Each chip is the shared rule_summary_label — feed.md's
                    // Example-display prose (`#rust, #fauna`, `media: yes`), not
                    // the raw rule_type wire key this used to join. joinToString's
                    // lambda isn't a @Composable context, so resolve via the
                    // non-composable resolveLocalized(context, ...) rather than
                    // the localized() convenience.
                    val ruleChipContext = LocalContext.current
                    Text(
                        addedRules.joinToString(", ") { r ->
                            resolveLocalized(
                                ruleChipContext,
                                com.fauna.ffi.ruleSummaryLabel(r.ruleType, r.value, r.required),
                            ) ?: r.ruleType
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }

                // Rule type select.
                ExposedDropdownMenuBox(
                    expanded = ruleTypeExpanded,
                    onExpandedChange = { ruleTypeExpanded = it },
                    modifier = Modifier.testTag(Ids.FEED_RULE_TYPE_SELECT),
                ) {
                    OutlinedTextField(
                        value = localized(selectedOption.label) ?: selectedOption.value,
                        onValueChange = {},
                        readOnly = true,
                        label = { Text(stringResource(R.string.feed_create_filter_rules)) },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = ruleTypeExpanded) },
                        modifier = Modifier.fillMaxWidth().menuAnchor(),
                    )
                    ExposedDropdownMenu(
                        expanded = ruleTypeExpanded,
                        onDismissRequest = { ruleTypeExpanded = false },
                    ) {
                        ruleTypeOptions.forEachIndexed { idx, opt ->
                            DropdownMenuItem(
                                text = { Text(localized(opt.label) ?: opt.value) },
                                onClick = { ruleTypeIndex = idx; ruleTypeExpanded = false },
                            )
                        }
                    }
                }

                // Show only the input the selected type's encoder arm actually
                // reads (input_kind): toggles ignore `value` (hide the value
                // entry), only TEXT_AND_NUMBER (the label rules) shows the
                // threshold, and only TOGGLE offers the required/excluded choice
                // — every other type stays implicitly required (never excluded)
                // via this UI, mirroring linux's apply_input_kind.
                if (selectedOption.inputKind != com.fauna.ffi.FfiRuleInputKind.TOGGLE) {
                    // For the label rules this is the category; the threshold
                    // (0–10) rides its own field and is packed as
                    // "category:threshold" on Add (the shared encoder splits on ':').
                    OutlinedTextField(
                        value = ruleValue,
                        onValueChange = { ruleValue = it },
                        label = {
                            Text(
                                if (selectedOption.inputKind == com.fauna.ffi.FfiRuleInputKind.TEXT_AND_NUMBER)
                                    stringResource(R.string.feed_create_rule_category)
                                else localized(selectedOption.label) ?: selectedOption.value
                            )
                        },
                        modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_RULE_VALUE_INPUT),
                        singleLine = true,
                    )
                }
                if (selectedOption.inputKind == com.fauna.ffi.FfiRuleInputKind.TEXT_AND_NUMBER) {
                    OutlinedTextField(
                        value = ruleThreshold,
                        onValueChange = { ruleThreshold = it },
                        label = { Text(stringResource(R.string.feed_create_rule_threshold)) },
                        modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_RULE_THRESHOLD_INPUT),
                        singleLine = true,
                    )
                }

                if (selectedOption.inputKind == com.fauna.ffi.FfiRuleInputKind.TOGGLE) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(
                            checked = ruleRequired,
                            onCheckedChange = { ruleRequired = it },
                            modifier = Modifier.testTag(Ids.FEED_RULE_REQUIRED_TOGGLE),
                        )
                        Text(localized(com.fauna.ffi.ruleRequiredLabel(ruleRequired)) ?: stringResource(R.string.feed_create_rule_required))
                    }
                }

                Button(
                    onClick = {
                        // Label rules pack two inputs into the single value the
                        // shared encoder splits on ':'; every other rule passes
                        // its value verbatim.
                        val raw = if (selectedOption.inputKind == com.fauna.ffi.FfiRuleInputKind.TEXT_AND_NUMBER)
                            "$ruleValue:$ruleThreshold" else ruleValue
                        addedRules.add(
                            FilterRuleInput(ruleType = selectedOption.value, value = raw, required = ruleRequired)
                        )
                        ruleValue = ""
                        ruleThreshold = com.fauna.ffi.defaultRuleThreshold()
                    },
                    // `fauna_client_feed::can_add_rule` — apple's `FeedCreateForm
                    // .canAddRule`, lifted (feed.md § Add-rule gating). android
                    // rendered this button permanently enabled until this fix.
                    enabled = com.fauna.ffi.canAddRule(selectedOption.inputKind, ruleValue, ruleThreshold),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_ADD_RULE_BUTTON),
                ) { Text(stringResource(R.string.feed_create_add_rule)) }

                Text(
                    stringResource(R.string.feed_create_factors),
                    style = MaterialTheme.typography.labelLarge,
                )
                if (addedFactors.isNotEmpty()) {
                    // Plain "{factor} ×{weight}" join — same read-only-summary
                    // shape as the rule chip above (no per-row remove; android's
                    // rule builder carries none either, and ui.yaml's create_feed
                    // scope declares no remove-button id for either list).
                    Text(
                        addedFactors.joinToString(", ") { f ->
                            "${f.factor} ×${com.fauna.ffi.formatWeightPermille(f.weightPermille)}"
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }

                var factorExpanded by remember { mutableStateOf(false) }
                ExposedDropdownMenuBox(
                    expanded = factorExpanded,
                    onExpandedChange = { factorExpanded = it },
                    modifier = Modifier.testTag(Ids.FEED_FACTOR_SELECT),
                ) {
                    OutlinedTextField(
                        value = factorOptions.getOrElse(factorIndex) { factorOptions[0] }.first,
                        onValueChange = {},
                        readOnly = true,
                        label = { Text(stringResource(R.string.feed_create_factors)) },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = factorExpanded) },
                        modifier = Modifier.fillMaxWidth().menuAnchor(),
                    )
                    ExposedDropdownMenu(
                        expanded = factorExpanded,
                        onDismissRequest = { factorExpanded = false },
                    ) {
                        factorOptions.forEachIndexed { idx, (label, _) ->
                            DropdownMenuItem(
                                text = { Text(label) },
                                onClick = { factorIndex = idx; factorExpanded = false },
                            )
                        }
                    }
                }

                OutlinedTextField(
                    value = newFactorWeight,
                    onValueChange = { newFactorWeight = it },
                    placeholder = { Text(stringResource(R.string.feed_create_factor_weight_placeholder)) },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_FACTOR_WEIGHT_INPUT),
                    singleLine = true,
                )

                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(
                        checked = newFactorGlobal,
                        onCheckedChange = { newFactorGlobal = it },
                        modifier = Modifier.testTag(Ids.FEED_FACTOR_GLOBAL_TOGGLE),
                    )
                    Text(stringResource(R.string.feed_create_factor_global_toggle))
                }

                Button(
                    onClick = {
                        addedFactors.add(
                            FactorWeightInput(
                                factor = selectedFactorKey,
                                weightPermille = com.fauna.ffi.parseWeightPermille(newFactorWeight),
                                global = newFactorGlobal,
                            )
                        )
                        newFactorWeight = "1.0"
                        newFactorGlobal = false
                    },
                    enabled = canAddFactor,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_ADD_FACTOR_BUTTON),
                ) { Text(stringResource(R.string.feed_create_add_factor)) }
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    val trimmed = name.trim()
                    if (trimmed.isNotEmpty()) {
                        onCreate(
                            trimmed,
                            if (combinationIsAll) "all" else "any",
                            addedRules.toList(),
                            addedFactors.toList(),
                        )
                    }
                },
                enabled = name.trim().isNotEmpty(),
                modifier = Modifier.testTag(Ids.CREATE_FEED),
            ) { Text(stringResource(R.string.common_create)) }
        },
        dismissButton = {
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.testTag(Ids.FEED_CREATE_CANCEL),
            ) { Text(stringResource(R.string.common_cancel)) }
        },
    )
}

/** The bridge-subscribe form (`bridge-form-*`) — subscribe a Bluesky /
 *  ActivityPub URI as a synthesised feed via `FeedManager::subscribe_bridge`. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun BridgeSubscribeDialog(
    bridges: List<uniffi.fauna_feed.AvailableBridge>,
    onDismiss: () -> Unit,
    onSubscribe: (kind: String, uri: String, name: String) -> Unit,
) {
    // Options = the bridges the nest can actually serve (snapshot.available_bridges,
    // build+runtime gated), never a hard-coded protocol list — Dim 3 consumption.
    var kindIndex by remember { mutableStateOf(0) }
    var kindExpanded by remember { mutableStateOf(false) }
    var uri by remember { mutableStateOf("") }
    var name by remember { mutableStateOf("") }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.feed_list_subscribe_bridge)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                ExposedDropdownMenuBox(
                    expanded = kindExpanded,
                    onExpandedChange = { kindExpanded = it },
                    modifier = Modifier.testTag(Ids.BRIDGE_FORM_BRIDGE_SELECT),
                ) {
                    OutlinedTextField(
                        value = bridges.getOrNull(kindIndex)?.name ?: "",
                        onValueChange = {},
                        readOnly = true,
                        label = { Text(stringResource(R.string.feed_bridge_form_kind)) },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = kindExpanded) },
                        modifier = Modifier.fillMaxWidth().menuAnchor(),
                    )
                    ExposedDropdownMenu(
                        expanded = kindExpanded,
                        onDismissRequest = { kindExpanded = false },
                    ) {
                        bridges.forEachIndexed { idx, b ->
                            DropdownMenuItem(
                                text = { Text(b.name) },
                                onClick = { kindIndex = idx; kindExpanded = false },
                            )
                        }
                    }
                }
                OutlinedTextField(
                    value = uri,
                    onValueChange = { uri = it },
                    label = { Text(stringResource(R.string.feed_bridge_form_uri)) },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.BRIDGE_FORM_URI_INPUT),
                    singleLine = true,
                )
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text(stringResource(R.string.feed_bridge_form_name)) },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.BRIDGE_FORM_NAME_INPUT),
                    singleLine = true,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { bridges.getOrNull(kindIndex)?.let { onSubscribe(it.id, uri.trim(), name.trim()) } },
                enabled = uri.isNotBlank() && bridges.isNotEmpty(),
                modifier = Modifier.testTag(Ids.BRIDGE_FORM_SUBSCRIBE_BUTTON),
            ) { Text(stringResource(R.string.feed_list_subscribe_bridge)) }
        },
        dismissButton = {
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.testTag(Ids.BRIDGE_FORM_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.common_cancel)) }
        },
    )
}
