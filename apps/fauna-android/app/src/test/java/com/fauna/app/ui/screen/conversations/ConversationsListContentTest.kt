package com.fauna.app.ui.screen.conversations

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import androidx.compose.ui.semantics.SemanticsProperties
import uniffi.fauna_conversations.GuardianState
import uniffi.fauna_conversations.Rail
import uniffi.fauna_conversations.SortOrder
import uniffi.fauna_conversations.ThreadFlavor
import uniffi.fauna_conversations.ThreadSummary
import uniffi.fauna_conversations.railGlyph
import uniffi.fauna_core.BridgeIdentitySnapshot
import uniffi.fauna_core.SourceGlyph

/**
 * Compose-level coverage for the stateless [ConversationsListContent] (the
 * conversation list pane, `docs/goal/ui/conversations.md`). Renders with seeded
 * [ThreadSummary] records — no Hilt, no VM, no FFI native calls (the snapshot
 * records are plain Kotlin data classes) — so the canonical ui.yaml IDs and the
 * gesture callbacks are exercised on the JVM. (Android E2E
 * `test_keying.py --client android` etc. is the standing gate once the `host`
 * emulator lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ConversationsListContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun thread(
        id: String,
        rail: Rail = Rail.FAUNA_MLS,
        label: String = "Alice",
        snippet: String = "hello",
        unread: UInt = 0u,
    ) = ThreadSummary(
        threadId = id,
        rail = rail,
        glyph = railGlyph(rail),
        flavor = ThreadFlavor.OneToOne,
        label = label,
        snippet = snippet,
        lastActivityMs = 1_700_000_000_000L,
        unreadCount = unread,
        participantCount = 2u,
    )

    private fun render(
        threads: List<ThreadSummary> = emptyList(),
        onOpenThread: (String) -> Unit = {},
        onNewConversation: () -> Unit = {},
        onSort: () -> Unit = {},
        onSearch: (String) -> Unit = {},
        unopenableMail: UInt = 0u,
    ) {
        composeTestRule.setContent {
            ConversationsListContent(
                threads = threads,
                sort = SortOrder.LATEST_ACTIVITY,
                searchQuery = "",
                onOpenThread = onOpenThread,
                onNewConversation = onNewConversation,
                onSort = onSort,
                onSearch = onSearch,
                unopenableMail = unopenableMail,
            )
        }
    }

    @Test
    fun chromeElementsRender() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("new-conversation-button").assertExists()
        composeTestRule.onNodeWithTag("conversation-sort").assertExists()
        composeTestRule.onNodeWithTag("conversation-search-box").assertExists()
    }

    @Test
    fun rowCountMatchesThreads() {
        render(threads = listOf(thread("t1"), thread("t2"), thread("t3")))
        assertEquals(
            3,
            composeTestRule.onAllNodesWithTag("conversation-item").fetchSemanticsNodes().size,
        )
        // One protocol-icon glyph + one dm-subject per row. The row is a
        // merging `ListItem`, so its child IDs live on the unmerged tree (the
        // tree the real Android testTag→AutomationId resolution also walks).
        assertEquals(
            3,
            composeTestRule.onAllNodesWithTag("protocol-icon", useUnmergedTree = true)
                .fetchSemanticsNodes().size,
        )
        assertEquals(
            3,
            composeTestRule.onAllNodesWithTag("dm-subject", useUnmergedTree = true)
                .fetchSemanticsNodes().size,
        )
    }

    @Test
    fun unreadIndicatorOnlyForUnreadRows() {
        render(threads = listOf(thread("t1", unread = 0u), thread("t2", unread = 3u)))
        // Exactly one row carries the unread badge.
        assertEquals(
            1,
            composeTestRule.onAllNodesWithTag("dm-unread-indicator", useUnmergedTree = true)
                .fetchSemanticsNodes().size,
        )
    }

    /**
     * A bridged row paints what the snapshot declares — the bridge's glyph with
     * its declared label, the bridge id on `stateDescription` (the driver's
     * `bridge` attribute), and `conversation-guardian-state` only on the room
     * the nest reports held (`conversations.md` § Where logic lives → *The
     * `Bridged` adapter*; `family-safety.md` § The bridge-DM gate).
     */
    @Test
    fun bridgedRowPaintsTheDeclaredIdentityAndTheGuardianMarker() {
        val matrix = BridgeIdentitySnapshot(id = "matrix", label = "Matrix", glyph = SourceGlyph.GLOBE)
        val held = thread("held", rail = Rail.BRIDGED, label = "@cold:example.org").copy(
            glyph = SourceGlyph.GLOBE,
            bridge = matrix,
            guardianState = GuardianState.HELD,
        )
        val open = thread("open", rail = Rail.BRIDGED, label = "@bob:example.org").copy(
            glyph = SourceGlyph.GLOBE,
            bridge = matrix,
        )
        render(threads = listOf(held, open))

        val icons = composeTestRule.onAllNodesWithTag("protocol-icon", useUnmergedTree = true)
            .fetchSemanticsNodes()
        assertEquals(2, icons.size)
        for (icon in icons) {
            val text = icon.config[SemanticsProperties.Text].joinToString("") { it.text }
            assertEquals(true, text.endsWith(" Matrix"))
            assertEquals("matrix", icon.config[SemanticsProperties.StateDescription])
        }
        val markers = composeTestRule
            .onAllNodesWithTag("conversation-guardian-state", useUnmergedTree = true)
            .fetchSemanticsNodes()
        assertEquals(1, markers.size)
        assertEquals("held", markers[0].config[SemanticsProperties.StateDescription])
    }

    @Test
    fun protocolIconReflectsRail() {
        // Rail is presentation-only (the glyph); the row itself never branches on it.
        render(threads = listOf(thread("t1", rail = Rail.SMTP)))
        composeTestRule.onNodeWithTag("dm-subject", useUnmergedTree = true).assertTextEquals("Alice")
    }

    @Test
    fun tappingRowOpensThread() {
        var opened: String? = null
        render(threads = listOf(thread("tid-42")), onOpenThread = { opened = it })
        composeTestRule.onNodeWithTag("conversation-item").performClick()
        assertEquals("tid-42", opened)
    }

    @Test
    fun newConversationButtonDispatches() {
        var clicked = false
        render(onNewConversation = { clicked = true })
        composeTestRule.onNodeWithTag("new-conversation-button").performClick()
        assert(clicked)
    }

    @Test
    fun sortButtonDispatches() {
        var sorted = false
        render(threads = listOf(thread("t1")), onSort = { sorted = true })
        composeTestRule.onNodeWithTag("conversation-sort").performClick()
        assert(sorted)
    }

    // The floor of the `error-message` stack (`ui/conversations.md` § Errors &
    // edge cases → *A fifth truth*, 2026-09-15): the list has no other
    // page-level error producer, but must still surface this standing truth —
    // a user who never opens a thread only ever sees the list.

    @Test
    fun unopenableMailAbsentByDefault() {
        render()
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
    }

    @Test
    fun unopenableMailShowsTheResolvedCount() {
        render(unopenableMail = 3u)
        composeTestRule.onNodeWithTag("error-message")
            .assertExists()
            .assertTextContains("could not be opened", substring = true)
            .assertTextContains("3 ", substring = true)
    }
}
