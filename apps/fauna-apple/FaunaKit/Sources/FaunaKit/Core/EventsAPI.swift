import Foundation

/// The calendar/event slice of `APIClient` that `EventsVM` consumes — the seam
/// that makes the view model's own logic testable without a nest.
///
/// **Why a protocol and not the concrete client.** `EventsVM`'s hardest bugs are
/// not in what it fetches but in the ORDER it publishes: two `events` re-queries
/// are routinely in flight at once (a foreground create/select and the 10 s poll
/// or a `fauna.calendar.changed` push), they finish out of order, and the wrong
/// winner puts a stale list on a page the user has just changed. That is pure
/// mechanism — no nest, no UI — yet with a concrete `APIClient` the only
/// instrument for it was a multi-minute tier_3 e2e run whose race window opens
/// only under machine load (measured: ~35% under contention, 0/5 on a quiet
/// machine). Testing the mile of mechanism headlessly and leaving e2e the last
/// inch of wiring is the split this seam buys.
///
/// Deliberately scoped to the events surface rather than all ~3300 lines of
/// `APIClient`: the ask is one worked example, not a fleet-wide VM refactor.
/// Extend it method-by-method as other view models need the same treatment.
///
/// `AnyObject` because the VM holds it as a stored optional reference, exactly as
/// it held `APIClient?` before.
public protocol EventsAPI: AnyObject {
    func listCalendars() async throws -> [FaunaCalendar]
    /// The shared four-answer calendar read (defaulted below for doubles).
    func readCalendars() async throws -> CalendarsLoad
    /// Read only on a *no key* answer: does the deployment serve calendar
    /// storage or mail (`SetupStatusReply::serves_dav_or_mail`)?
    func deploymentServesDavOrMail() async -> Bool
    func amIAdmin() async throws -> Bool
    func createCalendar(name: String) async throws
    func queryEvents(calendarId: String) async throws -> [EventSummary]
    /// `queryEvents`'s COST-saving twin for the backstop poll (events.md §
    /// Implementation status today — the delta-sync backstop). `nil` means
    /// "unchanged since the last call through this same `EventsAPI`
    /// instance" — the caller must not touch its model.
    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]?
    func queryMyEvents(filter: String) async throws -> [EventSummary]
    func createEvent(_ request: CreateEventRequest) async throws
    func getEvent(id: String) async throws -> EventDetail
    func deleteEvent(id: String) async throws
    func inviteToEvent(eventId: String, email: String) async throws
    func rsvpEvent(eventId: String, response: RsvpResponse) async throws
    func setReminder(eventId: String, offset: String) async throws
    func removeReminder(eventId: String) async throws
    func importCalendar(calendarId: String, icsText: String) async throws -> CalendarImportResult
    func exportCalendar(calendarId: String) async throws -> String
    /// The refused-change list (`ui/events.md` § Refused scheduling changes).
    /// Defaulted below so the test doubles that stub only the calendar surface
    /// keep compiling; `APIClient` supplies the real FFI-backed pair.
    func refusedChangesOpen() async -> [FfiRefusedChange]
    @discardableResult
    func refusedChangeDismiss(key: String) async throws -> Bool
}

/// The Events page's calendar read in the shared four answers
/// (`FfiCalendarsRead`: unresolved / no key / provision failed / loaded)
/// beside the calendars it carries, as the page's own rows.
public struct CalendarsLoad: Equatable {
    public var read: FfiCalendarsRead
    public var calendars: [FaunaCalendar]

    public init(read: FfiCalendarsRead, calendars: [FaunaCalendar]) {
        self.read = read
        self.calendars = calendars
    }
}

extension EventsAPI {
    /// Test doubles that stub only `listCalendars` read as the folded list: an
    /// empty list is a *no key* answer, anything else *loaded*. `APIClient`
    /// supplies the real four-answer read.
    public func readCalendars() async throws -> CalendarsLoad {
        let cals = try await listCalendars()
        if cals.isEmpty { return CalendarsLoad(read: .noKey, calendars: []) }
        return CalendarsLoad(
            read: .loaded(calendars: cals.map {
                FfiCalendarRow(id: $0.id, name: $0.name, color: $0.color ?? "")
            }),
            calendars: cals)
    }
    public func deploymentServesDavOrMail() async -> Bool { true }
    public func amIAdmin() async throws -> Bool { false }
    public func refusedChangesOpen() async -> [FfiRefusedChange] { [] }
    @discardableResult
    public func refusedChangeDismiss(key: String) async throws -> Bool { false }
}

/// The production conformance is empty on purpose: every requirement above is
/// already a `public func` on `APIClient` with the identical signature, so the
/// seam adds no indirection layer to maintain — it only names the surface.
extension APIClient: EventsAPI {}
