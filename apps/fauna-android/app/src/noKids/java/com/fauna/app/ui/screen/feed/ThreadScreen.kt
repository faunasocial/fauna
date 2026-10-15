package com.fauna.app.ui.screen.feed

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.FeedVM
import social.fauna.generated.Ids
import uniffi.fauna_feed.ThreadPostView
import uniffi.fauna_feed.ThreadStatus
import uniffi.fauna_feed.threadPostAuthorLabel

/**
 * ui.yaml `feed.sub_pages.thread` — the Bluesky thread page (`bridges.md`
 * § Bluesky-native thread view), reached from `feed-post-view-thread-button`
 * on the post detail. All state is the shared `FeedSnapshot.thread`
 * (`FeedManager::open_thread`): one `feed-thread-post` per post in thread order
 * (ancestors oldest-first, the opened post, its direct replies), the opened
 * post's `selected` state carried as its `stateDescription` — the nest names it,
 * no app guesses. A failed fetch paints no posts and its reason on the page's
 * `error-message`. Back pops to the post detail and closes the shared page, so
 * a slow fetch never lands on a page the reader left.
 */
@Composable
fun ThreadScreen(
    navController: NavController,
    postId: String,
    vm: FeedVM = hiltViewModel()
) {
    val snapshot by vm.snapshot.collectAsState()
    val page = snapshot?.thread?.takeIf { it.postId == postId }

    DisposableEffect(postId) {
        vm.openThread(postId)
        onDispose { vm.closeThread() }
    }

    val appMessages = LocalAppMessages.current
    val errorText = page?.error?.let { localized(it) }
    LaunchedEffect(errorText) { errorText?.let { appMessages.showError(it) } }

    Scaffold { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .testTag(Ids.FEED_THREAD_PAGE)
        ) {
            Text(
                stringResource(R.string.feed_post_thread),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(16.dp),
            )
            if (page == null || page.status == ThreadStatus.LOADING) {
                Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator()
                }
            } else {
                LazyColumn(
                    contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    itemsIndexed(page.posts) { _, post -> ThreadPost(post) }
                }
            }
        }
    }
}

/** One `feed-thread-post` — ONE text node reading `‹author› ‹post text›`
 *  (the tui leg's shape; the bridge reads a node's own text), the author from
 *  the shared `thread_post_author_label`, `selected` `"true"` on the focal
 *  post only. */
@Composable
private fun ThreadPost(post: ThreadPostView) {
    val author = remember(post) { threadPostAuthorLabel(post) }
    val primary = MaterialTheme.colorScheme.primary
    Surface(
        tonalElevation = if (post.isFocal) 2.dp else 0.dp,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            buildAnnotatedString {
                withStyle(SpanStyle(color = primary, fontWeight = FontWeight.SemiBold)) { append(author) }
                append(" ")
                append(post.text)
            },
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier
                .padding(8.dp)
                .testTag(Ids.FEED_THREAD_POST)
                .semantics { stateDescription = if (post.isFocal) "true" else "false" },
        )
    }
}
