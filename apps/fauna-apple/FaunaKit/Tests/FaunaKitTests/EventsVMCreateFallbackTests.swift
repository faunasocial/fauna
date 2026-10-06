import Testing
import Foundation
@testable import FaunaKit

// `EventsVM.createEvent`'s "no calendar selected → fall back to
// `calendars.first`" branch (events.md; matches linux/windows — the week/day/
// month grids create without a prior select). e2e coverage of this branch was
// deliberately given up: on macOS the unselected agenda is a
// virtualizing `List` whose registered row count saturates at what the pane
// fits, so a create made from that state is unobservable through the UI no
// matter how it's asserted (see `test_switch_to_week_view`'s own comment).
// The branch itself is pure VM logic with no rendering involved, so it is
// pinned here at tier_1 instead (testing.md — split the mile) — this was the
// residual left open.

/// A minimal `EventsAPI` stub that records the `calendarId` its `createEvent`
/// call actually received, so a test can tell "resolved to the first
/// calendar" apart from "passed the form's own (possibly empty) value".
@MainActor
private final class RecordingEventsAPI: EventsAPI {
    var calendars: [FaunaCalendar] = []
    private(set) var createEventCalendarIds: [String] = []

    func listCalendars() async throws -> [FaunaCalendar] { calendars }
    func createCalendar(name: String) async throws {}
    func queryEvents(calendarId: String) async throws -> [EventSummary] { [] }
    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? { [] }
    func queryMyEvents(filter: String) async throws -> [EventSummary] { [] }

    func createEvent(_ request: CreateEventRequest) async throws {
        createEventCalendarIds.append(request.calendarId)
    }

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

@MainActor
@Test("creating with nothing selected falls back to the first calendar, and leaves the union view scoped")
func createWithNoSelectionFallsBackToFirstCalendar() async {
    let api = RecordingEventsAPI()
    let cal1 = FaunaCalendar(id: "cal-1", name: "One")
    let cal2 = FaunaCalendar(id: "cal-2", name: "Two")
    api.calendars = [cal1, cal2]

    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    #expect(vm.selectedCalendar == nil, "loadCalendars must not select a calendar on its own")

    // The form submits an empty calendarId when the page is browsing the union
    // (no prior select) — the real production shape this branch resolves.
    await vm.createEvent(CreateEventRequest(calendarId: "", summary: "Standup",
                                             dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00"))

    #expect(api.createEventCalendarIds == ["cal-1"],
            "with nothing selected, the create must resolve to calendars.first")
    #expect(vm.selectedCalendar == nil,
            "the fallback resolution must stay local — it must never assign selectedCalendar")
    #expect(vm.errorMessage == nil)
}

@MainActor
@Test("creating with nothing selected and no calendars at all is a no-op")
func createWithNoSelectionAndNoCalendarsDoesNothing() async {
    let api = RecordingEventsAPI()
    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    #expect(vm.calendars.isEmpty)

    await vm.createEvent(CreateEventRequest(calendarId: "", summary: "Standup",
                                             dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00"))

    #expect(api.createEventCalendarIds.isEmpty,
            "with no resolvable calendar, createEvent must not call the API at all")
}
