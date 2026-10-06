package com.fauna.app.ui.viewmodel

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.screen.events.calendarViewTag
import com.fauna.app.ui.screen.events.pannedBy
import com.fauna.ffi.FfiCalendarViewMode
import com.fauna.ffi.calendarViewModeWire
import com.fauna.ffi.calendarViewModes
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import java.time.LocalDate

/**
 * android's calendar view vocabulary and pan policy are the SHARED ones.
 *
 * This file replaces `CalendarViewEnumTest`, which pinned the retired local
 * `CalendarView` enum — including the two facts that were the divergence:
 * that the date-unfiltered list was spelled `LIST` (the shared `from_wire`
 * refuses that word), and that it sorted first in a LIST/DAY/WEEK/MONTH
 * toggle order no other app used.
 *
 * `events.md` § Where logic lives → *View mode + visible range*.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class CalendarViewVocabularyTest {

    @Test
    fun theToggleOrderAndSpellingAreTheSharedOnes() {
        assertEquals(
            listOf("agenda", "month", "week", "day"),
            calendarViewModes().map { calendarViewModeWire(it) },
        )
        assertEquals("calendar-view-agenda", calendarViewTag(FfiCalendarViewMode.AGENDA))
    }

    /**
     * One click moves exactly one visible range — the whole point of the lift.
     * Before it, all three of android's grids agreed by triplication; the pin
     * is what stops the next edit to one of them from disagreeing.
     */
    @Test
    fun oneClickMovesOneVisibleRangePerMode() {
        val anchor = LocalDate.of(2026, 8, 22)

        assertEquals(
            LocalDate.of(2026, 9, 22),
            anchor.pannedBy(FfiCalendarViewMode.MONTH, forward = true),
        )
        assertEquals(
            LocalDate.of(2026, 8, 29),
            anchor.pannedBy(FfiCalendarViewMode.WEEK, forward = true),
        )
        assertEquals(
            LocalDate.of(2026, 8, 23),
            anchor.pannedBy(FfiCalendarViewMode.DAY, forward = true),
        )
    }

    @Test
    fun panningIsSymmetricBackwards() {
        val anchor = LocalDate.of(2026, 8, 22)
        assertEquals(
            LocalDate.of(2026, 7, 22),
            anchor.pannedBy(FfiCalendarViewMode.MONTH, forward = false),
        )
        assertEquals(
            LocalDate.of(2026, 8, 15),
            anchor.pannedBy(FfiCalendarViewMode.WEEK, forward = false),
        )
        assertEquals(
            LocalDate.of(2026, 8, 21),
            anchor.pannedBy(FfiCalendarViewMode.DAY, forward = false),
        )
    }

    /**
     * The agenda list is date-unfiltered, so its pan control must do NOTHING —
     * not move by zero, not move invisibly.
     *
     * Asserted on the anchor date itself, which is the finest-grained render of
     * the state being guarded. A coarser observable (a month label) cannot
     * witness a small move: the tui slice found exactly that, where an
     * `Agenda => Days(7)` mutant shifted the anchor a whole week without moving
     * the month label above it, and the assertion still passed.
     */
    @Test
    fun panningIsInertInTheDateUnfilteredAgenda() {
        val anchor = LocalDate.of(2026, 8, 22)
        assertEquals(anchor, anchor.pannedBy(FfiCalendarViewMode.AGENDA, forward = true))
        assertEquals(anchor, anchor.pannedBy(FfiCalendarViewMode.AGENDA, forward = false))
    }

    /**
     * A month step clamps onto a shorter month, and android's `java.time` walk
     * must land where the Rust `pan()` does for the Rust apps — the shared
     * policy is only worth sharing if the two applications of it agree.
     */
    @Test
    fun aMonthStepClampsOntoAShorterMonthLikeTheRustConsumers() {
        assertEquals(
            LocalDate.of(2026, 2, 28),
            LocalDate.of(2026, 1, 31).pannedBy(FfiCalendarViewMode.MONTH, forward = true),
        )
    }
}
