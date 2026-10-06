package com.fauna.app.ui.screen.search

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.SearchVM
import uniffi.fauna_client_search.SearchNav
import uniffi.fauna_client_search.SearchResultRow
import uniffi.fauna_conversations.ConversationsManager
import java.net.URLEncoder
import social.fauna.generated.Ids

/**
 * Route an activated `search-result-item`'s typed [SearchNav] target into the
 * destination page's own gesture — the android twin of tui's `open_result`
 * (`apps/fauna-tui/src/search.rs`, the lead-app reference this mirrors;
 * `docs/goal/ui/search.md` § User actions + § Where logic lives → *Result
 * navigation (deep link)*).
 *
 * - `Post` — no id-space resolve needed: post-class `content_id` **is** the
 *   post id (ratified wire contract), so the row's id passes straight into
 *   the existing `feed/post/{postId}` route. [PostDetailScreen] resolves an
 *   unloaded post from there (`docs/goal/ui/feed.md` § The read model →
 *   *Opening a post the timeline never loaded*).
 * - `Draft`/`Mail` — thread/message ids pass straight into the existing
 *   `conversation/{threadId}` route. `Draft` uses `ConversationsManager.
 *   selectThread`, exactly mirroring [com.fauna.app.ui.screen.conversations.ConversationListScreen]'s
 *   `onOpenThread`; a `Draft` with no `threadId` is the single-slot new-thread
 *   composer, mirroring `onNewConversation`. `Mail` uses `selectThreadAndMessage`
 *   — the whole of `SearchNav::Mail`'s contract (thread jump + message select,
 *   `docs/goal/ui/search.md` § State & data shape) — which IS exported
 *   (`ConversationsManagerInterface.selectThreadAndMessage`, unlike the
 *   Contact/File locates below). It lands the write side: nothing on android
 *   reads `ThreadDetail.selectedMessageId` back to scroll/highlight yet
 *   (tui's own `focus_selected_message` — `apps/fauna-tui/src/search.rs` —
 *   has no android analogue), so this jump lands correctly on the thread
 *   without visibly landing ON the selected message. That's a real, separate
 *   gap (out of this render-lift's assigned scope, which asked only for
 *   "open the thread"), not blocked on any missing FFI.
 * - `Contact`/`File` — both need an id-space *resolve* through shared Rust,
 *   not a wire-up: the row's identity and the destination's key are
 *   deliberately different spellings, and matching one for the other would
 *   compile, run, open nothing and raise nothing (silent failure, not a
 *   crash). Now that `FfiCarddavClient.locateCardByUidHash` /
 *   `MediaMachine.locateFile` are exported (both landed 2026-08-14; the
 *   latter turns out to self-export from `fauna-media-machine` directly, so
 *   it was available all along), neither arm navigates straight into a
 *   destination page's own gesture the way `Post`/`Draft`/`Mail` do — there
 *   is no route arg for "open card X" / "open file Y" on either destination
 *   page today, so this navigates to a NEW path-segment route
 *   (`contacts/card/{uidHash}`, `media/file/{folderId}/{pathHash}` —
 *   `FaunaNavHost.kt`) that `ContactsVM`/`MediaVM` read via `SavedStateHandle`
 *   (mirrors `profile/{actorId}` → `ProfileVM`) and resolve asynchronously on
 *   entry, exactly as `PostDetailScreen` now drives `resolve_post` for a post
 *   the timeline never loaded. A resolve that finds nothing (deleted since it
 *   was indexed) is the same DROPPED outcome search.md documents elsewhere —
 *   the page still opens, just with nothing pre-selected.
 */
