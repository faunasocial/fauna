package com.fauna.app.ui.events

import com.fauna.app.R
import com.fauna.app.ui.screen.events.snapToWeekStart
import com.fauna.app.ui.screen.events.weekdayAbbreviationsFrom
import org.junit.Assert.assertEquals
import org.junit.Test
import java.time.DayOfWeek
import java.time.LocalDate

/**
 * android's grids, headers and range labels agree on ONE locale-derived week
 * start (`ui/events.md` § Week & day timeline views — "the week's range label
 * reads the same value as the grid beside it, never its own").
 *
 * **These assert the column identity, not only the header text**, which is the
 * lesson the tui/linux pass paid for: the two failure modes are *different*
 * (the header ignores `week_start`; the grid reverts to Monday) and each kills
 * a different assertion. A test that checks only the header passes while every
 * column underneath it is wrong.
 *
 * Deliberately a plain JUnit test over the pure helpers — no Robolectric, no
 * FFI. The rotation and the snap ARE the logic; driving Compose to reach them
 * would test the framework instead.
 *
 * **Mutation-checked 2026-08-22, and the result is the argument for the shape.**
 * Reverting `snapToWeekStart` to a hardcoded `DayOfWeek.MONDAY` — the grid
 * reverting while the header stays locale-aware — kills
 * [theGridSnapsBackToWhicheverDayTheLocaleStartsOn] and
 * [theFirstHeaderCellNamesTheFirstColumnsDay], while
 * [theHeaderRotatesOffTheSameValueTheGridUsed] **survives it**: the header is
 * still perfectly correct, so a header-only assertion passes with every column
 * beneath it wrong. That is the exact vacuity this file is written against, and
 * it is why the agreement is asserted directly rather than inferred from the two
 * halves being individually right.
 */
class WeekStartRotationTest {

    /** `2026-08-22` is a Saturday — far enough from either boundary to move under every start. */
    private val saturday = LocalDate.of(2026, 8, 22)

    @Test
    fun theGridSnapsBackToWhicheverDayTheLocaleStartsOn() {
        assertEquals(
            "Monday-start locales snap back to the 17th",
            LocalDate.of(2026, 8, 17),
            saturday.snapToWeekStart(DayOfWeek.MONDAY),
        )
        assertEquals(
            "Sunday-start locales (US) snap back to the 16th — a DIFFERENT column set",
            LocalDate.of(2026, 8, 16),
            saturday.snapToWeekStart(DayOfWeek.SUNDAY),
        )
        assertEquals(
            "Saturday-start locales keep the anchor itself",
            saturday,
            saturday.snapToWeekStart(DayOfWeek.SATURDAY),
        )
    }

    /**
     * The header rotates to match, off the SAME value the grid snapped with.
     *
     * `DayOfWeek.getValue()` is `1 = Mon … 7 = Sun` while the catalog list is
     * index `0 = Mon` — the origin mismatch § Where logic lives warns is
     * "invisible in review and shifts every grid by a day". This is the pin on
     * that one conversion.
     */
    @Test
    fun theHeaderRotatesOffTheSameValueTheGridUsed() {
        assertEquals(
            listOf(
                R.string.time_weekday_mon, R.string.time_weekday_tue, R.string.time_weekday_wed,
                R.string.time_weekday_thu, R.string.time_weekday_fri, R.string.time_weekday_sat,
                R.string.time_weekday_sun,
            ),
            weekdayAbbreviationsFrom(DayOfWeek.MONDAY),
        )
        assertEquals(
            listOf(
                R.string.time_weekday_sun, R.string.time_weekday_mon, R.string.time_weekday_tue,
                R.string.time_weekday_wed, R.string.time_weekday_thu, R.string.time_weekday_fri,
                R.string.time_weekday_sat,
            ),
            weekdayAbbreviationsFrom(DayOfWeek.SUNDAY),
        )
        assertEquals(
            listOf(
                R.string.time_weekday_sat, R.string.time_weekday_sun, R.string.time_weekday_mon,
                R.string.time_weekday_tue, R.string.time_weekday_wed, R.string.time_weekday_thu,
                R.string.time_weekday_fri,
            ),
            weekdayAbbreviationsFrom(DayOfWeek.SATURDAY),
        )
    }

    /**
     * The header's first cell names the day the grid's first column actually
     * holds — the agreement itself, asserted directly rather than inferred from
     * the two properties above. This is the assertion that dies if either half
     * is reverted to Monday while the other follows the locale.
     */
    @Test
    fun theFirstHeaderCellNamesTheFirstColumnsDay() {
        val abbreviationOf = mapOf(
            DayOfWeek.MONDAY to R.string.time_weekday_mon,
            DayOfWeek.TUESDAY to R.string.time_weekday_tue,
            DayOfWeek.WEDNESDAY to R.string.time_weekday_wed,
            DayOfWeek.THURSDAY to R.string.time_weekday_thu,
            DayOfWeek.FRIDAY to R.string.time_weekday_fri,
            DayOfWeek.SATURDAY to R.string.time_weekday_sat,
            DayOfWeek.SUNDAY to R.string.time_weekday_sun,
        )
        for (firstDay in DayOfWeek.entries) {
            val firstColumnDay = saturday.snapToWeekStart(firstDay).dayOfWeek
            assertEquals(
                "grid and header disagree for a $firstDay-start locale",
                abbreviationOf[firstColumnDay],
                weekdayAbbreviationsFrom(firstDay).first(),
            )
        }
    }
}
