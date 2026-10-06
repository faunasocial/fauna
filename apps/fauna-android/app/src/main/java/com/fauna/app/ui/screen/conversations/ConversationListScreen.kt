package com.fauna.app.ui.screen.conversations

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Create
import androidx.compose.material.icons.filled.Email
import androidx.compose.material.icons.filled.Sort
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.sourceGlyphEmoji
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.ConversationsVM
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_conversations.GuardianState
import uniffi.fauna_conversations.SortOrder
import uniffi.fauna_conversations.ThreadSummary
import uniffi.fauna_conversations.guardianStateAttrToken
import uniffi.fauna_conversations.guardianStateLabel
import uniffi.fauna_conversations.nextSortOrder
import social.fauna.generated.Ids

/**
 * Conversation list pane (mobile-collapsed list screen) — observer-driven
 * render of the shared [uniffi.fauna_conversations.ConversationsManager]
 * snapshot, mirroring the Linux/Windows precedent
 * (apps/fauna-linux/src/views/conversations/list.rs).
 *
 * Per docs/goal/ui/conversations.md §"Architectural rules" #1 (observer-driven,
 * no client-side state machine) + #5 (capability-gating, never rail branches —
 * `rail` is used only for the presentational protocol-icon glyph). The VM-bound
 * [ConversationListScreen] is the wrapper the NavHost mounts; the stateless
 * [ConversationsListContent] is split out for the Compose test harness (no VM,
 * no manager, no FFI — Robolectric can't load the `.so`).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationListScreen(
    navController: NavController,
    vm: ConversationsVM = hiltViewModel(),
) {
    val snapshot by vm.managerSnapshot.collectAsState()
    val manager = vm.conversationsManager
    // ValueFormat.conversationTimestamp is an FFI call (shared fauna_core::format)
    // — keep it in the stateful Screen so the Content stays JVM/Robolectric-testable.
    val context = LocalContext.current

    ConversationsListContent(
        threads = snapshot?.threads ?: emptyList(),
        sort = snapshot?.sort ?: SortOrder.LATEST_ACTIVITY,
        searchQuery = snapshot?.searchQuery ?: "",
        // The floor of the `error-message` stack (`ui/conversations.md` §
        // Errors & edge cases → *A fifth truth*): received mail this run that
        // could not open under the account's key set. The mobile-collapsed
        // list has no other page-level error producer today (unlike the
        // detail screen), so this is the list's only truth, not a precedence
        // chain — but it must still surface here, since a user who never
        // opens a thread (this app's e2e included) only ever sees the list.
        // Same FFI-fault-degrades-to-0 posture as detail's read.
        unopenableMail = runCatching { manager.unopenableMailCount() }.getOrDefault(0u),
        // Contextual conversation timestamp (today → clock, Yesterday/weekday →
        // localized, older → date) — the shared fauna_core::format bucket, not the
        // generic relative-time elapsed form (value-formatting.md § Conversation timestamp).
        formatTime = { ms -> ValueFormat.conversationTimestamp(context, ms) },
        onOpenThread = { threadId ->
            // Select the thread on the shared manager, then push the detail
            // screen (mobile collapse — conversations.md §"Mobile collapse").
            manager.selectThread(threadId)
            navController.navigate("conversation/$threadId")
        },
        onNewConversation = {
            // Open the in-pane new-thread compose (conversations.md §"New-thread
            // compose lives in the detail pane, not a modal"). On mobile the
            // detail pane is a separate screen, so seed the shared compose state
            // and navigate to the new-thread compose screen.
            manager.startNewConversation()
            navController.navigate("conversation_compose")
        },
        // SortOrder cycles latest-activity → oldest-first → unread → … so a
        // single sort affordance covers all three the snapshot exposes. The
        // shared `fauna_conversations::snapshot::next_sort_order` owns the
        // cycle (linux/web/apple/windows precedent) — android used to
        // hand-roll the same cycle locally.
        onSort = { manager.setSort(nextSortOrder(current = snapshot?.sort ?: SortOrder.LATEST_ACTIVITY)) },
        onSearch = { manager.setSearchQuery(it.ifEmpty { null }) },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationsListContent(
    threads: List<ThreadSummary>,
    sort: SortOrder,
    searchQuery: String,
    onOpenThread: (String) -> Unit,
    onNewConversation: () -> Unit,
    onSort: () -> Unit,
    onSearch: (String) -> Unit,
    // Last-activity timestamp formatter — injected so the Content stays FFI-free
    // for Robolectric (ValueFormat.conversationTimestamp is a shared-Rust FFI
    // call). The VM wrapper supplies the real one; tests pass a pure stub.
    formatTime: (Long) -> String = { "" },
    // The floor of the `error-message` stack (`ui/conversations.md` § Errors &
    // edge cases → *A fifth truth*) — see the call site's comment for why the
    // list screen carries this at all. `0u` keeps the Content/test harness
    // FFI-free, matching every other injected default here.
    unopenableMail: UInt = 0u,
) {
    Scaffold(
        modifier = Modifier.testTag(Ids.CONVERSATIONS_TAB),
        topBar = {
            Column {
                TopAppBar(
                    title = {
                        Text(
                            stringResource(R.string.conversations_list_title),
                            modifier = Modifier.testTag(Ids.PAGE_HEADING),
                        )
                    },
                )
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = searchQuery,
                        onValueChange = onSearch,
                        modifier = Modifier
                            .weight(1f)
                            .padding(horizontal = 16.dp, vertical = 4.dp)
                            .testTag(Ids.CONVERSATION_SEARCH_BOX),
                        placeholder = { Text(stringResource(R.string.conversations_list_search_placeholder)) },
                        singleLine = true,
                    )
                    IconButton(
                        onClick = onSort,
                        modifier = Modifier.testTag(Ids.CONVERSATION_SORT),
                    ) {
                        Icon(Icons.Default.Sort, contentDescription = stringResource(R.string.common_sort))
                    }
                }
            }
        },
        floatingActionButton = {
            FloatingActionButton(
                onClick = onNewConversation,
                modifier = Modifier.testTag(Ids.NEW_CONVERSATION_BUTTON),
            ) {
                Icon(Icons.Default.Create, stringResource(R.string.conversations_list_new_conversation))
            }
        },
    ) { padding ->
        Column(modifier = Modifier.padding(padding).fillMaxSize()) {
            if (unopenableMail > 0u) {
                Text(
                    localized(
                        LocalizedText(
                            key = "conversations.errors.mail_unopenable",
                            args = mapOf("count" to unopenableMail.toString()),
                        ),
                    ).orEmpty(),
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 4.dp)
                        .testTag(Ids.ERROR_MESSAGE),
                )
            }
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth(),
            ) {
                if (threads.isEmpty()) {
                    Column(
                        modifier = Modifier.align(Alignment.Center),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Icon(
                            Icons.Default.Email,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.size(48.dp),
                        )
                        Spacer(modifier = Modifier.height(8.dp))
                        Text(
                            stringResource(R.string.conversations_list_no_conversations),
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                } else {
                    LazyColumn(modifier = Modifier.fillMaxSize()) {
                        items(threads, key = { it.threadId }) { thread ->
                            ThreadRow(
                                thread = thread,
                                formatTime = formatTime,
                                onClick = { onOpenThread(thread.threadId) },
                            )
                            HorizontalDivider()
                        }
                    }
                }
            }
        }
    }
}

/**
 * `conversation-guardian-state` — the family gate's marker, the same element on
 * the list row and in the thread header: the shared localized text, the wire
 * word on `stateDescription` (the driver's `state` attribute).
 */
