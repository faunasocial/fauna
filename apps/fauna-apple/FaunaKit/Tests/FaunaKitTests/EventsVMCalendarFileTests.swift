import Testing
import Foundation
@testable import FaunaKit

// The calendar-level `.ics` import / export contract (events.md § Import /
// Export): `calendar-import-button` with no file chosen says so on
// `error-message`; a real import says the imported/skipped counts; the export
// hands the shell one file's name + bytes and says where it landed. The
// guard, the counts sentence and the file naming are shared logic
// (`EventsVM`), so both apple shells get them from one place — the shells only
// own the picker and the save presentation.

/// Records what the import was handed and answers with scripted counts.
@MainActor
private final class ICSStubAPI: EventsAPI {
    var calendars: [FaunaCalendar] = []
    var exported = "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"
    var importResult = CalendarImportResult(imported: 2, skipped: 1, updated: nil, total: 3, errors: nil)
    private(set) var importedTexts: [String] = []
    private(set) var importedCalendarIds: [String] = []

    func listCalendars() async throws -> [FaunaCalendar] { calendars }
    func createCalendar(name: String) async throws {}
    func queryEvents(calendarId: String) async throws -> [EventSummary] { [] }
    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? { [] }
    func queryMyEvents(filter: String) async throws -> [EventSummary] { [] }
    func createEvent(_ request: CreateEventRequest) async throws {}
    func deleteEvent(id: String) async throws {}
    func inviteToEvent(eventId: String, email: String) async throws {}
    func rsvpEvent(eventId: String, response: RsvpResponse) async throws {}
    func setReminder(eventId: String, offset: String) async throws {}
    func removeReminder(eventId: String) async throws {}
    func getEvent(id: String) async throws -> EventDetail {
        EventDetail(id: id, uid: id, calendarId: "cal-1", summary: "",
                    dtstart: "", dtend: "", organizer: "", organizedByMe: true)
    }

    func importCalendar(calendarId: String, icsText: String) async throws -> CalendarImportResult {
        importedCalendarIds.append(calendarId)
        importedTexts.append(icsText)
        return importResult
    }

    func exportCalendar(calendarId: String) async throws -> String { exported }
}

/// A VM with one selected calendar named `name`, over a fresh stub.
@MainActor
private func selectedVM(_ name: String = "Work") async -> (EventsVM, ICSStubAPI) {
    let api = ICSStubAPI()
    let cal = FaunaCalendar(id: "cal-1", name: name)
    api.calendars = [cal]
    let vm = EventsVM()
    vm.configure(api: api)
    await vm.loadCalendars()
    await vm.selectCalendar(cal)
    return (vm, api)
}

private func writeTempICS(_ body: String) throws -> URL {
    let url = FileManager.default.temporaryDirectory
        .appendingPathComponent("ics-\(UUID().uuidString).ics")
    try body.write(to: url, atomically: true, encoding: .utf8)
    return url
}

@MainActor
@Test("import with no file chosen says so on error-message and never reaches the API")
func importWithNoFileSaysSo() async {
    let (vm, api) = await selectedVM()
    for blank in ["", "   ", "\n"] {
        vm.errorMessage = nil
        await vm.importCalendarFile(atPath: blank)
        #expect(vm.errorMessage == L.events.icsFileRequired,
                "a blank path (\(blank.debugDescription)) is the no-file guard, not a silent no-op")
    }
    #expect(api.importedTexts.isEmpty)
    #expect(vm.icsNotice == nil)
}

@MainActor
@Test("import reads the chosen file into the selected calendar and says the counts")
func importSaysTheCounts() async throws {
    let (vm, api) = await selectedVM()
    let body = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    let url = try writeTempICS(body)
    defer { try? FileManager.default.removeItem(at: url) }

    // A path with stray surrounding whitespace (a paste) still resolves.
    await vm.importCalendarFile(atPath: "  \(url.path)\n")

    #expect(api.importedTexts == [body])
    #expect(api.importedCalendarIds == ["cal-1"])
    #expect(vm.errorMessage == nil)
    #expect(vm.icsNotice == L.events.importResult(imported: "2", skipped: "1", total: "3"),
            "a partial import is an expected outcome — the counts, not an error")
}

@MainActor
@Test("import of a file that cannot be read lands on error-message, with no counts")
func importOfUnreadableFileErrors() async {
    let (vm, api) = await selectedVM()
    await vm.importCalendarFile(atPath: "/nonexistent/\(UUID().uuidString).ics")
    #expect(vm.errorMessage?.isEmpty == false)
    #expect(vm.icsNotice == nil)
    #expect(api.importedTexts.isEmpty)
}

