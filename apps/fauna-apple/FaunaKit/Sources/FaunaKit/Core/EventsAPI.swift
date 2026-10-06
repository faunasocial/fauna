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
}

/// The production conformance is empty on purpose: every requirement above is
/// already a `public func` on `APIClient` with the identical signature, so the
/// seam adds no indirection layer to maintain — it only names the surface.
extension APIClient: EventsAPI {}