@Composable
internal fun GuardianStateMarker(state: GuardianState) {
    Text(
        guardianStateLabel(state),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.error,
        modifier = Modifier
            .padding(end = 8.dp)
            .testTag(Ids.CONVERSATION_GUARDIAN_STATE)
            .semantics { stateDescription = guardianStateAttrToken(state) },
    )
}

@Composable
private fun ThreadRow(thread: ThreadSummary, formatTime: (Long) -> String, onClick: () -> Unit) {
    val unread = thread.unreadCount > 0u
    ListItem(
        headlineContent = {
            Text(
                localized(com.fauna.ffi.threadLabelDisplay(thread.label)) ?: thread.label,
                fontWeight = if (unread) FontWeight.Bold else FontWeight.Normal,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.testTag(Ids.DM_SUBJECT),
            )
        },
        supportingContent = {
            Text(thread.snippet, maxLines = 1, overflow = TextOverflow.Ellipsis)
        },
        leadingContent = if (unread) {
            { Badge(modifier = Modifier.testTag(Ids.DM_UNREAD_INDICATOR)) }
        } else {
            null
        },
        trailingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                // The family gate's marker, only while the nest reports one for
                // this room's peer (`family-safety.md` § The bridge-DM gate →
                // *App affordance*): computed there, painted here, and the row
                // still opens.
                thread.guardianState?.let { GuardianStateMarker(it) }
                // The glyph is the snapshot's — on a bridged room the one its
                // bridge declared, with the bridge's declared label beside it so
                // two bridges sharing a glyph still read apart
                // (`conversations.md` § Where logic lives → *The `Bridged`
                // adapter*, ruling 2 (a)). No app-side mapping; the bridge id
                // rides `stateDescription` (the driver's `bridge` attribute).
                val bridge = thread.bridge
                val glyph = sourceGlyphEmoji(thread.glyph)
                Text(
                    if (bridge != null) "$glyph ${bridge.label}" else glyph,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier
                        .padding(end = 8.dp)
                        .testTag(Ids.PROTOCOL_ICON)
                        .then(
                            if (bridge != null) {
                                Modifier.semantics { stateDescription = bridge.id }
                            } else {
                                Modifier
                            },
                        ),
                )
                Text(
                    formatTime(thread.lastActivityMs),
                    style = MaterialTheme.typography.labelSmall,
                )
            }
        },
        modifier = Modifier
            .clickable(onClick = onClick)
            .testTag(Ids.CONVERSATION_ITEM),
    )
}
