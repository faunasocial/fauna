package com.fauna.app.ui.screen.profile

import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.screen.feed.FeedVerbFailureEffects
import com.fauna.app.ui.screen.feed.PostCard
import com.fauna.app.ui.screen.feed.postAuthorLabel
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.ContactOverlaysVM
import com.fauna.app.ui.viewmodel.FeedVM
import social.fauna.generated.Ids

// The profile page's Posts tab — **the built half** (`profile.md` § Layout &
// flow → *The Posts tab*). Compiled into `debug`, `release`, `storeSafe` and
// `foss` from `src/noKids/`; the `kids` build type compiles the empty twin under
// `src/kids/java/com/fauna/app/ui/screen/profile/ProfilePostsSection.kt`
// instead, since a kids artifact carries no feed (`family-safety.md` § The
// account age band, the kids-app bullet, item (4)) — the same seam as
// `KidsExcisedNav`.

/** How close (px) to the bottom of the profile's scroll the next page is read —
 *  the feed's own load-more gesture (scrolling near the end of the list). */
private const val LOAD_MORE_THRESHOLD_PX = 600

/**
 * The Posts tab's content: the viewed actor's posts, newest first, as the feed
 * paints them. The tab adds no post logic of its own (mirrors tui's
 * `profile/posts.rs`):
 *
 * - **The rows** come from an **author window** of the feed manager —
 *   [FeedVM.openAuthorWindow] mints `FfiFeedManager.authorWindow(actorId)` off
 *   the timeline's manager on EVERY open of the tab (this composable entering
 *   composition), reading `fauna.feed.author.posts` (`feed.md` § State & data
 *   shape → *The author window*).
 * - **Each row is the feed's own [PostCard]** with every child — interaction bar,
 *   badges, quoted post, media, the ⋯ menu — over a [FeedVM] bound to the window,
 *   so the card's ⋯ menu, reply dialog and tip list open HERE, in place, and every
 *   verb they fire (like / repost / quote / reply, train, delete, the buy) acts on
 *   the window, whose counts and rows then move.
 * - **Activating a card** is the door a search result uses: the card navigates to
 *   the feed's `feed/post/{id}` detail, whose own (timeline) [FeedVM] runs
 *   `resolvePost` before painting (`feed.md` § The read model → *Opening a post the
 *   timeline never loaded*).
 * - **Empty / loading / error.** `profile-posts-empty` renders iff the shared
 *   `FeedSnapshot::empty_state()` says empty — the `feed-empty-state` decision;
 *   nothing below the section while the first read is in flight; a failed read
 *   lands on the page's `error-message`.
 * - **Paging** is the feed's load-more gesture: scrolling near the end of the
 *   page ([scrollState] is the profile page's own scroll, which hosts the cards).
 */
@Composable
fun ProfilePostsSection(
    navController: NavController,
    actorIdHex: String,
    scrollState: ScrollState,
    vm: FeedVM = hiltViewModel(),
    overlaysVm: ContactOverlaysVM = hiltViewModel(),
) {
    LaunchedEffect(actorIdHex) { vm.openAuthorWindow(actorIdHex) }

    val snapshot by vm.windowSnapshot.collectAsState()
    val contentPolicyInputs by vm.contentPolicyInputs.collectAsState()
    val overlayEpoch by overlaysVm.epoch.collectAsState()
    val appMessages = LocalAppMessages.current
    val posts = snapshot?.posts ?: emptyList()

    // The window's read failure on the page's `error-message`; when it clears,
    // the banner clears only if it still shows THIS text (the profile page's
    // other sections share the banner).
    val errorText = localized(snapshot?.error)
    var shownError by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(errorText) {
        if (errorText != null) {
            appMessages.showError(errorText)
        } else if (shownError != null && appMessages.error.value == shownError) {
            appMessages.showError(null)
        }
        shownError = errorText
    }
    // The card's verb failures, exactly as on the feed page.
    FeedVerbFailureEffects(vm)
    // Guardian Notify counts every rendered post's floor enforcement, as the
    // feed page does (family-safety.md § Guardian Notify).
    LaunchedEffect(posts) { posts.forEach { vm.noteContentEnforcement(it.postId, it.labels) } }

    val ownActorId = remember { vm.ownActorIdHex() }
    val revealedMuted = remember { mutableStateListOf<String>() }
    val bridgeRoster = snapshot?.bridgeRoster ?: emptyList()

    val shouldLoadMore by remember {
        derivedStateOf {
            val s = snapshot
            s != null && s.hasMore && s.status != uniffi.fauna_feed.FeedStatus.LOADING &&
                scrollState.value >= scrollState.maxValue - LOAD_MORE_THRESHOLD_PX
        }
    }
    LaunchedEffect(shouldLoadMore) { if (shouldLoadMore) vm.loadMore() }

    ProfilePostsContent(
        postKeys = posts.map { it.postId },
        showEmpty = snapshot?.let { uniffi.fauna_feed.feedEmptyState(it) } != null,
    ) { index ->
        val post = posts[index]
        PostCard(
            post = post,
            authorLabel = remember(post.author, post.authorDisplay, overlayEpoch) {
                postAuthorLabel(overlaysVm.overlays(), post)
            },
            vm = vm,
            navController = navController,
            ownActorId = ownActorId,
            bridgeRoster = bridgeRoster,
            muteRevealed = post.postId in revealedMuted,
            onRevealMuted = { if (post.postId !in revealedMuted) revealedMuted.add(post.postId) },
            contentPolicyInputs = contentPolicyInputs,
        )
    }
}

/**
 * Stateless paint of the tab — `profile-posts-section` (the content host, present
 * whenever the Posts tab is active), then `profile-posts-empty` iff [showEmpty],
 * then one [card] per row, keyed by post id. FFI-free so the Robolectric harness
 * drives it with any card.
 */
@Composable
internal fun ProfilePostsContent(
    postKeys: List<String>,
    showEmpty: Boolean,
    card: @Composable (index: Int) -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp)
            .testTag(Ids.PROFILE_POSTS_SECTION),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (showEmpty) {
            Text(
                stringResource(R.string.profile_no_posts),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.PROFILE_POSTS_EMPTY),
            )
        }
        postKeys.forEachIndexed { index, postKey -> key(postKey) { card(index) } }
    }
}
