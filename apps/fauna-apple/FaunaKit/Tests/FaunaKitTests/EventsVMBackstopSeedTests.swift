import Testing
import Foundation
@testable import FaunaKit

// The Events-page backstop poll's delta-sync optimization
// (`docs/goal/ui/events.md` § Implementation status today — the delta-sync
// backstop). The shared
// `fauna_client_caldav::delta_sync` seam is Rust-side and already
// mutation-graded there; what this file pins is apple's OWN wiring: the
// backstop poll (`refreshIfChanged`, reached here via `refreshFromPush`)
// must consult `queryEventsSeeded` — never the full `queryEvents` — and must
// perform ZERO full reads when every polled calendar reports unchanged.

/// A fake `EventsAPI` that answers `queryEventsSeeded` from a script
/// (`nil` = unchanged) while separately counting calls to the full
/// `queryEvents` — the two counters are the whole point: a passing test must
/// show the seeded seam consulted and the full read NOT paid for.
@MainActor
private final class SeededEventsAPI: EventsAPI {
    var calendars: [FaunaCalendar] = []
    /// Per-calendar-id scripted answers for `queryEventsSeeded`. Missing key
    /// or explicit `nil` value both mean "unchanged" — `nil` is the seam's
    /// own "nothing to report" case represented directly.
    var seededAnswers: [String: [EventSummary]?] = [:]
    /// What a fallback full `queryEvents` returns, by calendar id.
    var fullAnswers: [String: [EventSummary]] = [:]

    private(set) var seededCallCount = 0
    private(set) var fullReadCallCount = 0
    private(set) var seededCalledCalendarIds: [String] = []
    private(set) var fullReadCalledCalendarIds: [String] = []

    func listCalendars() async throws -> [FaunaCalendar] { calendars }

    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? {
        seededCallCount += 1
        seededCalledCalendarIds.append(calendarId)
        return seededAnswers[calendarId] ?? nil
    }

    func queryEvents(calendarId: String) async throws -> [EventSummary] {
        fullReadCallCount += 1
        fullReadCalledCalendarIds.append(calendarId)
        return fullAnswers[calendarId] ?? []
    }

    func queryMyEvents(filter: String) async throws -> [EventSummary] { [] }
    func createCalendar(name: String) async throws {}
    func createEvent(_ request: CreateEventRequest) async throws {}
    func deleteEvent(id: String) async throws {}
    func inviteToEvent(eventId: String, email: String) async throws {}
    func rsvpEvent(eventId: String, response: RsvpResponse) async throws {}
    func setReminder(eventId: String, offset: String) async throws {}
    func removeReminder(eventId: String) async throws {}
    func exportCalendar(calendarId: String) async throws -> String { "" }

    func getEvent(id: String) async throws -> EventDetail {
        EventDetail(id: id, uid: id, calendarId: "cal-1", summary: "",
                    dtstart: "", dtend: "", organizer: "", organizedByMe: true)
    }

    func importCalendar(calendarId: String, icsText: String) async throws -> CalendarImportResult {
        CalendarImportResult(imported: 0, skipped: 0, updated: nil, total: 0, errors: nil)
    }
}

private func event(_ id: String, calendarId: String) -> EventSummary {
    EventSummary(id: id, uid: id, summary: "Event \(id)",
                 dtstart: "2026-08-28T14:00", dtend: "2026-08-28T15:00",
                 calendarId: calendarId)
}

private let cal1 = FaunaCalendar(id: "cal-1", name: "Cal One")
private let cal2 = FaunaCalendar(id: "cal-2", name: "Cal Two")

@MainActor
@Test("single-calendar backstop poll: unchanged performs NO full read and leaves events untouched")
func singleCalendarUnchangedSkipsFullRead() async {
    let api = SeededEventsAPI()
    api.calendars = [cal1]
    api.fullAnswers["cal-1"] = [event("ev-a", calendarId: "cal-1")]
    let vm = EventsVM()
    vm.configure(api: api)
    vm.calendars = [cal1]
    // The initial select pays one full read (mirrors production: a page
    // always shows a real list before any poll tick can run).
    await vm.selectCalendar(cal1)
    #expect(vm.events.map(\.id) == ["ev-a"])
    #expect(api.fullReadCallCount == 1)

    // The calendar is unchanged: the seam reports nil for it.
    api.seededAnswers["cal-1"] = .some(nil)
    await vm.refreshFromPush()

    #expect(api.seededCallCount == 1, "the poll must consult the seeded seam")
    #expect(api.fullReadCallCount == 1, "an unchanged calendar must not pay for a second full read")
    #expect(vm.events.map(\.id) == ["ev-a"], "the model must be untouched by a no-op tick")
}

