package com.fauna.app.ui.screen.feed

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.RegionPlaneHost
import com.fauna.app.core.RegionSubject
import com.fauna.app.core.post
import com.fauna.app.ui.components.C2paBadge
import com.fauna.app.ui.components.GuardianReason
import com.fauna.app.ui.components.NestBlockWitness
import com.fauna.app.ui.components.NestPlaceholder
import com.fauna.app.ui.components.RegionBlockWitness
import com.fauna.app.ui.components.RegionPlaceholder
import com.fauna.app.ui.components.DocumentBlocks
import com.fauna.app.ui.components.ContentLabelBadge
import com.fauna.app.ui.components.GatedPostBadge
import com.fauna.app.ui.components.ProtocolBadge
import com.fauna.app.ui.components.DelegatedOriginBadge
import com.fauna.app.ui.components.UnverifiedSourceBadge
import com.fauna.app.ui.components.documentHasBlockedRemoteImage
import com.fauna.app.ui.components.documentMediaImageHash
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.localizedNested
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.FeedVM
import com.fauna.ffi.adminBlockNotice
import com.fauna.ffi.legalTakedownTombstone
import uniffi.fauna_core.RenderDocument
import uniffi.fauna_feed.PostSummary
import uniffi.fauna_feed.postOffersThreadView
import java.text.SimpleDateFormat
import java.util.*
import social.fauna.generated.Ids

/**
 * `post_detail` — renders the focal post from the **same shared snapshot** the
 * list loaded (`docs/goal/ui/feed.md` § post_detail; ui.yaml `post_detail` scope:
 * dialog, author, body, tags, image, quoted-post). No separate fetch: the post is
 * the [PostSummary] the list already holds (the `FeedManager` snapshot is shared
 * across the Feed routes via the host). The embedded quoted-post card renders via
 * the same [QuotedPostEmbed] as the list card (the uniform fix). Post
 * interactions (reply / like / repost / quote) are client glue — not the manager
 * surface — and remain untagged affordances (the interaction-bar IDs belong to
 * the feed list card, not post_detail).
 */
