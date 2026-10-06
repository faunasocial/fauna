import Foundation

/// Apple's leg of the shared calendar **view-mode vocabulary** — the twin of
/// android's `EventsPan.kt`, and the reason macOS and iOS no longer each carry
/// their own `enum` for the same four modes
/// (`docs/goal/ui/events.md` § Where logic lives → *View mode + visible range*).
///
/// Everything that can be decided once is decided in `fauna_core::caltime` and
/// reaches here through `libs/fauna-ffi/src/caltime.rs`: which modes exist and
/// in what toggle order (`calendarViewModes()`), each mode's cross-app wire
/// spelling (`calendarViewModeWire`), and **how far one pan click moves**
/// (`calendarPanStep`). What stays on this side is only what the goal doc's
/// carve-out deliberately keeps platform-side: the Gregorian walk itself, on
/// `Calendar`, plus the localized label a human reads.
public extension FfiCalendarViewMode {

    /// The `calendar-view-*` element id, built from the shared wire word so the
    /// id cannot drift from the vocabulary the other six apps switch on.
    var accessibilityId: String { "calendar-view-\(wire)" }

    /// The mode's cross-app wire spelling — also the `events-view-toggle`
    /// automation value. Lowercase, like every other app: apple used to publish
    /// a capitalized Swift `rawValue` here, which was the odd one out.
    var wire: String { calendarViewModeWire(mode: self) }

    /// The localized toggle label. The only part of the mode a human reads, and
    /// the only part that may differ per locale — the id and the wire word above
    /// must not.
    var displayLabel: String {
        switch self {
        case .agenda: L.events.viewAgenda
        case .month: L.events.viewMonth
        case .week: L.events.viewWeek
        case .day: L.events.viewDay
        }
    }

    /// Apply one `events-prev-month` / `events-next-month` click to `anchor` in
    /// this mode.
    ///
    /// **The policy is shared; the date math is apple's.** `calendarPanStep`
    /// answers only *how far* — `Months(1)`, `Days(7)`, `Days(1)` or `None` —
    /// and the `Calendar.current.date(byAdding:)` walk below is what
    /// `events.md` § Where logic lives keeps on the platform's own calendar
    /// library. ⚠ Do **not** reach for the Rust `pan()` instead: that is the
    /// seam for the Rust and wasm consumers, and using it would move the walk
    /// off `Calendar`, collapsing exactly the split the carve-out protects.
    ///
    /// `.none` returns the anchor **unchanged**, and that is the contract rather
    /// than a step of zero: the agenda list is date-unfiltered, so panning it
    /// would move state nothing renders.
    func panned(from anchor: Date, forward: Bool) -> Date {
        let sign = forward ? 1 : -1
        switch calendarPanStep(mode: self) {
        case .months(let count):
            return Calendar.current.date(
                byAdding: .month, value: sign * Int(count), to: anchor) ?? anchor
        case .days(let count):
            return Calendar.current.date(
                byAdding: .day, value: sign * Int(count), to: anchor) ?? anchor
        case .none:
            return anchor
        }
    }
}
