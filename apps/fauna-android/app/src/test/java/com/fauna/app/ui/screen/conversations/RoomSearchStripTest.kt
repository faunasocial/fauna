package com.fauna.app.ui.screen.conversations

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_conversations.RoomSearchNav
import uniffi.fauna_conversations.RoomSearchResultKind
import uniffi.fauna_conversations.RoomSearchResultSnapshot
import uniffi.fauna_conversations.RoomSearchSnapshot
import uniffi.fauna_conversations.RoomSearchStatus

/**
 * The community room's search strip and its rows (`docs/goal/behavior/room-search.md`
 * § The surface), rendered from seeded snapshot records — the records are plain
 * Kotlin data classes, so no VM, no manager and no FFI call is needed. What is
 * pinned is the paint and the gestures: the field submits the typed query, the
 * rows paint in the served order with their `kind`, the empty state, the in-flight
 * and failed states painting no result region, and a row handing back its typed
 * target. The query, the leg and the words are the shared manager's (Rust).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class RoomSearchStripTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val messageNav = RoomSearchNav.Message(threadId = "t1", messageId = "m1")
    private val postNav = RoomSearchNav.Post(postId = "p1", homeNestUrl = "https://home.example")

    private fun row(kind: RoomSearchResultKind, text: String, nav: RoomSearchNav) =
        RoomSearchResultSnapshot(
            kind = kind,
            senderDisplay = "ada",
            sentAtMs = null,
            text = text,
            pending = false,
            navigation = nav,
        )

    private fun search(status: RoomSearchStatus, vararg rows: RoomSearchResultSnapshot) =
        RoomSearchSnapshot(query = "whiskers", status = status, results = rows.toList())

    @Test
    fun theFieldSubmitsTheTypedQueryOnTheSubmitButton() {
        val submitted = mutableListOf<String>()
        composeTestRule.setContent { RoomSearchStrip(onSubmit = { submitted += it }) }

        composeTestRule.onNodeWithTag(Ids.ROOM_SEARCH_FIELD).performTextInput("whiskers")
        // Typing alone asks nothing — the strip is submit-driven.
        assertTrue(submitted.isEmpty())
        composeTestRule.onNodeWithTag(Ids.ROOM_SEARCH_SUBMIT_BUTTON).performClick()

        assertEquals(listOf("whiskers"), submitted)
    }

    @Test
    fun hitsPaintInTheServedOrderWithTheirKindAndHandBackTheirTarget() {
        val opened = mutableListOf<RoomSearchNav>()
        composeTestRule.setContent {
            RoomSearchResults(
                search = search(
                    RoomSearchStatus.DONE,
                    row(RoomSearchResultKind.POST, "a post teaser", postNav),
                    row(RoomSearchResultKind.MESSAGE, "a message line", messageNav),
                ),
                onOpenHit = { opened += it },
            )
        }

        val rows = composeTestRule.onAllNodesWithTag(Ids.ROOM_SEARCH_RESULT_ITEM)
        rows.assertCountEquals(2)
        assertEquals(
            "post",
            rows[0].fetchSemanticsNode().config[SemanticsProperties.StateDescription],
        )
        assertEquals(
            "message",
            rows[1].fetchSemanticsNode().config[SemanticsProperties.StateDescription],
        )
        rows[0].performClick()
        rows[1].performClick()

        assertEquals(listOf<RoomSearchNav>(postNav, messageNav), opened)
        composeTestRule.onAllNodesWithTag(Ids.ROOM_SEARCH_NO_RESULTS).assertCountEquals(0)
    }

    @Test
    fun anAnsweredQueryWithNoHitsPaintsTheEmptyState() {
        composeTestRule.setContent {
            RoomSearchResults(search = search(RoomSearchStatus.DONE), onOpenHit = {})
        }

        composeTestRule.onNodeWithTag(Ids.ROOM_SEARCH_NO_RESULTS).assertIsDisplayed()
        composeTestRule.onAllNodesWithTag(Ids.ROOM_SEARCH_RESULT_ITEM).assertCountEquals(0)
    }

    private fun assertNoResultRegionWhen(status: RoomSearchStatus) {
        composeTestRule.setContent {
            RoomSearchResults(
                search = search(status, row(RoomSearchResultKind.MESSAGE, "stale", messageNav)),
                onOpenHit = {},
            )
        }
        composeTestRule.onAllNodesWithTag(Ids.ROOM_SEARCH_NO_RESULTS).assertCountEquals(0)
        composeTestRule.onAllNodesWithTag(Ids.ROOM_SEARCH_RESULT_ITEM).assertCountEquals(0)
    }

    @Test
    fun aQueryInFlightPaintsNoResultRegion() = assertNoResultRegionWhen(RoomSearchStatus.SEARCHING)

    @Test
    fun aFailedQueryPaintsNoResultRegion() = assertNoResultRegionWhen(RoomSearchStatus.FAILED)

    @Test
    fun aSubmittedQueryReplacesTheStreamAndAnOpenIdleStripDoesNot() {
        assertFalse(roomSearchReplacesStream(null))
        assertFalse(roomSearchReplacesStream(search(RoomSearchStatus.IDLE)))
        assertTrue(roomSearchReplacesStream(search(RoomSearchStatus.SEARCHING)))
        assertTrue(roomSearchReplacesStream(search(RoomSearchStatus.DONE)))
        assertTrue(roomSearchReplacesStream(search(RoomSearchStatus.FAILED)))
    }

    @Test
    fun aRowReadsSenderTimeAndFirstLineJoinedByAMiddleDot() {
        val full = row(RoomSearchResultKind.MESSAGE, "hello", messageNav).copy(sentAtMs = 5L)
        assertEquals("ada · t5 · hello", roomSearchRowText(full) { "t$it" })

        val pending = RoomSearchResultSnapshot(
            kind = RoomSearchResultKind.MESSAGE,
            senderDisplay = null,
            sentAtMs = null,
            text = "Not on this device yet",
            pending = true,
            navigation = messageNav,
        )
        assertEquals("Not on this device yet", roomSearchRowText(pending) { "t$it" })
    }
}
