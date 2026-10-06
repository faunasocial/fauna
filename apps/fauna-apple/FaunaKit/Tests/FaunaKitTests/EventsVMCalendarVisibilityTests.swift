import Testing
import Foundation
@testable import FaunaKit

// `EventsVM`'s `calendar-visibility` per-calendar show/hide display filter
// (events.md § Where logic lives → *Which calendars display*): the shared
// `calendar_is_displayed`/`resolve_calendar_selection` composition, consumed
// via UniFFI exactly like tui/linux/web/android. These pin the two edge cases
// that drifted apart across the pre-lift per-app copies (events.md's own
// history): the empty-visible-set-means-union rule, and seeding a brand-new
// calendar visible on every path that replaces `calendars`.

/// A minimal `EventsAPI` stub — these tests are about the visibility
/// composition, not publish ordering (see `EventsVMPublishOrderingTests`), so
/// `queryEvents` just answers per-calendar from a fixed map.
@MainActor
private final class StubEventsAPI: EventsAPI {
    var calendars: [FaunaCalendar] = []
    /// Per-calendar canned events, keyed by calendar id.
    var eventsByCalendar: [String: [EventSummary]] = [:]
    private(set) var queryEventsCallCount = 0

    func listCalendars() async throws -> [FaunaCalendar] { calendars }
    func createCalendar(name: String) async throws {}

    func queryEvents(calendarId: String) async throws -> [EventSummary] {
        queryEventsCallCount += 1
        return eventsByCalendar[calendarId] ?? []
    }

    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? {
        try await queryEvents(calendarId: calendarId)
    }

    func queryMyEvents(filter: String) async throws -> [EventSummary] { [] }
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
    EventSummary(id: id, uid: id, summary: id,
                 dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00",
                 calendarId: calendarId)
}

@MainActor
@Test("a brand-new calendar seeds visible; an existing one keeps the user's choice")
func seedingKeepsChoiceAndAddsNewCalendarsVisible() async {
    let api = StubEventsAPI()
    let cal1 = FaunaCalendar(id: "cal-1", name: "One")
    let cal2 = FaunaCalendar(id: "cal-2", name: "Two")
    api.calendars = [cal1, cal2]
    api.eventsByCalendar = ["cal-1": [event("ev-a", calendarId: "cal-1")],
                            "cal-2": [event("ev-b", calendarId: "cal-2")]]

    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    #expect(vm.visibleCalendarIds == ["cal-1", "cal-2"], "first load seeds every calendar visible")
    #expect(vm.events.map(\.id).sorted() == ["ev-a", "ev-b"])

    // The user hides cal-1.
    vm.toggleCalendarVisibility(calendarId: "cal-1")
    #expect(vm.events.map(\.id) == ["ev-b"])

    // A brand-new calendar (cal-3) appears; cal-1's OFF choice must survive,
    // and cal-3 must start visible rather than needing a second toggle.
    let cal3 = FaunaCalendar(id: "cal-3", name: "Three")
    api.calendars = [cal1, cal2, cal3]
    api.eventsByCalendar["cal-3"] = [event("ev-c", calendarId: "cal-3")]
    await vm.loadCalendars()

    #expect(vm.visibleCalendarIds == ["cal-2", "cal-3"],
            "cal-1 stays hidden (user's own choice); cal-3 is seeded visible")
    #expect(vm.events.map(\.id).sorted() == ["ev-b", "ev-c"])
}

@MainActor
@Test("unchecking every box empties the visible set, which the shared predicate reads as the union")
func emptyVisibleSetIsTheFullUnion() async {
    let api = StubEventsAPI()
    let cal1 = FaunaCalendar(id: "cal-1", name: "One")
    let cal2 = FaunaCalendar(id: "cal-2", name: "Two")
    api.calendars = [cal1, cal2]
    api.eventsByCalendar = ["cal-1": [event("ev-a", calendarId: "cal-1")],
                            "cal-2": [event("ev-b", calendarId: "cal-2")]]

    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()

    vm.toggleCalendarVisibility(calendarId: "cal-1")
    vm.toggleCalendarVisibility(calendarId: "cal-2")
    #expect(vm.visibleCalendarIds.isEmpty)
    #expect(vm.events.map(\.id).sorted() == ["ev-a", "ev-b"],
            "an EMPTY visible set means \"no filter\" (the full union), never \"hide everything\"")
}

@MainActor
@Test("a live calendar-item selection ignores calendar-visibility entirely")
func liveSelectionIgnoresVisibilityToggle() async {
    let api = StubEventsAPI()
    let cal1 = FaunaCalendar(id: "cal-1", name: "One")
    api.calendars = [cal1]
    api.eventsByCalendar = ["cal-1": [event("ev-a", calendarId: "cal-1")]]

    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    await vm.selectCalendar(cal1)

    // Hiding the SELECTED calendar must not empty the page: selecting it wins
    // outright over the display filter (events.md § Where logic lives).
    vm.toggleCalendarVisibility(calendarId: "cal-1")
    #expect(vm.events.map(\.id) == ["ev-a"])
}

@MainActor
@Test("toggling calendar-visibility is purely local — no refetch")
func toggleDoesNotRefetch() async {
    let api = StubEventsAPI()
    let cal1 = FaunaCalendar(id: "cal-1", name: "One")
    let cal2 = FaunaCalendar(id: "cal-2", name: "Two")
    api.calendars = [cal1, cal2]
    api.eventsByCalendar = ["cal-1": [event("ev-a", calendarId: "cal-1")],
                            "cal-2": [event("ev-b", calendarId: "cal-2")]]

    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    let callsAfterLoad = api.queryEventsCallCount

    vm.toggleCalendarVisibility(calendarId: "cal-1")
    vm.toggleCalendarVisibility(calendarId: "cal-1")

    #expect(api.queryEventsCallCount == callsAfterLoad,
            "the already-fetched union is re-filtered locally, never re-queried")
}
