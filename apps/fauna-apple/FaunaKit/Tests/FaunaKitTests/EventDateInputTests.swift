import Testing
import Foundation
@testable import FaunaKit

/// `EventDateInput.parse` normalizes the combined `event-dtstart`/`event-dtend`
/// field (`YYYY-MM-DDTHH:MM`) into a seconds-bearing RFC 3339 datetime for the
/// API, shared by both apple shells (priority #2/#4). The seconds MUST be added —
/// a seconds-less value reaches the nest's iCalendar serializer as a malformed
/// `T1400`, which no client's week/day time-grid date parser can read, so the
/// event silently vanishes from every grid column. The old
/// implementation gated on `ISO8601DateFormatter` (which rejects the timezone-less
/// `…:00` form) and self-defeatingly fell back to the seconds-less string.

@Test func parseAppendsSecondsToBareMinuteTime() {
    #expect(EventDateInput.parse("2026-06-28T14:00") == "2026-06-28T14:00:00")
    #expect(EventDateInput.parse("2026-06-28T10:00") == "2026-06-28T10:00:00")
}

@Test func parseTrimsWhitespaceBeforePadding() {
    #expect(EventDateInput.parse("  2026-06-28T14:00  ") == "2026-06-28T14:00:00")
}

@Test func parsePassesThroughSecondsAndTimezoneForms() {
    #expect(EventDateInput.parse("2026-06-28T14:00:00") == "2026-06-28T14:00:00")
    #expect(EventDateInput.parse("2026-06-28T14:00:00Z") == "2026-06-28T14:00:00Z")
    #expect(EventDateInput.parse("2026-06-28T14:00:00+02:00") == "2026-06-28T14:00:00+02:00")
}

@Test func parseLeavesDateOnlyUntouched() {
    #expect(EventDateInput.parse("2026-06-28") == "2026-06-28")
}
