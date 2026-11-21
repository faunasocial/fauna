import Testing
import Foundation
@testable import FaunaKit

// The other calendar writes (events.md § Errors & edge cases → *The other
// calendar writes*): `new-calendar-btn`, `create-calendar` and
// `calendar-import-button` paint the shared `calendar_write_possibility` and
// each action asks it again before it sends; the import's no-file refusal
// stays at the press; an unreadable calendar and a re-listing with none are
// said on `error-message`; a key lost at the act reads as "loading". tui's
// twin tests live in `apps/fauna-tui/src/events/`.

/// A double whose calendar read is scripted whole, so every answer of the
/// shared read can be put to the VM.
@MainActor
private final class ScriptedReadAPI: EventsAPI {
    var load = CalendarsLoad(read: .unresolved, calendars: [])
    /// The read a `createCalendar` leaves behind (the re-listing).
    var loadAfterCreate: CalendarsLoad?
    var createError: Error?
    private(set) var createdNames: [String] = []
    private(set) var importedTexts: [String] = []

    func listCalendars() async throws -> [FaunaCalendar] { load.calendars }
    func readCalendars() async throws -> CalendarsLoad { load }
    func createCalendar(name: String) async throws {
        if let createError { throw createError }
        createdNames.append(name)
        if let loadAfterCreate { load = loadAfterCreate }
    }
    func queryEvents(calendarId: String) async throws -> [EventSummary] { [] }
    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? { [] }
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
        importedTexts.append(icsText)
        return CalendarImportResult(imported: 1, skipped: 0, updated: nil, total: 1, errors: nil)
    }
}

private let personal = FaunaCalendar(id: "cal-1", name: "Personal")

private func loaded(_ cals: [FaunaCalendar], unreadable: UInt32 = 0) -> CalendarsLoad {
    CalendarsLoad(
        read: .loaded(calendars: cals.map {
            FfiCalendarRow(id: $0.id, name: $0.name, color: $0.color ?? "")
        }, unreadable: unreadable),
        calendars: cals)
}

private let provisionFailed = CalendarsLoad(
    read: .provisionFailed(error: "the nest refused the calendar"), calendars: [])

@MainActor
private func vm(over api: ScriptedReadAPI) async -> EventsVM {
    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    return vm
}

@MainActor
@Test("the three write doors paint the shared write predicate in each answer of the read")
func writeDoorsPaintTheSharedPredicate() async {
    let api = ScriptedReadAPI()
    let page = await vm(over: api)
    #expect(!page.calendarWriteGate.isPossible, "unresolved is a wait, never possible")
    #expect(page.calendarWriteGate == calendarWritePossibility(read: .unresolved))

    api.load = provisionFailed
    await page.loadCalendars()
    #expect(!page.calendarWriteGate.isPossible,
            "a refused Personal is not re-offered under another name")
    #expect(page.calendarWriteGate.reasonText?.isEmpty == false)

    api.load = loaded([personal])
    await page.loadCalendars()
    #expect(page.calendarWriteGate.isPossible)
    #expect(page.calendarWriteGate == page.createPossibility,
            "one reason for every write door while the answers coincide")
}

@MainActor
@Test("new-calendar-btn and create-calendar ask the predicate at the press and refuse with its reason")
func calendarCreateAsksAgainAtThePress() async {
    let api = ScriptedReadAPI()
    let page = await vm(over: api)
    let reason = calendarWritePossibility(read: .unresolved).reasonText

    page.toggleNewCalendar()
    #expect(!page.showNewCalendar, "the form does not open while the read is unresolved")
    #expect(page.errorMessage == reason)

    page.errorMessage = nil
    await page.createCalendar(name: "Work")
    #expect(api.createdNames.isEmpty, "a refused create never reaches the nest")
    #expect(page.errorMessage == reason)

    api.load = loaded([personal])
    await page.loadCalendars()
    page.errorMessage = nil
    page.toggleNewCalendar()
    #expect(page.showNewCalendar)
    page.toggleNewCalendar()
    #expect(!page.showNewCalendar, "an open form always closes")
}

@MainActor
@Test("a re-listing with no calendar after a create says the provision-failed sentence")
func emptyRelistingAfterCreateSaysProvisionFailed() async {
    let api = ScriptedReadAPI()
    api.load = loaded([personal])
    let page = await vm(over: api)
    page.toggleNewCalendar()
    api.loadAfterCreate = provisionFailed
    await page.createCalendar(name: "Work")
    #expect(api.createdNames == ["Work"])
    #expect(page.errorMessage == calendarWritePossibility(read: provisionFailed.read).reasonText)
    #expect(page.errorMessage?.isEmpty == false)
}

@MainActor
@Test("a key lost between the paint and the act refuses as loading, never as calendar-requires-mail")
func keyLostAtTheActReadsAsLoading() async {
    let api = ScriptedReadAPI()
    api.load = loaded([personal])
    let page = await vm(over: api)
    api.createError = FfiError.General(msg: L.errors.calendarRequiresMail)
    await page.createCalendar(name: "Work")
    #expect(page.errorMessage == calendarWritePossibility(read: .unresolved).reasonText)
    #expect(page.errorMessage != L.errors.calendarRequiresMail)
}

@MainActor
@Test("the import asks the predicate first; the no-file refusal stays at the press after it")
func importAsksThePredicateThenTheFile() async {
    let api = ScriptedReadAPI()
    let page = await vm(over: api)
    await page.importCalendarFile(atPath: "")
    #expect(page.errorMessage == calendarWritePossibility(read: .unresolved).reasonText,
            "while unresolved the predicate's reason, not the no-file sentence")

    api.load = loaded([personal])
    await page.loadCalendars()
    await page.selectCalendar(personal)
    await page.importCalendarFile(atPath: "")
    #expect(page.errorMessage == L.events.icsFileRequired)
    #expect(api.importedTexts.isEmpty)
}

@MainActor
@Test("unreadable calendars are left out and said on error-message once per change of the count")
func unreadableCalendarsAreSaidOncePerChange() async {
    let api = ScriptedReadAPI()
    api.load = loaded([personal], unreadable: 2)
    let page = await vm(over: api)
    #expect(page.calendars == [personal])
    #expect(page.errorMessage == L.events.calendarsUnreadable(count: "2"))

    page.errorMessage = nil
    await page.loadCalendars()
    #expect(page.errorMessage == nil, "the same count is not said again")

    api.load = loaded([personal], unreadable: 1)
    await page.loadCalendars()
    #expect(page.errorMessage == L.events.calendarsUnreadable(count: "1"))
}
