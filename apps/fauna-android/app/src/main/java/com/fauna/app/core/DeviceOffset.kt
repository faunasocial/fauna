package com.fauna.app.core

import java.time.Instant
import java.time.ZoneId

/**
 * The device's UTC offset — the ONE place android reads it (value-formatting.md
 * § Absolute local timestamp display, the app-side one-door rule). Shared Rust
 * is WASM-safe and owns no clock, so every app supplies `utc_offset_seconds` /
 * offset-minutes itself — and supplies it from one door, because the value is
 * not display-only: it rides `fauna.family.usage_report` / `notify_report`,
 * where the nest persists it as the ward-local day bucket. Before this door
 * android derived it through TWO different APIs (`java.time.ZoneId` and the
 * legacy `java.util.TimeZone`), which are not guaranteed to agree — so the day
 * the nest stored could drift from the day the app rendered against. `ZoneId`
 * (the modern, DST-correct API) is the resolution; do not reintroduce
 * `java.util.TimeZone` here or at any call site.
 */
object DeviceOffset {
    /** Seconds east of UTC — the unit the shared formatters take. */
    fun utcOffsetSeconds(): Int =
        ZoneId.systemDefault().rules.getOffset(Instant.now()).totalSeconds

    /** Minutes east of UTC — the unit the family day-bucket rule wants
     *  (`family-safety.md` § Screen time). */
    fun utcOffsetMinutes(): Int = utcOffsetSeconds() / 60
}