@MainActor
@Test("an attempt replaces the previous attempt's outcome, error or notice")
func anAttemptClearsThePreviousOutcome() async throws {
    let (vm, _) = await selectedVM()
    await vm.importCalendarFile(atPath: "")
    #expect(vm.errorMessage == L.events.icsFileRequired)

    let url = try writeTempICS("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
    defer { try? FileManager.default.removeItem(at: url) }
    await vm.importCalendarFile(atPath: url.path)
    #expect(vm.errorMessage == nil, "the stale no-file error must not outlive a good import")
    #expect(vm.icsNotice != nil)

    await vm.importCalendarFile(atPath: "")
    #expect(vm.icsNotice == nil, "the stale counts must not outlive a refused attempt")
    #expect(vm.errorMessage == L.events.icsFileRequired)
}

@MainActor
@Test("export hands back one .ics named for the calendar, and the shell says where it landed")
func exportNamesTheFile() async {
    let (vm, api) = await selectedVM("Work")
    api.exported = "BEGIN:VCALENDAR\r\nSUMMARY:Standup\r\nEND:VCALENDAR\r\n"

    let file = await vm.exportCalendarFile()
    #expect(file?.fileName == "Work.ics")
    #expect(file.map { String(decoding: $0.data, as: UTF8.self) } == api.exported)
    #expect(vm.icsNotice == nil, "nothing is written yet, so nothing is claimed")

    vm.calendarExported(to: "/Users/u/Downloads/Work.ics")
    #expect(vm.icsNotice == L.events.calendarExported(path: "/Users/u/Downloads/Work.ics"))
}

@MainActor
@Test("a calendar name that is not a safe file name still exports as one .ics")
func exportFileNameIsSafe() async {
    let (vm, _) = await selectedVM("Home / Away: 2026")
    let file = await vm.exportCalendarFile()
    let name = file?.fileName ?? ""
    #expect(name.hasSuffix(".ics"))
    #expect(!name.contains("/") && !name.contains(":"),
            "a path separator in a calendar name must not escape the downloads directory: \(name)")

    let (blank, _) = await selectedVM("  ")
    #expect(await blank.exportCalendarFile()?.fileName == "calendar.ics")
}

@MainActor
@Test("the notice belongs to the selected calendar and the account: switching or resetting drops it")
func noticeIsScoped() async throws {
    let (vm, api) = await selectedVM()
    let url = try writeTempICS("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
    defer { try? FileManager.default.removeItem(at: url) }
    await vm.importCalendarFile(atPath: url.path)
    #expect(vm.icsNotice != nil)

    await vm.selectCalendar(FaunaCalendar(id: "cal-2", name: "Other"))
    #expect(vm.icsNotice == nil, "the counts described the previous calendar")

    await vm.importCalendarFile(atPath: url.path)
    vm.icsImportPath = url.path
    #expect(vm.icsNotice != nil)
    vm.reset()
    #expect(vm.icsNotice == nil, "an account switch must not leave the outgoing account's line on screen")
    #expect(vm.icsImportPath.isEmpty, "…nor the outgoing account's typed path")
    _ = api
}

// MARK: - The e2e downloads write never overwrites

@Test("a second write of the same name lands beside the first, never over it")
func nonOverwritingWriteKeepsTheFirst() throws {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("ics-dl-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: dir) }

    let first = try #require(SnapshotFileSaver.writeNonOverwriting(
        into: dir, suggestedFileName: "Work.ics", data: Data("one".utf8)))
    let second = try #require(SnapshotFileSaver.writeNonOverwriting(
        into: dir, suggestedFileName: "Work.ics", data: Data("two".utf8)))
    let third = try #require(SnapshotFileSaver.writeNonOverwriting(
        into: dir, suggestedFileName: "Work.ics", data: Data("three".utf8)))

    #expect(first.lastPathComponent == "Work.ics")
    #expect(Set([first, second, third].map(\.lastPathComponent)).count == 3, "three distinct files")
    #expect(second.pathExtension == "ics" && third.pathExtension == "ics",
            "the extension survives, so a `.ics` glob still finds every export")
    #expect(try String(contentsOf: first, encoding: .utf8) == "one", "the earlier export is untouched")
    #expect(try String(contentsOf: second, encoding: .utf8) == "two")
    #expect(try String(contentsOf: third, encoding: .utf8) == "three")
}