@Composable
fun PostDetailScreen(
    navController: NavController,
    postId: String,
    source: String,
    // The post's home nest when the link names one (the room search's post hit on
    // a foreign-homed room); blank = this nest's own post.
    homeNestUrl: String = "",
    vm: FeedVM = hiltViewModel()
) {
    val snapshot by vm.snapshot.collectAsState()
    // The `FeedSnapshot::find_post` union (feed.md § State & data shape,
    // `deep_linked_post` field doc: "Read via find_post()/rendered_posts(),
    // never directly.") — the timeline list first, then the ONE deep-link slot
    // a search hit on a post the timeline never loaded parks its fetch in
    // (feed.md § The read model → *Opening a post the timeline never loaded*).
    // A plain `posts`-only scan is exactly the shape that renders a blank
    // "post not found" dialog for a search deep link.
    val post = snapshot?.let { s ->
        s.posts.firstOrNull { it.postId == postId }
            ?: s.deepLinkedPost?.takeIf { it.postId == postId }
    }
    var replyText by remember { mutableStateOf("") }

    // Drive `FeedManager::resolve_post` (`ui/search.md` § Where logic lives →
    // *Result navigation (deep link)*) when the union above doesn't already
    // hold this id — a search hit on a post the timeline never scrolled to.
    // Gates a brief loading state rather than flashing "post not found":
    // `post == null` here is ambiguous between "not yet resolved" and
    // "genuinely unavailable" until the resolve settles, and the manager's
    // own `notify()` (fired on every outcome that changes state) is what
    // updates [snapshot] — and therefore `post` above — once it does.
    // Idempotent/cheap when the post IS already loaded (the manager's own
    // early return), so this fires unconditionally rather than re-deriving
    // that check client-side.
    var resolving by remember(postId) { mutableStateOf(post == null) }
    LaunchedEffect(postId, homeNestUrl) {
        if (post == null) {
            resolving = true
            // A link that names the post's home resolves through it
            // (`resolve_post_from`: this nest first, then the home via the relay,
            // `feed.md` § *A post homed on another nest*).
            if (homeNestUrl.isNotBlank()) vm.resolvePostFrom(postId, homeNestUrl) else vm.resolvePost(postId)
        }
        resolving = false
    }

    // Self-serve teaser-buy failures (`gated-post-buy-button`, monetization.md
    // § Per-post pay-to-unlock) — this page's own `error-message` banner, the
    // FeedScreen list card's `feed_error_buy_unlock` wiring mirrored onto the
    // per-entry-scoped [vm] `post_detail` gets from `hiltViewModel()` here.
    val appMessages = LocalAppMessages.current
    val buyUnlockError by vm.buyUnlockError.collectAsState()
    val buyUnlockErrorText = buyUnlockError?.let { stringResourceFmt(R.string.feed_error_buy_unlock, it) }
    LaunchedEffect(buyUnlockErrorText) { buyUnlockErrorText?.let { appMessages.showError(it) } }
    // Reply / quote / repost failures from this page — the list card's wiring.
    VerbErrorEffect(vm)

    // Media resolution (lazy `resolve_media` to fold the `Image` block) is owned by
    // the `FeedPostImage` painter below — the same fire-once trigger the list card
    // uses (feed.md § The read model: the feed-index projection never reads the body
    // where the blob hash lives).

    Scaffold { padding ->
        if (post == null) {
            Box(
                modifier = Modifier.fillMaxSize().padding(padding),
                contentAlignment = Alignment.Center
            ) {
                if (resolving) {
                    CircularProgressIndicator()
                } else {
                    Text(stringResource(R.string.feed_post_post_not_found))
                }
            }
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize().padding(padding),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp)
            ) {
                item {
                    Box(modifier = Modifier.testTag(Ids.FEED_POST_DETAIL_DIALOG)) {
                        MainPostContent(
                            post,
                            vm,
                            onOpenAuthor = { openAuthorProfile(navController, post, vm.ownActorIdHex()) },
                            bridgeRoster = snapshot?.bridgeRoster ?: emptyList(),
                            onQuoteClick = { vm.quote(post.postId, "") },
                            onViewThread = {
                                navController.navigate(
                                    "feed/post/${java.net.URLEncoder.encode(post.postId, "UTF-8")}/thread"
                                )
                            },
                        )
                    }
                }

                item {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        OutlinedTextField(
                            value = replyText,
                            onValueChange = { replyText = it },
                            placeholder = { Text(stringResource(R.string.feed_post_write_reply)) },
                            modifier = Modifier.weight(1f),
                            singleLine = true
                        )
                        Spacer(Modifier.width(8.dp))
                        IconButton(onClick = {
                            if (replyText.isNotBlank()) {
                                vm.reply(post.postId, replyText)
                                replyText = ""
                            }
                        }) { Icon(Icons.Default.Send, stringResource(R.string.common_send)) }
                    }
                }
            }
        }
    }

}

