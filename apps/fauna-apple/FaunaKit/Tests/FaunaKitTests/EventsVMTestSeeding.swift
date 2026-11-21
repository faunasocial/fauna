import Foundation
@testable import FaunaKit

extension EventsVM {
    /// Seed the VM as a landed calendar read would leave it: the list AND the
    /// read's shared answer (`calendarsRead`), never the list alone.
    ///
    /// Tests that start from "the page already shows these calendars" must use
    /// this rather than assigning `calendars` by hand. Every compose door and
    /// `createEvent` ask the shared create predicate, which reads
    /// `calendarsRead` (events.md § *No calendar to hold an event*, ruling 3),
    /// and the poll re-runs the whole read while it is not `.loaded`. A VM
    /// holding a list under an `.unresolved` read is a state production never
    /// reaches: a create is refused, so a test waiting on its re-query spins
    /// forever on the main actor, and a poll tick takes the re-read branch
    /// instead of the diff under test.
    func seedLoadedCalendars(_ list: [FaunaCalendar]) {
        calendars = list
        calendarsRead = list.isEmpty
            ? .provisionFailed(error: "the Personal calendar was not listed")
            : .loaded(calendars: list.map {
                FfiCalendarRow(id: $0.id, name: $0.name, color: $0.color ?? "")
            }, unreadable: 0)
    }
}
