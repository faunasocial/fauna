package com.fauna.app.ui.screen.events

import com.fauna.ffi.FfiCalendarViewMode
import com.fauna.ffi.FfiPanStep
import com.fauna.ffi.calendarPanStep
import java.time.LocalDate

/**
 * Apply one `events-prev-month` / `events-next-month` click to [this] anchor
 * date, in [mode].
 *
 * **The policy is shared; the date math is android's.** `calendarPanStep` is
 * `fauna_core::caltime::pan_step` across the UniFFI boundary and answers only
 * *how far* — `Months(1)`, `Days(7)`, `Days(1)`, or `None` — while the
 * `java.time` walk below is what `events.md` § Where logic lives deliberately
 * keeps on the platform's own calendar library. Do not reach for the Rust
 * `pan()` here: that is the seam for the Rust and wasm consumers, and porting
 * it would collapse exactly the split that carve-out exists to protect.
 *
 * Before this, android held the policy **three times** — `MonthGridContent`
 * stepped months, `WeekGridContent` weeks, `DayTimelineContent` days, each
 * inline in its own button — plus a fourth copy in `EventsVM.navigateDate`.
 * All four agreed, which is precisely why it survived: triplication that
 * happens to be correct still drifts the moment one copy is edited, and the
 * other six apps had already disagreed six different ways on this same control.
 *
 * [FfiPanStep.None] returns the anchor **unchanged**, and that is the contract
 * rather than a zero step: the agenda list is date-unfiltered, so panning it
 * would move state nothing renders.
 */
fun LocalDate.pannedBy(mode: FfiCalendarViewMode, forward: Boolean): LocalDate {
    val sign = if (forward) 1L else -1L
    return when (val step = calendarPanStep(mode)) {
        is FfiPanStep.Months -> plusMonths(sign * step.count.toLong())
        is FfiPanStep.Days -> plusDays(sign * step.count.toLong())
        is FfiPanStep.None -> this
    }
}

/** The `calendar-view-*` element id for [mode], built from the shared wire word. */
fun calendarViewTag(mode: FfiCalendarViewMode): String =
    "calendar-view-" + com.fauna.ffi.calendarViewModeWire(mode)