@Composable
private fun MainPostContent(
    post: PostSummary,
    vm: FeedVM,
    onOpenAuthor: () -> Unit,
    bridgeRoster: List<uniffi.fauna_core.BridgeIdentitySnapshot>,
    onQuoteClick: () -> Unit,
    onViewThread: () -> Unit,
) {
    val overlaysVm: com.fauna.app.ui.viewmodel.ContactOverlaysVM = androidx.hilt.navigation.compose.hiltViewModel()
    val overlayEpoch by overlaysVm.epoch.collectAsState()
    val authorLabel = remember(post.author, post.authorDisplay, overlayEpoch) {
        postAuthorLabel(overlaysVm.overlays(), post)
    }
    AuthorFaceResolve(post, vm)
    // Legal-takedown tombstone (moderation.md § Legal takedown → *App render*):
    // the nest withheld the sealed envelope under a legal obligation, so the
    // detail body collapses to the shared localized tombstone — no author /
    // tags / image / quote, mirroring the DM-bubble
    // (ConversationDetailScreen's `legalTakedownRef` branch) and the
    // quoted-post-embed (FeedScreen's `QuotedPostEmbed`) takedown branches,
    // the third of the doc's three render surfaces. No dedicated testTag
    // (presentation only, matching both precedents; a new e2e id needs
    // ui.yaml approval first, § UI Consistency A).
    //
    // Reachable only once `post` comes from the snapshot's `deepLinkedPost`
    // slot — `FeedManager::resolve_post` is the one read that sees
    // `PostGetReply.legal_takedown` and parks a `PostSummary::taken_down`
    // tombstone there (moderation.md § Legal takedown → *App render*), which
    // `PostDetailScreen`'s own `LaunchedEffect(postId)` now drives for any id
    // not already in the timeline's `posts`.
    val legalTakedownRef = post.legalTakedownRef
    if (legalTakedownRef != null) {
        Column {
            Text(
                localized(legalTakedownTombstone(legalTakedownRef)).orEmpty(),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        return
    }

    // Admin-blocked post (moderation.md § Admin block of a reported item): for any
    // viewer but its author the nest withheld the body, so the shared notice
    // stands in the body area exactly as the tombstone above does. The author's
    // own read keeps the body; the notice paints beside it (no dedicated id).
    val adminBlockReason = post.adminBlockReason
    if (adminBlockReason != null && post.body.isEmpty()) {
        Column {
            Text(
                localizedNested(adminBlockNotice(adminBlockReason)).orEmpty(),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        return
    }

    // The region placeholder in place of the detail body when a region policy
    // withholds the post — an unrevealed `collapse`, or a `block` (no reveal):
    // the same composed call the list card makes (region-blocking.md § Where it
    // composes — no render surface reaches the composed verdict for one source
    // while bypassing it for another; linux's `region_withheld`).
    val contentPolicyInputs by vm.contentPolicyInputs.collectAsState()
    val regionRevision by RegionPlaneHost.revision.collectAsState()
    val decision = remember(post, contentPolicyInputs, regionRevision) {
        RegionPlaneHost.render(post.labels, contentPolicyInputs, RegionSubject.post(post))
    }
    RegionBlockWitness("post-detail:${post.postId}", decision.isRegionBlocked)
    NestBlockWitness("post-detail:${post.postId}", decision.isNestBlocked)
    var regionRevealed by remember(post.postId) { mutableStateOf(false) }
    val regionPlaceholder = decision.placeholder
    if (regionPlaceholder != null && (regionPlaceholder.verb == "block" || !regionRevealed)) {
        RegionPlaceholder(
            regionPlaceholder,
            witnessKey = "post-detail:${post.postId}",
            onReveal = { regionRevealed = true },
        )
        return
    }
    // The nest's twin (nest-rules.md § The nest source in the engine): a rule of
    // the nest the post was read through, naming the nest and the admin's reason.
    val nestPlaceholder = decision.nestPlaceholder
    if (nestPlaceholder != null && (nestPlaceholder.verb == "block" || !regionRevealed)) {
        NestPlaceholder(
            nestPlaceholder,
            witnessKey = "post-detail:${post.postId}",
            onReveal = { regionRevealed = true },
        )
        return
    }
    // The family arm composes here too (family-safety.md § Content policy): a
    // guardian-blocked post opened from its placeholder card shows the same
    // placeholder — the fixed frame and the guardian rule's reason beside it
    // (`content-policy-blocked-reason`) — never its body. A `block` has no
    // reveal; a `collapse` shares the detail's session-local reveal.
    if (decision.verdict == "block") {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                stringResource(
                    if (decision.reported) R.string.moderation_report_hidden_placeholder
                    else R.string.family_content_blocked_notice,
                ),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .testTag(Ids.CONTENT_POLICY_BLOCKED_NOTICE)
                    .semantics { stateDescription = "blocked" },
            )
            GuardianReason(decision.guardianReason)
        }
        return
    }
    if (decision.verdict == "collapse" && !regionRevealed) {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                stringResource(R.string.family_content_collapsed_notice),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .testTag(Ids.CONTENT_POLICY_BLOCKED_NOTICE)
                    .semantics { stateDescription = "collapsed" },
            )
            GuardianReason(decision.guardianReason)
            TextButton(
                onClick = { regionRevealed = true },
                modifier = Modifier.testTag(Ids.CONTENT_POLICY_COLLAPSED_REVEAL_BUTTON),
            ) {
                Text(stringResource(R.string.family_content_reveal_button))
            }
        }
        return
    }

    // Detail-open unlock for a gated post — the shared manager resolves + fetches
    // the sealed full body and swaps it into the snapshot, so the teaser the list
    // projection carried repaints as the full post (feed.md § Encryption at rest).
    // Fire-once while gated and not yet unlocked (the same discipline as the media
    // / link-preview painters); a non-entitled reader's unlock fails and the teaser
    // stays.
    if (post.gatedTier != null && !post.gatedUnlocked) {
        LaunchedEffect(post.postId) { vm.unlockGatedPost(post.postId) }
    }
    Column {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(
                authorLabel,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
                // `click feed-post-detail-author → profile` (`feed.md` § User actions).
                modifier = Modifier
                    .clickable { onOpenAuthor() }
                    .testTag(Ids.FEED_POST_DETAIL_AUTHOR)
            )
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                if (post.source.isNotEmpty()) {
                    ProtocolBadge(source = post.source, bridgeRoster = bridgeRoster)
                }
                // The "unverified source" caveat, shown iff this client could not
                // verify the signed envelope (post.verification == FAILED); mirrors
                // the list card + linux post_detail (security.md § Client display).
                UnverifiedSourceBadge(verification = post.verification)
                // The D10 audit marker, mirroring the list card (atproto-pds-full.md
                // § D10 → Audit): an external app wrote this post as the account.
                DelegatedOriginBadge(authoringOrigin = post.authoringOrigin)
                post.gatedTier?.let { GatedPostBadge(it, roomLabel = post.roomLabel) }
                // The self-serve teaser-buy affordance (monetization.md § Per-post
                // pay-to-unlock, gap (2c)) — same row as the badge, the same
                // shared composable the list card uses.
                PostUnlockOfferTeaser(
                    offer = post.unlockOffer,
                    gatedTier = post.gatedTier,
                    postId = post.postId,
                    vm = vm,
                )
            }
        }
        // Content-label badge — the highest-confidence classifier verdict on the
        // opened post (the shared `primaryContentLabel` pick, as the list card
        // and the DM bubble use). A room's relayed verdicts fold onto a
        // deep-linked post at unlock, so this is where a member sees them even
        // with no timeline card (feed-read-model.md § the cross-nest deep-link relay).
        com.fauna.ffi.primaryContentLabel(post.labels)?.let { entry ->
            Spacer(Modifier.height(4.dp))
            ContentLabelBadge(label = entry.category)
        }
        Spacer(Modifier.height(8.dp))
        // Body — walk the shared RenderDocument (PostSummary.document, render-model.md § D6),
        // the SAME walker the Conversations page + list card use; no flat-text re-render.
        if (post.document.blocks.isNotEmpty()) {
            DocumentBlocks(
                post.document,
                modifier = Modifier.testTag(Ids.FEED_POST_DETAIL_BODY),
                style = MaterialTheme.typography.bodyLarge,
            )
            // load-remote-content-button → shared FeedManager (render-model.md § D3):
            // dispatch flips the reveal set + re-emits, so the next snapshot's
            // post.document carries RemoteImage.revealed = true and this detail recomposes.
            if (documentHasBlockedRemoteImage(post.document)) {
                TextButton(
                    onClick = { vm.revealRemoteImages(post.postId) },
                    modifier = Modifier.testTag(Ids.LOAD_REMOTE_CONTENT_BUTTON),
                ) {
                    Text(stringResource(R.string.conversations_detail_load_remote_content))
                }
            }
            Spacer(Modifier.height(8.dp))
        }

        // `feed-post-view-thread-button` (bridges.md § Bluesky-native thread
        // view): offered only on a post the shared `post_offers_thread_view`
        // says has a Bluesky thread; opens [ThreadScreen] over the shared
        // `FeedManager::open_thread`.
        if (postOffersThreadView(post)) {
            TextButton(
                onClick = onViewThread,
                modifier = Modifier.testTag(Ids.FEED_POST_VIEW_THREAD_BUTTON),
            ) {
                Text(stringResource(R.string.feed_post_view_thread))
            }
            Spacer(Modifier.height(8.dp))
        }

        // Media — paint the folded feed `Image`/`Video` block from the post `document`
        // (render-model.md § D6/D6b), the same `FeedPostImage`/`FeedPostVideo` painters
        // (fire-once `resolve_media` fold + blob-bytes fetch) the list card uses.
        if (post.hasMedia) {
            FeedPostImage(document = post.document, postId = post.postId, mediaHash = post.mediaHash, vm = vm)
            FeedPostVideo(document = post.document, postId = post.postId, vm = vm)
            PostImageC2paBadge(document = post.document, checkC2pa = { vm.checkBlobC2pa(post.postId, it) })
            Spacer(Modifier.height(8.dp))
        }

        // Link-preview cards (render-model.md § D4) — the same `FeedLinkPreviewCards`
        // painter the list card uses (one card per Resolved `LinkPreview`, og:image
        // blocked-by-default), so the detail and list stay consistent.
        FeedLinkPreviewCards(document = post.document, vm = vm)

        // Embedded quoted-post card — painted from the post `document`, the same
        // path as the list card (render-model.md § D6).
        post.quotedPostId?.let { qid ->
            QuotedPostEmbed(document = post.document, quotedPostId = qid, vm = vm)
            Spacer(Modifier.height(8.dp))
        }

        if (post.tags.isNotEmpty()) {
            Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                post.tags.forEach { tag ->
                    SuggestionChip(
                        onClick = {},
                        label = { Text("#$tag", style = MaterialTheme.typography.labelSmall) },
                        modifier = Modifier.testTag(Ids.TAG_CHIP)
                    )
                }
            }
            Spacer(Modifier.height(8.dp))
        }

        // The tip surface (`monetization.md` § Tips) — the same surface the
        // list card paints, the same fire-once trigger.
        TipSurface(tips = post.tips, postId = post.postId, vm = vm)

        Text(
            formatAbsoluteTime(post.timestamp),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant
        )
        Spacer(Modifier.height(12.dp))
        HorizontalDivider()
        Spacer(Modifier.height(8.dp))

        // Interaction affordances — client glue (not the FeedManager surface).
        Row(
            horizontalArrangement = Arrangement.spacedBy(24.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            IconButton(onClick = { vm.like(post.postId) }) {
                Icon(
                    if (post.viewerLiked) Icons.Default.Favorite else Icons.Default.FavoriteBorder,
                    stringResource(R.string.feed_like_tooltip),
                    tint = if (post.viewerLiked) InteractionActiveTint else LocalContentColor.current,
                    modifier = Modifier.size(20.dp),
                )
            }
            IconButton(onClick = { vm.repost(post.postId) }) {
                Icon(Icons.Default.Repeat, stringResource(R.string.feed_post_repost), modifier = Modifier.size(20.dp))
            }
            IconButton(onClick = onQuoteClick) {
                Icon(Icons.Default.FormatQuote, stringResource(R.string.composer_quote), modifier = Modifier.size(20.dp))
            }
            Icon(Icons.Default.ChatBubbleOutline, stringResource(R.string.common_reply), modifier = Modifier.size(20.dp))
        }
    }
}

