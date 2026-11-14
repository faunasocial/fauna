package com.fauna.app.ui.screen.events

import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_core.RsvpResponse
import java.time.LocalDate

/**
 * The agenda card's inline RSVP trio and Agenda view's inert pan header
 * (`docs/goal/ui/events.md` § Where logic lives → *View mode + visible range*,
 * § Implementation status today → the inline card-level RSVP row).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AgendaCardPartsTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    /** Each button submits for the card's OWN event, with its own response. */
    @Test
    fun cardTrioSubmitsForItsOwnEvent() {
        val sent = mutableListOf<Pair<String, RsvpResponse>>()
        composeTestRule.setContent {
            EventCardRsvpButtons(eventId = "ev-42", onRsvp = { id, r -> sent += id to r })
        }
        composeTestRule.onNodeWithTag(Ids.EVENT_RSVP_GOING).performClick()
        composeTestRule.onNodeWithTag(Ids.EVENT_RSVP_INTERESTED).performClick()
        composeTestRule.onNodeWithTag(Ids.EVENT_RSVP_DECLINE).performClick()
        assertEquals(
            listOf(
                "ev-42" to RsvpResponse.GOING,
                "ev-42" to RsvpResponse.INTERESTED,
                "ev-42" to RsvpResponse.DECLINED,
            ),
            sent,
        )
    }

    /**
     * Both agenda pan buttons are painted and both hand back the anchor
     * unchanged — asserted on the date itself, the finest-grained render of
     * the state, so a pan of any size would fail it.
     */
    @Test
    fun agendaPanLeavesTheAnchorUnchanged() {
        val anchor = LocalDate.of(2026, 8, 22)
        val moves = mutableListOf<LocalDate>()
        composeTestRule.setContent {
            AgendaPanHeader(date = anchor, onDateChange = { moves += it })
        }
        composeTestRule.onNodeWithTag(Ids.CALENDAR_DATE_LABEL).assertTextEquals("Upcoming")
        composeTestRule.onNodeWithTag(Ids.EVENTS_NEXT_MONTH).performClick()
        composeTestRule.onNodeWithTag(Ids.EVENTS_PREV_MONTH).performClick()
        assertEquals(listOf(anchor, anchor), moves)
    }
}
