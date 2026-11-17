import SwiftUI

/// The calendar list's empty state while no calendar can hold an event
/// (events.md § Errors & edge cases → *No calendar to hold an event*, rulings 2
/// and 3), shared by both apple targets.
///
/// Everything is the shared Rust's answer, never re-derived here: the reason is
/// `eventCreatePossibility`'s, the jump is `eventCreateStep`'s. While the read
/// is still unresolved the page paints its loading line (un-id'd — the empty
/// state is never painted for it); a loaded read paints nothing.
public struct CalendarListEmptyState: View {
    let vm: EventsVM
    /// Takes this person to the place `step` is taken (the host owns the
    /// navigation: Settings → Mail & Calendar, or the admin Calendar page).
    let onStep: (FfiEventCreateStep) -> Void

    public init(vm: EventsVM, onStep: @escaping (FfiEventCreateStep) -> Void) {
        self.vm = vm
        self.onStep = onStep
    }

    public var body: some View {
        if vm.calendarsLoading {
            Text(L.events.createBlocked.loading)
                .font(.caption)
                .foregroundStyle(.secondary)
        } else if case .loaded = vm.calendarsRead {
            EmptyView()
        } else {
            let reason = vm.createPossibility.reasonText ?? L.events.createBlocked.notHere
            VStack(alignment: .leading, spacing: 6) {
                automationText(Ids.calendarListEmptyState, reason)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let step = vm.createStep, Self.offers(step) {
                    let label = step == .addCredential
                        ? L.events.createBlocked.openMailSettings
                        : L.events.createBlocked.openAdminCalendar
                    Button(label) { onStep(step) }
                        .controlSize(.small)
                        .accessibilityIdentifier(Ids.calendarListEmptyStateStep)
                        .automationActivate(Ids.calendarListEmptyStateStep) { onStep(step) }
                }
            }
        }
    }

    /// Whether this flavor can take `step` at all: the kids flavor excises
    /// both places a step leads to (Settings → Mail & Calendar and the admin
    /// Calendar page), so it offers neither jump (ui README § Offer only what
    /// is possible) — the reason line still stands.
    static func offers(_ step: FfiEventCreateStep) -> Bool {
        #if FAUNA_KIDS
        return false
        #else
        return true
        #endif
    }
}