/**
 * The `c2pa-badge` (ui.yaml, post_detail scope) — shown only when the post's
 * media blob carries a verified C2PA provenance manifest. android's own
 * feed-upload path computes `has_c2pa` for real (`process_media`+`c2pa-detect`
 * are default-on for every native UniFFI app, `fauna-ffi/Cargo.toml`), so
 * unlike web's client-side stub this check is meaningful here — one check per
 * resolved media hash (`FeedVM.checkBlobC2pa`: the `x-c2pa` HEAD as pre-filter,
 * then the shared `detectMediaC2pa` verdict over the fetched bytes), keyed so it
 * only re-fires when the hash changes. Takes the
 * check as an injected suspend lambda (mirrors `ConversationDetailScreen`'s
 * `loadLinkPreviewImageBytes`) so it's testable without a live `FeedVM`.
 */
@Composable
internal fun PostImageC2paBadge(document: RenderDocument, checkC2pa: suspend (String) -> Boolean) {
    val hash = documentMediaImageHash(document)
    val verified by produceState(initialValue = false, hash) {
        value = hash?.let { checkC2pa(it) } ?: false
    }
    if (verified) {
        C2paBadge()
    }
}

private fun formatAbsoluteTime(epochMillis: Long): String {
    val sdf = SimpleDateFormat("MMM d, yyyy 'at' HH:mm", Locale.getDefault())
    return sdf.format(Date(epochMillis))
}
