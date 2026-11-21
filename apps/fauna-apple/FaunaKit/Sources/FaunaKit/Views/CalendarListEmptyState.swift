import SwiftUI

/// The calendar list's empty state while no calendar can hold an event
/// (events.md § Errors & edge cases → *No calendar to hold an event*, rulings 2
/// and 3), shared by both apple targets.
///
/// Everything is the shared Rust's answer, never re-derived here: the reason is
/// `eventCreatePossibility`'s. While the read is still unresolved — a missing
/// key included, the mint in flight (ruling 5) — the page paints its loading
/// line (un-id'd — the empty state is never painted for it); a loaded read
/// paints nothing; a refused provision paints its reason, with no step (the
/// next read retries it).
public struct CalendarListEmptyState: View {
    let vm: EventsVM

    public init(vm: EventsVM) {
        self.vm = vm
    }

    public var body: some View {
        if vm.calendarsLoading {
            Text(L.events.createBlocked.loading)
                .font(.caption)
                .foregroundStyle(.secondary)
        } else if case .loaded = vm.calendarsRead {
            EmptyView()
        } else if let reason = vm.createPossibility.reasonText {
            automationText(Ids.calendarListEmptyState, reason)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}
