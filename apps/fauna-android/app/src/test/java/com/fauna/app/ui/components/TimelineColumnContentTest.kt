package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.unit.dp
import com.fauna.app.data.api.EventSummary
import com.fauna.ffi.FfiDayEvent
import com.fauna.ffi.FfiDayEventPlacement
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import java.time.LocalTime
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the day-column renderer [TimelineColumn]
 * (events.md § Week & day timeline views). All-day/timed classification, timed
 * minute geometry, and overlap column-packing are ONE shared-Rust call now
 * (`fauna_core::caltime::day_column_layout`, unit-tested there) reached via the
 * UniFFI `dayColumnLayout`; it is **injected** here (the `dayColumnLayout`
 * param) so the test seeds deterministic placements **without** a native call
 * (mirrors `AttendeeRowContentTest`, which injects `attendee_display`'s
 * `view`). The test asserts the column renderer hands the shared fn the raw
 * event strings unmodified and wires the returned placements to the right
 * block count / geometry, never re-deriving classification or minute math
 * itself.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TimelineColumnContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun ev(id: String, start: String, end: String?) =
        EventSummary(id = id, uid = id, summary = "E$id", dtstart = start, dtend = end)

    private fun timedPlacement(startMin: Int, endMin: Int, columnIndex: Int, totalColumns: Int) =
        FfiDayEventPlacement(
            allDay = false,
            startMin = startMin.toUInt(),
            endMin = endMin.toUInt(),
            columnIndex = columnIndex.toUInt(),
            totalColumns = totalColumns.toUInt(),
        )

    @Test
    fun feedsRawEventStringsToTheSharedDayColumnLayoutFn() {
        // Two overlapping events: 09:00–10:00 and 09:30–10:30. The column hands
        // the shared `day_column_layout` the raw (start, end) strings unmodified
        // — classification, minute geometry, and column packing are entirely its
        // job; the column only maps the returned placements to block geometry.
        val events = listOf(
            ev("1", "2026-06-25T09:00:00", "2026-06-25T10:00:00"),
            ev("2", "2026-06-25T09:30:00", "2026-06-25T10:30:00"),
        )
        var captured: List<FfiDayEvent> = emptyList()
        composeTestRule.setContent {
            Box(Modifier.size(width = 400.dp, height = 1440.dp)) {
                TimelineColumn(
                    events = events,
                    onEventClick = {},
                    onSlotClick = {},
                    dayColumnLayout = { dayEvents ->
                        captured = dayEvents
                        listOf(
                            timedPlacement(startMin = 540, endMin = 600, columnIndex = 0, totalColumns = 2),
                            timedPlacement(startMin = 570, endMin = 630, columnIndex = 1, totalColumns = 2),
                        )
                    },
                )
            }
        }
        assertEquals(2, captured.size)
        assertEquals("2026-06-25T09:00:00", captured[0].start)
        assertEquals("2026-06-25T10:00:00", captured[0].end)
        assertEquals("2026-06-25T09:30:00", captured[1].start)
        assertEquals("2026-06-25T10:30:00", captured[1].end)
        // One tagged block per timed event (positioned into its packed column).
        composeTestRule.onAllNodesWithTag("calendar-event-block", useUnmergedTree = true)
            .assertCountEquals(2)
    }

    @Test
    fun everyTimedEventGetsABlock() {
        val events = (1..3).map { ev("$it", "2026-06-25T0$it:00:00", "2026-06-25T0$it:30:00") }
        composeTestRule.setContent {
            TimelineColumn(
                events = events,
                onEventClick = {},
                onSlotClick = {},
                dayColumnLayout = { dayEvents ->
                    dayEvents.mapIndexed { i, _ -> timedPlacement(i * 60, i * 60 + 30, 0, 1) }
                },
            )
        }
        composeTestRule.onAllNodesWithTag("calendar-event-block", useUnmergedTree = true)
            .assertCountEquals(3)
    }

    @Test
    fun currentTimeLineShownOnToday() {
        // Empty events → the column never calls dayColumnLayout (stays FFI-free).
        composeTestRule.setContent {
            TimelineColumn(events = emptyList(), onEventClick = {}, onSlotClick = {}, showCurrentTimeLine = true)
        }
        composeTestRule.onNodeWithTag("calendar-current-time", useUnmergedTree = true).assertExists()
    }

    @Test
    fun currentTimeLineAbsentWhenNotToday() {
        composeTestRule.setContent {
            TimelineColumn(events = emptyList(), onEventClick = {}, onSlotClick = {}, showCurrentTimeLine = false)
        }
        composeTestRule.onNodeWithTag("calendar-current-time", useUnmergedTree = true).assertDoesNotExist()
    }

    // ── DayEventColumn (the shared per-day renderer the week grid reuses) ──────

    @Test
    fun dayEventColumn_tagsEachTimedBlock() {
        // The same shared renderer backs each of the week grid's 7 columns; one
        // tagged block per timed event, packed via the injected (FFI-free) layout.
        val events = listOf(
            ev("1", "2026-06-25T09:00:00", "2026-06-25T10:00:00"),
            ev("2", "2026-06-25T09:30:00", "2026-06-25T10:30:00"),
        )
        composeTestRule.setContent {
            Box(Modifier.size(width = 200.dp, height = 1440.dp)) {
                DayEventColumn(
                    events = events,
                    onEventClick = {},
                    onSlotClick = {},
                    dayColumnLayout = { dayEvents ->
                        dayEvents.mapIndexed { i, _ -> timedPlacement(540, 600, i, 2) }
                    },
                )
            }
        }
        composeTestRule.onAllNodesWithTag("calendar-event-block", useUnmergedTree = true)
            .assertCountEquals(2)
    }

    @Test
    fun dayEventColumn_excludesAllDayEvents() {
        // An all-day event (date-only DTSTART) belongs in the band, not the grid;
        // a column whose shared layout classifies every event all-day draws no
        // timed block. The injected fake keeps this FFI-free — the real
        // `day_column_layout` classification is unit-tested in shared Rust.
        composeTestRule.setContent {
            Box(Modifier.size(width = 200.dp, height = 1440.dp)) {
                DayEventColumn(
                    events = listOf(ev("a", "2026-06-25", null)),
                    onEventClick = {},
                    onSlotClick = {},
                    dayColumnLayout = { dayEvents ->
                        dayEvents.map {
                            FfiDayEventPlacement(
                                allDay = true, startMin = 0u, endMin = 0u, columnIndex = 0u, totalColumns = 0u,
                            )
                        }
                    },
                )
            }
        }
        composeTestRule.onAllNodesWithTag("calendar-event-block", useUnmergedTree = true)
            .assertCountEquals(0)
    }

    // ── Empty-slot quick-create targets (events-time-slot-{HH-MM}) ────────────

    @Test
    fun dayEventColumn_tilesQuarterHourSlotTargetsAcrossTheWholeDay() {
        // 96 targets tile the full 24h column (ui.yaml `events-time-slot-{HH-MM}`,
        // indexed by the slot's snapped start). Spot-checked at the four corners
        // of the tiling — first, both times the two e2e tests click, and last.
        composeTestRule.setContent {
            Box(Modifier.size(width = 200.dp, height = 1440.dp)) {
                DayEventColumn(events = emptyList(), onEventClick = {}, onSlotClick = {})
            }
        }
        listOf("00-00", "09-15", "13-30", "23-45").forEach { hhmm ->
            composeTestRule.onNodeWithTag("events-time-slot-$hhmm", useUnmergedTree = true)
                .assertExists()
        }
    }

    @Test
    fun slotClickReportsThatSlotsOwnClosureCapturedTime() {
        // Load-bearing for e2e: each slot carries its OWN time, never one derived
        // from the pointer's y — headless drivers emit their gesture at (0,0), so a
        // y-derived time would read 00:00 for every slot (linux's reference leg,
        // `week_grid.rs::add_time_slot_markers`). Two different slots, two
        // different answers, is what pins that.
        val clicked = mutableListOf<LocalTime>()
        composeTestRule.setContent {
            TimelineColumn(events = emptyList(), onEventClick = {}, onSlotClick = { clicked += it })
        }
        composeTestRule.onNodeWithTag("events-time-slot-09-15", useUnmergedTree = true)
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("events-time-slot-13-30", useUnmergedTree = true)
            .performScrollTo().performClick()
        assertEquals(listOf(LocalTime.of(9, 15), LocalTime.of(13, 30)), clicked)
    }

    @Test
    fun anEventBlockClickNeverQuickCreatesTheSlotBeneathIt() {
        // Block-vs-slot precedence is structural, not a gesture-ordering trick: the
        // blocks are added to the Box AFTER the slot targets, and Compose hit-tests
        // topmost-first — the same child-order solution linux/apple/windows use.
        // Without it, a click on an event would both open the event AND quick-create
        // under it (the bug linux's column-level background gesture shipped).
        val slotClicks = mutableListOf<LocalTime>()
        var eventClicks = 0
        composeTestRule.setContent {
            TimelineColumn(
                events = listOf(ev("1", "2026-06-25T09:00:00", "2026-06-25T10:00:00")),
                onEventClick = { eventClicks++ },
                onSlotClick = { slotClicks += it },
                dayColumnLayout = { listOf(timedPlacement(540, 600, 0, 1)) },
            )
        }
        composeTestRule.onNodeWithTag("calendar-event-block", useUnmergedTree = true)
            .performScrollTo().performClick()
        assertEquals(1, eventClicks)
        assertEquals(emptyList<LocalTime>(), slotClicks)
    }

    // ── AllDayBand ────────────────────────────────────────────────────────────

    @Test
    fun allDayBand_present_andRendersOnlyAllDayChips() {
        val allDay = ev("a", "2026-06-25", null)      // date-only → all-day chip
        val timed = ev("t", "2026-06-25T09:00:00", "2026-06-25T10:00:00") // excluded
        composeTestRule.setContent {
            AllDayBand(days = listOf(java.time.LocalDate.of(2026, 6, 25) to listOf(allDay, timed)), onEventClick = {})
        }
        composeTestRule.onNodeWithTag("calendar-allday-band", useUnmergedTree = true).assertExists()
        composeTestRule.onNodeWithText("Ea").assertExists()   // all-day chip rendered
        composeTestRule.onNodeWithText("Et").assertDoesNotExist() // timed event not in the band
    }
}