internal fun openSearchResult(
    nav: SearchNav,
    navController: NavController,
    conversationsManager: ConversationsManager,
): (() -> Unit)? = when (nav) {
    is SearchNav.Post -> {
        { navController.navigate("feed/post/${URLEncoder.encode(nav.postId, "UTF-8")}?source=") }
    }
    is SearchNav.Draft -> {
        {
            val threadId = nav.threadId
            if (threadId != null) {
                conversationsManager.selectThread(threadId)
                navController.navigate("conversation/$threadId")
            } else {
                conversationsManager.startNewConversation()
                navController.navigate("conversation_compose")
            }
        }
    }
    is SearchNav.Mail -> {
        {
            conversationsManager.selectThreadAndMessage(nav.threadId, nav.messageId)
            navController.navigate("conversation/${nav.threadId}")
        }
    }
    is SearchNav.Contact -> {
        { navController.navigate("contacts/card/${URLEncoder.encode(nav.uidHash, "UTF-8")}") }
    }
    is SearchNav.File -> {
        {
            navController.navigate(
                "media/file/${nav.folderId}/${URLEncoder.encode(nav.pathHash, "UTF-8")}"
            )
        }
    }
}

/** Icon for a result row's raw `content_type`, mirroring the shared kind-class
 *  prefixes (`libs/fauna-client-search/src/kind.rs::kind_class`) rather than
 *  the old per-app literal set — the old `"conversation"`/`"group_message"`/
 *  `"feed_post"`/`"event"` values never matched a real nest `content_type` at
 *  all, so every row rendered the fallback icon. */
