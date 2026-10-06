package com.fauna.app.ui.events

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.data.api.EventSummary
import com.fauna.app.ui.screen.events.DayTimelineContent
import com.fauna.app.ui.screen.events.MonthGridContent
import com.fauna.app.ui.screen.events.WeekGridContent
import com.fauna.ffi.FfiDayEventPlacement
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.LocalDate

@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class CalendarViewTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val testEvents = listOf(
        EventSummary(
            id = "evt1", uid = "uid1", summary = "Team Meeting",
            dtstart = "${LocalDate.now()}T10:00:00", dtend = "${LocalDate.now()}T11:00:00"
        ),
        EventSummary(
            id = "evt2", uid = "uid2", summary = "Lunch",
            dtstart = "${LocalDate.now()}T12:00:00", dtend = "${LocalDate.now()}T13:00:00"
        )
    )

    // ── MonthGridContent ─────────────────────────────────────────────

    @Test
    fun monthGrid_showsMonthHeader() {
        val today = LocalDate.now()
        composeTestRule.setContent {
            MonthGridContent(
                date = today,
                events = emptyList(),
                onDateChange = {},
                onDayClick = {}
            )
        }
        // Should show month name
        composeTestRule.onNode(hasText(today.month.name, substring = true, ignoreCase = true))
            .assertIsDisplayed()
    }

    @Test
    fun monthGrid_showsDayHeaders() {
        composeTestRule.setContent {
            MonthGridContent(
                date = LocalDate.now(),
                events = emptyList(),
                onDateChange = {},
                onDayClick = {}
            )
        }
        composeTestRule.onNodeWithText("Mon").assertIsDisplayed()
        composeTestRule.onNodeWithText("Tue").assertIsDisplayed()
        composeTestRule.onNodeWithText("Wed").assertIsDisplayed()
        composeTestRule.onNodeWithText("Thu").assertIsDisplayed()
        composeTestRule.onNodeWithText("Fri").assertIsDisplayed()
    }

    @Test
    fun monthGrid_todayIsHighlighted() {
        val today = LocalDate.now()
        composeTestRule.setContent {
            MonthGridContent(
                date = today,
                events = emptyList(),
                onDateChange = {},
                onDayClick = {}
            )
        }
        // Today's date number should be visible. On single-digit days the 42-cell
        // grid renders the same number twice (this month's cell + the next month's
        // trailing cell), so match all and assert the first — composition is
        // chronological, so the current-month cell (always in the top rows) comes
        // before the next-month duplicate. Avoids date-dependent "found 2 nodes".
        composeTestRule.onAllNodesWithText("${today.dayOfMonth}").onFirst().assertIsDisplayed()
    }

    @Test
    fun monthGrid_dayClick_callsCallback() {
        var clickedDate: LocalDate? = null
        composeTestRule.setContent {
            MonthGridContent(
                date = LocalDate.now(),
                events = emptyList(),
                onDateChange = {},
                onDayClick = { clickedDate = it }
            )
        }
        // Click on day 15 of current month
        composeTestRule.onNodeWithText("15").performClick()
        composeTestRule.waitForIdle()
        assert(clickedDate != null) { "Day click callback should have been called" }
        assert(clickedDate!!.dayOfMonth == 15) { "Clicked day should be 15" }
    }

    // ── DayTimelineContent ───────────────────────────────────────────

    @Test
    fun dayTimeline_showsDateHeader() {
        val today = LocalDate.now()
        composeTestRule.setContent {
            DayTimelineContent(
                date = today,
                events = testEvents,
                onDateChange = {},
                onEventClick = {},
                onSlotClick = {}
            )
        }
        // Day name should be visible in header
        composeTestRule.onNode(
            hasText(today.dayOfWeek.name.lowercase().replaceFirstChar { it.uppercase() }, substring = true)
        ).assertIsDisplayed()
    }

    @Test
    fun dayTimeline_showsEvents() {
        composeTestRule.setContent {
            DayTimelineContent(
                date = LocalDate.now(),
                events = testEvents,
                onDateChange = {},
                onEventClick = {},
                onSlotClick = {}
            )
        }
        // TimelineColumn positions event cards absolutely in a 24h-tall scrolling
        // column and auto-scrolls to the current hour, so the 10:00/12:00 cards sit
        // off-screen at some times of day. Scroll each into view before asserting,
        // so the test is green regardless of the wall-clock hour it runs at.
        composeTestRule.onNodeWithText("Team Meeting").performScrollTo().assertIsDisplayed()
        composeTestRule.onNodeWithText("Lunch").performScrollTo().assertIsDisplayed()
    }

    @Test
    fun dayTimeline_navigation_callsCallback() {
        var newDate: LocalDate? = null
        val today = LocalDate.now()
        composeTestRule.setContent {
            DayTimelineContent(
                date = today,
                events = emptyList(),
                onDateChange = { newDate = it },
                onEventClick = {},
                onSlotClick = {}
            )
        }
        // Click next day arrow
        composeTestRule.onNodeWithContentDescription("Next day").performClick()
        composeTestRule.waitForIdle()
        assert(newDate == today.plusDays(1)) { "Should navigate to next day" }
    }

    // ── WeekGridContent ──────────────────────────────────────────────

    @Test
    fun weekGrid_showsWeekHeader() {
        composeTestRule.setContent {
            WeekGridContent(
                date = LocalDate.now(),
                events = emptyList(),
                onDateChange = {},
                onEventClick = {},
                onSlotClick = {}
            )
        }
        // Should show abbreviated day names
        composeTestRule.onNodeWithText("Mon").assertIsDisplayed()
        composeTestRule.onNodeWithText("Sun").assertIsDisplayed()
    }

    @Test
    fun weekGrid_rendersAllDayBand() {
        composeTestRule.setContent {
            WeekGridContent(
                date = LocalDate.now(),
                events = emptyList(),
                onDateChange = {},
                onEventClick = {},
                onSlotClick = {},
                dayColumnLayout = { emptyList() },
            )
        }
        composeTestRule.onNodeWithTag("calendar-allday-band", useUnmergedTree = true).assertExists()
    }

    @Test
    fun weekGrid_rendersTimedEventBlocksViaSharedRenderer() {
        // A timed event on a day of the displayed week lays out as a packed
        // calendar-event-block (the shared DayEventColumn), layout injected FFI-free.
        val monday = LocalDate.now()
            .with(java.time.temporal.TemporalAdjusters.previousOrSame(java.time.DayOfWeek.MONDAY))
        val events = listOf(
            EventSummary(id = "w1", uid = "w1", summary = "WeekEvt",
                dtstart = "${monday}T09:00:00", dtend = "${monday}T10:00:00"),
        )
        composeTestRule.setContent {
            WeekGridContent(
                date = monday,
                events = events,
                onDateChange = {},
                onEventClick = {},
                onSlotClick = {},
                dayColumnLayout = { dayEvents ->
                    dayEvents.map {
                        FfiDayEventPlacement(
                            allDay = false, startMin = 540u, endMin = 600u, columnIndex = 0u, totalColumns = 1u,
                        )
                    }
                },
            )
        }
        composeTestRule.onAllNodesWithTag("calendar-event-block", useUnmergedTree = true)
            .assertCountEquals(1)
    }
}
