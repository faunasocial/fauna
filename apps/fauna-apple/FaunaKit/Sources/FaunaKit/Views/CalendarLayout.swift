import Foundation
import SwiftUI

/// Shared geometry + layout math for the Outlook-style week/day time grids
/// (`WeekDayTimeGrid`), used by both apple apps (priority #2). The pixel
/// constants mirror the reference linux implementation
/// (`apps/fauna-linux/src/views/events/week_grid.rs`) so events lay out with the
/// same proportions across clients. All-day/timed classification, timed
/// minute geometry, and overlap column-packing are shared Rust
/// (`fauna_core::caltime::day_column_layout`, reached via `dayColumnLayout`,
/// events.md § Where logic lives); Gregorian date math (week dates, week-start,
/// which day an event falls on) stays on Swift `Calendar`/`DateComponents` —
/// platform date libraries are strictly better at that half.
public enum CalendarLayout {
    /// One half-hour row, in points. linux `HALF_HOUR_PX = 30.0`.
    public static let halfHourPx: CGFloat = 30
    /// One hour row (= 2 half-hours).
    public static let hourPx: CGFloat = halfHourPx * 2
    /// One empty-slot quick-create target (`events-time-slot-{HH-MM}`), in
    /// points. The *visual* row granularity stays half-hourly (events.md
    /// § Week & day timeline views); this is the finer granularity the ratified
    /// slot ids snap to, matching linux's 96 per-column markers.
    public static let quarterHourPx: CGFloat = halfHourPx / 2
    /// Number of empty-slot targets in one day column (24h at 15-min snap).
    public static let slotsPerDay: Int = 24 * 4
    /// Full 24-hour column height (48 half-hours). linux `COLUMN_HEIGHT`.
    public static let gridHeight: CGFloat = halfHourPx * 48
    /// Left hour-gutter width. linux gutter = 56px.
    public static let gutterWidth: CGFloat = 56
    /// Minimum timed-block height (one half-hour slot). linux `MIN_BLOCK_HEIGHT`.
    public static let minBlockHeight: CGFloat = halfHourPx
    /// Initial scroll offset (~08:00 working hours). linux `8 * 2 * HALF_HOUR_PX`.
    public static let scroll8amPx: CGFloat = halfHourPx * 2 * 8
    /// Horizontal gap between overlap sub-columns. linux subtracts 2px per block.
    public static let columnGap: CGFloat = 2

    /// Parse an event timestamp. The encrypted-CalDAV seam hands back RFC 3339
    /// (`EventSummary.dtstart`/`dtend`); also tolerate the bare date-only form
    /// (RFC 5545 `VALUE=DATE`, all-day events). Lifted from the four duplicated
    /// `parseDate` copies in the old per-app week/day views.
    public static func parseEventDate(_ str: String) -> Date? {
        let fmts = [
            "yyyy-MM-dd'T'HH:mm:ssZ",
            "yyyy-MM-dd'T'HH:mm:ss",
            "yyyy-MM-dd HH:mm:ss",
            "yyyy-MM-dd",
        ]
        for fmt in fmts {
            let df = DateFormatter()
            df.dateFormat = fmt
            if let d = df.date(from: str) { return d }
        }
        return nil
    }

    /// Minutes since local midnight (0…1440).
    public static func minutesSinceMidnight(_ date: Date, _ calendar: Calendar = .current) -> Int {
        calendar.component(.hour, from: date) * 60 + calendar.component(.minute, from: date)
    }

    /// `events` that start on `day` — the day-filter both grid views apply
    /// before laying anything out. Lifted from the byte-identical
    /// `eventsForDay` copies in `MonthGridView`/`WeekDayTimeGrid`.
    public static func eventsForDay(_ events: [EventSummary], day: Date, calendar: Calendar = .current) -> [EventSummary] {
        events.filter { event in
            guard let start = parseEventDate(event.dtstart) else { return false }
            return calendar.isDate(start, inSameDayAs: day)
        }
    }

    /// A timed event positioned within a single day column. `columnIndex` /
    /// `totalColumns` come from the shared `dayColumnLayout`'s overlap packing;
    /// the view turns them into pixel x/width over the live column width.
    public struct TimedBlock: Identifiable {
        public let event: EventSummary
        public let topPx: CGFloat
        public let heightPx: CGFloat
        public let columnIndex: Int
        public let totalColumns: Int
        public var id: String { event.id }
    }

    /// The split + positioned layout for one day's events: all-day events (for the
    /// band) and pixel-positioned, overlap-packed timed blocks.
    public struct DayLayout {
        public let allDay: [EventSummary]
        public let timed: [TimedBlock]
    }

    /// Lay out one day's events (`events` should already be filtered to the
    /// day). All-day/timed classification, timed minute geometry, and overlap
    /// column packing all come from the shared `dayColumnLayout` — one entry
    /// per input event, input-order aligned — so identical events lay out
    /// identically across clients.
    public static func layoutDay(events: [EventSummary]) -> DayLayout {
        guard !events.isEmpty else { return DayLayout(allDay: [], timed: []) }
        let placements = dayColumnLayout(
            events: events.map { FfiDayEvent(start: $0.dtstart, end: $0.dtend) }
        )

        var allDay: [EventSummary] = []
        var timed: [TimedBlock] = []
        for (event, placement) in zip(events, placements) {
            if placement.allDay {
                allDay.append(event)
                continue
            }
            let topPx = CGFloat(placement.startMin) / 30.0 * halfHourPx
            let durationMin = max(Int(placement.endMin) - Int(placement.startMin), 30)
            let heightPx = max(CGFloat(durationMin) / 30.0 * halfHourPx, minBlockHeight)
            timed.append(TimedBlock(
                event: event,
                topPx: topPx,
                heightPx: heightPx,
                columnIndex: Int(placement.columnIndex),
                totalColumns: Int(max(placement.totalColumns, 1))
            ))
        }
        return DayLayout(allDay: allDay, timed: timed)
    }

    /// The hour-gutter label for `hour` (00:00 … 23:00), 24-hour like linux.
    public static func hourLabel(_ hour: Int) -> String {
        String(format: "%02d:00", hour)
    }
}
