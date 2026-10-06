import Testing
import Foundation
@testable import FaunaKit

/// `CalendarLayout` is the shared week/day time-grid math (`WeekDayTimeGrid`),
/// used by both apple apps (priority #2, events.md § Week & day timeline
/// views). The encrypted-CalDAV seam hands `EventSummary.dtstart`/`dtend` back as
/// RFC 3339 with a trailing `Z` (every `fauna-client-caldav` fixture is
/// `"2026-06-02T09:00:00Z"`), so `parseEventDate` MUST parse that form — a nil
/// parse silently drops the event from `eventsForDay` on every day column, which
/// is exactly the "week/day renders zero `calendar-event-block`" regression.

private let utc: Calendar = {
    var c = Calendar(identifier: .gregorian)
    c.timeZone = TimeZone(identifier: "UTC")!
    return c
}()

private func ev(_ dtstart: String, _ dtend: String, summary: String = "Standup") -> EventSummary {
    EventSummary(id: "id-\(dtstart)", uid: "uid", summary: summary,
                 dtstart: dtstart, dtend: dtend)
}

// MARK: - parseEventDate (the decisive A2 check)

@Test func parseEventDateAcceptsRfc3339Zulu() {
    // The real wire form from the encrypted-CalDAV decode. If this is nil the
    // event is filtered out of every day column → zero positioned blocks.
    #expect(CalendarLayout.parseEventDate("2026-06-28T10:00:00Z") != nil)
}

@Test func parseEventDateAcceptsRfc3339NumericOffset() {
    #expect(CalendarLayout.parseEventDate("2026-06-28T10:00:00+00:00") != nil)
    #expect(CalendarLayout.parseEventDate("2026-06-28T10:00:00+0200") != nil)
}

@Test func parseEventDateAcceptsNoZoneAndDateOnly() {
    #expect(CalendarLayout.parseEventDate("2026-06-28T10:00:00") != nil)
    #expect(CalendarLayout.parseEventDate("2026-06-28") != nil)
}

// MARK: - layoutDay produces a positioned timed block

@Test func layoutDayPositionsTimedBlockForZuluEvent() {
    let layout = CalendarLayout.layoutDay(
        events: [ev("2026-06-28T10:00:00Z", "2026-06-28T11:00:00Z")])
    #expect(layout.timed.count == 1)
    #expect(layout.allDay.isEmpty)
    // 10:00 → 600 minutes → topPx 600 (halfHourPx 30, 600/30*30).
    #expect(layout.timed.first?.topPx == 600)
}

@Test func zuluTimedEventNotClassifiedAllDay() {
    #expect(!eventIsAllDay(start: "2026-06-28T10:00:00Z", end: "2026-06-28T11:00:00Z"))
}

// MARK: - the eventsForDay same-day match (the WeekDayTimeGrid filter)

@Test func zuluEventMatchesItsOwnDay() {
    let start = CalendarLayout.parseEventDate("2026-06-28T10:00:00Z")
    let day = CalendarLayout.parseEventDate("2026-06-28T00:00:00Z")
    #expect(start != nil && day != nil)
    if let start, let day {
        #expect(utc.isDate(start, inSameDayAs: day))
    }
}
