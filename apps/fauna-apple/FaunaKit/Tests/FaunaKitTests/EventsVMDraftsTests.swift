import Testing
import Foundation
@testable import FaunaKit

// The events-rail leg of draft-persistence v2 (`docs/goal/behavior/
// reserved-folders.md` § Drafts Sync; `docs/goal/ui/events.md` § Persistence)
// — the local VM logic that is unique to this leg, at the level
// `EventsVMCreateFallbackTests` already pins `EventsVM` at: a scripted
// `EventsAPI`, no live nest, no notification centre.
//
// What this does NOT cover: the actual `FfiEventDraftsSync` round trip
// (restore/save over the wire) — that is Rust-tested in
// `libs/fauna-ffi/src/event_drafts.rs` and proven end to end by
// `tests/e2e-unified/tests/test_event_draft_persistence.py`. `FfiEventDraftsSync`
// is a UniFFI-generated Object type, not one of FaunaKit's own subclassable
// Swift classes (unlike `APIClient`), so there is no `Recording` double for
// it the way `BackupDestinationsVMTests`' `RecordingAPI` stands in for
// `APIClient`. What IS testable without one — and is exactly the logic this
// leg adds over the manager-backed rails — is that `resumableDraft` updates
// and clears at the right points: every mutator below runs its
// `resumableDraft` bookkeeping unconditionally, before the
// `eventDraftsSync`-gated network half, so a VM with no rail attached still
// exercises it faithfully.

@MainActor
private final class StubEventsAPI: EventsAPI {
    var calendars: [FaunaCalendar] = []
    var createEventError: Error?
    private(set) var createEventCalls = 0

    func listCalendars() async throws -> [FaunaCalendar] { calendars }
    func createCalendar(name: String) async throws {}
    func queryEvents(calendarId: String) async throws -> [EventSummary] { [] }
    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? { [] }
    func queryMyEvents(filter: String) async throws -> [EventSummary] { [] }

    func createEvent(_ request: CreateEventRequest) async throws {
        createEventCalls += 1
        if let createEventError { throw createEventError }
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

private struct StubCreateError: Error {}

private func sampleDraft(_ summary: String = "Quarterly walrus review") -> FfiEventDrafts {
    FfiEventDrafts(summary: summary, dtstart: "2026-09-01T09:00", dtend: "2026-09-01T10:00",
                    description: "bring the herring numbers", location: "Room 3")
}

@MainActor
@Test("scheduleDraftsSave updates resumableDraft immediately, with no rail attached")
func scheduleDraftsSaveUpdatesResumableDraftImmediately() async {
    let vm = EventsVM()
    #expect(vm.resumableDraft == nil)

    vm.scheduleDraftsSave(sampleDraft())

    #expect(vm.resumableDraft == sampleDraft())
}

@MainActor
@Test("a day-cell gesture clears the rail — beginCompose(onDay:)")
func dayCellGestureClearsTheDraft() async {
    let vm = EventsVM()
    vm.scheduleDraftsSave(sampleDraft())
    #expect(vm.resumableDraft != nil)

    vm.beginCompose(onDay: Date())

    #expect(vm.resumableDraft == nil,
            "a day-cell gesture starts fresh (events.md § Persistence) — it must not resume a stale draft")
    #expect(vm.showNewEvent, "the compose must still open")
}

@MainActor
@Test("a day-cell slot gesture clears the rail — beginCompose(atSlot:)")
func dayCellSlotGestureClearsTheDraft() async {
    let vm = EventsVM()
    vm.scheduleDraftsSave(sampleDraft())

    vm.beginCompose(atSlot: Date())

    #expect(vm.resumableDraft == nil)
}

@MainActor
@Test("a successful create clears the rail, on the success path")
func successfulCreateClearsTheDraft() async {
    let api = StubEventsAPI()
    api.calendars = [FaunaCalendar(id: "cal-1", name: "One")]
    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    vm.scheduleDraftsSave(sampleDraft())

    await vm.createEvent(CreateEventRequest(calendarId: "cal-1", summary: "Standup",
                                             dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00"))

    #expect(api.createEventCalls == 1)
    #expect(vm.resumableDraft == nil,
            "a successful create must clear the rail (events.md § Persistence)")
}

@MainActor
@Test("a FAILED create leaves the draft intact, for the user to retry")
func failedCreateLeavesTheDraftIntact() async {
    let api = StubEventsAPI()
    api.calendars = [FaunaCalendar(id: "cal-1", name: "One")]
    api.createEventError = StubCreateError()
    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    let draft = sampleDraft()
    vm.scheduleDraftsSave(draft)

    await vm.createEvent(CreateEventRequest(calendarId: "cal-1", summary: "Standup",
                                             dtstart: "2026-08-07T14:00", dtend: "2026-08-07T15:00"))

    #expect(api.createEventCalls == 1)
    #expect(vm.resumableDraft == draft,
            "a failed create must NEVER clear the rail — the compose stays open for a retry (events.md § Persistence: clear on the success path, never at the submit click)")
}

@MainActor
@Test("attachEventDrafts(nil) resets the rail and does not crash")
func attachEventDraftsNilResetsTheRail() async {
    let vm = EventsVM()
    vm.scheduleDraftsSave(sampleDraft())
    #expect(vm.resumableDraft != nil)

    vm.attachEventDrafts(nil)

    #expect(vm.resumableDraft == nil,
            "the identity seam clears the rail for the INCOMING actor even with no sync handle (a build failure), rather than leaving the outgoing actor's draft resumable")
}

@MainActor
@Test("re-attaching clears any draft left by a superseded session (the identity seam)")
func reattachingClearsASupersededDraft() async {
    let vm = EventsVM()
    vm.scheduleDraftsSave(sampleDraft("outgoing actor's draft"))
    #expect(vm.resumableDraft != nil)

    // A second `attachEventDrafts` call — e.g. an account switch — must never
    // let the outgoing actor's in-memory draft read as resumable for the
    // incoming one, even before any restore has landed.
    vm.attachEventDrafts(nil)

    #expect(vm.resumableDraft == nil)
}