@MainActor
@Test("single-calendar backstop poll: changed uses the seeded payload directly, still no extra full read")
func singleCalendarChangedUsesSeededPayload() async {
    let api = SeededEventsAPI()
    api.calendars = [cal1]
    api.fullAnswers["cal-1"] = [event("ev-a", calendarId: "cal-1")]
    let vm = EventsVM()
    vm.configure(api: api)
    vm.calendars = [cal1]
    await vm.selectCalendar(cal1)
    #expect(api.fullReadCallCount == 1)

    // The seam itself performed the read (that is its own "ReadRequired"
    // fallback) and hands the fresh list straight back as `Some(events)`.
    api.seededAnswers["cal-1"] = .some([event("ev-a", calendarId: "cal-1"), event("ev-b", calendarId: "cal-1")])
    await vm.refreshFromPush()

    #expect(api.seededCallCount == 1)
    #expect(api.fullReadCallCount == 1, "the seeded call's own full read must not be paid for AGAIN via queryEvents")
    #expect(vm.events.map(\.id).sorted() == ["ev-a", "ev-b"])
}

@MainActor
@Test("union backstop poll (no selection): every calendar unchanged performs NO full read")
func unionAllUnchangedSkipsFullRead() async {
    let api = SeededEventsAPI()
    api.calendars = [cal1, cal2]
    api.fullAnswers["cal-1"] = [event("ev-a", calendarId: "cal-1")]
    api.fullAnswers["cal-2"] = [event("ev-b", calendarId: "cal-2")]
    let vm = EventsVM()
    vm.configure(api: api)
    vm.calendars = [cal1, cal2]
    // No selection — force the initial load through the same union path the
    // poll will use, so `fullReadCallCount` starts from a known baseline.
    await vm.loadCalendars()
    let baselineFullReads = api.fullReadCallCount
    #expect(Set(vm.events.map(\.id)) == ["ev-a", "ev-b"])

    api.seededAnswers["cal-1"] = .some(nil)
    api.seededAnswers["cal-2"] = .some(nil)
    await vm.refreshFromPush()

    #expect(api.seededCallCount == 2, "the poll must consult the seam for every calendar in the union")
    #expect(api.fullReadCallCount == baselineFullReads,
            "every calendar unchanged must not pay for a single full read")
    #expect(Set(vm.events.map(\.id)) == ["ev-a", "ev-b"], "the model must be untouched by an all-quiet tick")
}

@MainActor
@Test("union backstop poll: one changed calendar falls through to exactly one full union read")
func unionOneChangedFallsThroughToFullRead() async {
    let api = SeededEventsAPI()
    api.calendars = [cal1, cal2]
    api.fullAnswers["cal-1"] = [event("ev-a", calendarId: "cal-1")]
    api.fullAnswers["cal-2"] = [event("ev-b", calendarId: "cal-2")]
    let vm = EventsVM()
    vm.configure(api: api)
    vm.calendars = [cal1, cal2]
    await vm.loadCalendars()
    let baselineFullReads = api.fullReadCallCount

    // cal-1 unchanged, cal-2 changed (a third event landed there).
    api.seededAnswers["cal-1"] = .some(nil)
    api.seededAnswers["cal-2"] = .some([event("ev-b", calendarId: "cal-2"), event("ev-c", calendarId: "cal-2")])
    api.fullAnswers["cal-2"] = [event("ev-b", calendarId: "cal-2"), event("ev-c", calendarId: "cal-2")]
    await vm.refreshFromPush()

    #expect(api.seededCallCount == 2, "both calendars are consulted as change signals")
    #expect(api.fullReadCallCount == baselineFullReads + 2,
            "a changed calendar falls through to ONE authoritative full union read (both calendars)")
    #expect(Set(vm.events.map(\.id)) == ["ev-a", "ev-b", "ev-c"])
}
