package com.fauna.app.ui.screen.events

import androidx.annotation.StringRes
import com.fauna.app.R
import java.time.DayOfWeek
import java.time.LocalDate
import java.time.temporal.TemporalAdjusters
import java.time.temporal.WeekFields
import java.util.Locale

/**
 * The device locale's first day of the week — android's half of the
 * **per-platform** week-start probe (`ui/events.md` § Where logic lives →
 * *Pure calendar/date primitives*, amended 2026-08-01).
 *
 * Deliberately `java.time`'s own answer rather than the shared Rust
 * `caltime::locale_week_start`: that probe is native-Rust-only by design, and
 * `WeekFields` carries the full CLDR data the Rust table only approximates.
 * The *rule* is shared; the lookup is the platform's, exactly as with the
 * Gregorian date math beside it.
 */
fun localeFirstDayOfWeek(): DayOfWeek = WeekFields.of(Locale.getDefault()).firstDayOfWeek

/**
 * Snap [this] back to the start of its week under [firstDay].
 *
 * **[firstDay] is a parameter, never re-probed inside**, and that is the fix
 * shape rather than an accident of style: linux shipped a range label that
 * probed the locale independently of its own grid and so could name a
 * different week than the columns underneath it. Passing one value to the
 * grid, the header and the label makes that disagreement unrepresentable.
 */
fun LocalDate.snapToWeekStart(firstDay: DayOfWeek): LocalDate =
    with(TemporalAdjusters.previousOrSame(firstDay))

/**
 * The seven weekday abbreviations in column order for a grid starting on
 * [firstDay].
 *
 * The strings come from the app's own i18n catalog, never a locale-dependent
 * platform formatter — `events.md` keeps localized month/weekday *names* on
 * the client catalog, because a platform formatter renders a language from
 * inside the binary where no catalog can translate it. What the locale decides
 * here is only the **rotation**.
 *
 * `DayOfWeek.getValue()` is `1 = Mon … 7 = Sun`, so the catalog index is
 * `value - 1` — this is the seam the § warns about, where every platform uses a
 * different origin (.NET `0 = Sun`, Swift `1 = Sun`, caltime `0 = Mon`). It is
 * converted once, here, and pinned by test.
 */
fun weekdayAbbreviationsFrom(firstDay: DayOfWeek): List<Int> {
    val offset = firstDay.value - 1
    return List(MONDAY_FIRST_WEEKDAY_ABBREVIATIONS.size) { i ->
        MONDAY_FIRST_WEEKDAY_ABBREVIATIONS[(offset + i) % MONDAY_FIRST_WEEKDAY_ABBREVIATIONS.size]
    }
}

/** The catalog's own order: index 0 is Monday, matching caltime's `0 = Mon`. */
@get:StringRes
private val MONDAY_FIRST_WEEKDAY_ABBREVIATIONS = listOf(
    R.string.time_weekday_mon,
    R.string.time_weekday_tue,
    R.string.time_weekday_wed,
    R.string.time_weekday_thu,
    R.string.time_weekday_fri,
    R.string.time_weekday_sat,
    R.string.time_weekday_sun,
)
