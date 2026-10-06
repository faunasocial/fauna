package com.fauna.app.ui.screen.events

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import com.fauna.app.R
import java.time.DayOfWeek
import java.time.LocalDate

/**
 * Composed calendar header labels — month-name/weekday-name resolved through
 * the app's own i18n catalog (`docs/goal/ui/events.md` § Where logic lives:
 * "the localized month/weekday name strings" are client glue, not
 * `fauna_core`), never the OS-locale `DateTimeFormatter.ofPattern`, so the
 * calendar stays in the app's chosen language regardless of device locale —
 * the same reasoning that already moved the weekday-header row off
 * `ofPattern("EEE")`. Mirrors the shape of linux's
 * `views/events/time_utils.rs` `format_month_label`/`format_day_label`
 * (the lead for this client-side logic), which composes plain
 * `"{name} {number}"` strings rather than a locale-reordering mechanism.
 */

@Composable
private fun monthFullName(month: Int): String = stringResource(
    when (month) {
        1 -> R.string.time_month_full_jan
        2 -> R.string.time_month_full_feb
        3 -> R.string.time_month_full_mar
        4 -> R.string.time_month_full_apr
        5 -> R.string.time_month_full_may
        6 -> R.string.time_month_full_jun
        7 -> R.string.time_month_full_jul
        8 -> R.string.time_month_full_aug
        9 -> R.string.time_month_full_sep
        10 -> R.string.time_month_full_oct
        11 -> R.string.time_month_full_nov
        else -> R.string.time_month_full_dec
    }
)

@Composable
private fun monthShortName(month: Int): String = stringResource(
    when (month) {
        1 -> R.string.time_month_jan
        2 -> R.string.time_month_feb
        3 -> R.string.time_month_mar
        4 -> R.string.time_month_apr
        5 -> R.string.time_month_may
        6 -> R.string.time_month_jun
        7 -> R.string.time_month_jul
        8 -> R.string.time_month_aug
        9 -> R.string.time_month_sep
        10 -> R.string.time_month_oct
        11 -> R.string.time_month_nov
        else -> R.string.time_month_dec
    }
)

@Composable
private fun weekdayFullName(dayOfWeek: DayOfWeek): String = stringResource(
    when (dayOfWeek) {
        DayOfWeek.MONDAY -> R.string.time_weekday_full_mon
        DayOfWeek.TUESDAY -> R.string.time_weekday_full_tue
        DayOfWeek.WEDNESDAY -> R.string.time_weekday_full_wed
        DayOfWeek.THURSDAY -> R.string.time_weekday_full_thu
        DayOfWeek.FRIDAY -> R.string.time_weekday_full_fri
        DayOfWeek.SATURDAY -> R.string.time_weekday_full_sat
        DayOfWeek.SUNDAY -> R.string.time_weekday_full_sun
    }
)

/** "July 2026" — mirrors linux `format_month_label`. */
@Composable
fun monthYearLabel(date: LocalDate): String =
    "${monthFullName(date.monthValue)} ${date.year}"

/** "Wednesday, Jul 29, 2026" — mirrors linux `format_day_label`. */
@Composable
fun fullDayLabel(date: LocalDate): String =
    "${weekdayFullName(date.dayOfWeek)}, ${monthShortName(date.monthValue)} ${date.dayOfMonth}, ${date.year}"

/**
 * "Jul 27 - Aug 2, 2026" — the week-range label. Keeps android's existing
 * always-both-months shape (unlike linux's richer same-month/cross-year
 * branching, out of scope for this fix) and only swaps the month
 * abbreviation off the OS locale onto the app's i18n catalog.
 */
@Composable
fun weekRangeLabel(weekStart: LocalDate, weekEnd: LocalDate): String =
    "${monthShortName(weekStart.monthValue)} ${weekStart.dayOfMonth} - " +
        "${monthShortName(weekEnd.monthValue)} ${weekEnd.dayOfMonth}, ${weekEnd.year}"
