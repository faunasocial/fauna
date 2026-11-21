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
    /// When set, `createEvent` throws it (a failed save).
    var createError: Error?

    func listCalendars() async throws -> [FaunaCalendar] { calendars }
    func createCalendar(name: String) async throws {}
    func queryEvents(calendarId: String) async throws -> [EventSummary] { [] }
    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? { [] }
    func queryMyEvents(filter: String) async throws -> [EventSummary] { [] }

    func createEvent(_ request: CreateEventRequest) async throws {
        if let createError { throw createError }
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
@Test("no calendar to hold an event: the submit is refused with the predicate's reason, never silent")
func createWithNoSelectionAndNoCalendarsIsRefusedWithItsReason() async {
    let api = RecordingEventsAPI()
    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    #expect(vm.calendars.isEmpty)

    let created = await vm.createEvent(CreateEventRequest(
        calendarId: "", summary: "Standup",
        dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00"))

    #expect(!created)
    #expect(api.createEventCalendarIds.isEmpty,
            "with no resolvable calendar, createEvent must not call the API at all")
    #expect(!(vm.createFormError ?? "").isEmpty,
            "the refusal is the form's own sentence, not a silent return")
}

@MainActor
@Test("every door to the compose is shut while no calendar can hold an event")
func composeDoorsAreGatedOnTheSharedPredicate() async {
    let api = RecordingEventsAPI()
    let vm = EventsVM()
    vm.configure(api: api)
    // Before the first read the answer is the loading reason, not possible.
    #expect(vm.calendarsLoading)
    vm.toggleNewEvent()
    #expect(!vm.showNewEvent)

    await vm.loadCalendars()
    #expect(!vm.createPossibility.isPossible)
    vm.toggleNewEvent()
    vm.beginCompose(onDay: Date())
    vm.beginCompose(atSlot: Date())
    #expect(!vm.showNewEvent, "no door opens a compose that cannot succeed")
    #expect(vm.createPossibility.reasonText != nil,
            "the empty state says why, and names no step (ruling 5)")

    api.calendars = [FaunaCalendar(id: "cal-1", name: "One")]
    await vm.loadCalendars()
    #expect(vm.createPossibility.isPossible)
    vm.beginCompose(atSlot: Date())
    #expect(vm.showNewEvent)
}

@MainActor
@Test("a failed save keeps the compose open and puts the whole sentence in the form")
func failedSaveStaysInTheForm() async {
    struct Boom: Error, LocalizedError { var errorDescription: String? { "boom" } }
    let api = RecordingEventsAPI()
    api.calendars = [FaunaCalendar(id: "cal-1", name: "One")]
    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    vm.toggleNewEvent()
    #expect(vm.showNewEvent)

    api.createError = Boom()
    let created = await vm.createEvent(CreateEventRequest(
        calendarId: "", summary: "Standup",
        dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00"))

    #expect(!created)
    #expect(vm.showNewEvent, "the form stays open with what was typed")
    #expect(vm.createFormError?.contains("could not be saved") == true)
    #expect(vm.errorMessage == nil, "the sentence is the form's, not the page banner's")

    api.createError = nil
    let retried = await vm.createEvent(CreateEventRequest(
        calendarId: "", summary: "Standup",
        dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00"))
    #expect(retried)
    #expect(vm.createFormError == nil)
}