private fun contentTypeIcon(contentType: String): ImageVector = when {
    contentType == "post" || contentType.startsWith("post/") -> Icons.Default.Star
    contentType == "profile" -> Icons.Default.Person
    contentType == "mail" || contentType.startsWith("imap") || contentType.startsWith("email") ->
        Icons.Default.Email
    contentType.startsWith("calendar") -> Icons.Default.DateRange
    contentType == "conversation" -> Icons.Default.Face
    else -> Icons.Default.Search
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SearchResultsScreen(
    navController: NavController,
    query: String,
    vm: SearchVM = hiltViewModel()
) {
    val snapshot by vm.snapshot.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current
    val conversationsManager = vm.conversationsManager

    val typeFilterOptions = remember { com.fauna.ffi.searchTypeFilterOptions() }
    val typeFilter = snapshot?.typeFilter ?: typeFilterOptions.firstOrNull() ?: "all"
    val errorText = resolveLocalized(context, snapshot?.error)

    LaunchedEffect(query) { vm.start(query) }
    LaunchedEffect(errorText) { appMessages.showError(errorText) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(query, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.testTag(Ids.PAGE_HEADING)) },
                navigationIcon = {
                    IconButton(onClick = { navController.popBackStack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.common_back))
                    }
                }
            )
        }
    ) { padding ->
        Column(modifier = Modifier.padding(padding).fillMaxSize()) {
            // Filter chips — the shared `search-type-filter` token set
            // (search.md § State & data shape), never a per-app literal list.
            // Selecting one re-fires the last query under the new token
            // through the manager (both backends), not a client-side
            // post-filter of already-fetched rows.
            LazyRow(
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                items(typeFilterOptions) { token ->
                    FilterChip(
                        selected = token == typeFilter,
                        onClick = { vm.setTypeFilter(token) },
                        label = { Text(typeFilterLabel(token)) },
                        modifier = Modifier.testTag(Ids.SEARCH_TYPE_FILTER)
                    )
                }
            }

            Box(modifier = Modifier.fillMaxSize()) {
                val snap = snapshot
                when {
                    // Not searched yet (query blank) — a transient window
                    // before the first snapshot lands, since this screen
                    // fires `start(query)` immediately. Render nothing, never
                    // "no results" (mirrors tui's `if snap.searched() { … }`
                    // gate — search.md § State & data shape).
                    snap == null || snap.query.isBlank() -> {}
                    // `no_results` and a non-empty `results` are mutually
                    // exclusive by construction (the manager sets
                    // `no_results = results.is_empty()` only once BOTH arms
                    // settle), so checking these two first — before
                    // `in_flight` — means a load-more or a filter re-fire
                    // keeps the previous page on screen instead of blanking
                    // it out from under the user while the new page loads.
                    snap.noResults -> {
                        Column(
                            modifier = Modifier.align(Alignment.Center).testTag(Ids.SEARCH_NO_RESULTS),
                            horizontalAlignment = Alignment.CenterHorizontally
                        ) {
                            Icon(Icons.Default.Search, contentDescription = null,
                                modifier = Modifier.size(48.dp))
                            Spacer(modifier = Modifier.height(8.dp))
                            Text(stringResource(R.string.search_page_no_results) + " '${snap.query}'")
                        }
                    }
                    snap.results.isEmpty() -> {
                        // The first fire for this query is still in flight and
                        // has not settled either way yet.
                        CircularProgressIndicator(modifier = Modifier.align(Alignment.Center))
                    }
                    else -> {
                        LazyColumn(modifier = Modifier.fillMaxSize().testTag(Ids.SEARCH_RESULTS_VIEW)) {
                            items(snap.results) { row: SearchResultRow ->
                                val badgeLabel = resolveLocalized(context, row.badge) ?: row.contentType
                                // row.navigation carries a typed target (SearchNav) — activating
                                // the row routes into the destination page's own gesture
                                // (search.md § User actions; openSearchResult above). Every
                                // variant is wired now; a row with no target (`row.navigation ==
                                // null`) still stays non-clickable — a row whose target has no
                                // destination yet renders inert, per ui.yaml.
                                val onOpen = row.navigation?.let {
                                    openSearchResult(it, navController, conversationsManager)
                                }
                                ListItem(
                                    overlineContent = {
                                        Text(badgeLabel, style = MaterialTheme.typography.labelSmall)
                                    },
                                    leadingContent = {
                                        Icon(contentTypeIcon(row.contentType), contentDescription = row.contentType)
                                    },
                                    headlineContent = {
                                        Text(row.snippet, maxLines = 2, overflow = TextOverflow.Ellipsis)
                                    },
                                    trailingContent = {
                                        // row.timestamp is already normalised to epoch millis by
                                        // the shared manager (SearchResultRow doc) — no unit
                                        // conversion here, unlike the pre-adoption raw wire reply.
                                        Text(ValueFormat.relativeTime(context, row.timestamp),
                                            style = MaterialTheme.typography.labelSmall)
                                    },
                                    modifier = Modifier
                                        .let { m -> if (onOpen != null) m.clickable(onClick = onOpen) else m }
                                        .testTag(Ids.SEARCH_RESULT_ITEM)
                                )
                                HorizontalDivider()
                            }

                            if (snap.hasMore) {
                                item {
                                    Button(
                                        onClick = { vm.loadMore() },
                                        modifier = Modifier
                                            .fillMaxWidth()
                                            .padding(horizontal = 16.dp, vertical = 8.dp)
                                            .testTag(Ids.SEARCH_LOAD_MORE_BUTTON),
                                    ) {
                                        Text(stringResource(R.string.common_load_more))
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/** i18n label for a `search-type-filter` token — the shared
 *  `fauna_client_search::type_filter_label` over UniFFI
 *  (`com.fauna.ffi.searchTypeFilterLabel`), the same map tui and linux paint
 *  from, reusing the `search_page` badge keys the result-row badge renders
 *  with, so an option and the rows it selects can never read differently.
 *  Deliberately NOT a local match: every app hand-rolled this as a *closed*
 *  match with an `_ -> all` fallback, so a token added to the shared option
 *  list rendered a second option reading "All" — and windows' copy had already
 *  drifted onto a different key set, labelling `profile` "Contacts". An
 *  unrecognised token surfaces raw instead: the shared map passes it through,
 *  and [resolveLocalized] returns the key when no resource matches.
 *  See `docs/goal/ui/search.md` § State & data shape -> Type filter. */
@Composable
private fun typeFilterLabel(token: String): String =
    resolveLocalized(LocalContext.current, com.fauna.ffi.searchTypeFilterLabel(token)) ?: ""
