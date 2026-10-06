import SwiftUI
import FaunaKit

/// macOS month view over the shared `MonthGridView` (priority #1/#4 — mirrors the
/// month grid every other app renders; events.md § Layout & flow lists it as a
/// main-area view for all 7 apps). Month prev/next + the `calendar-date-label`
/// live in the `MacEventListView` toolbar (driving `vm.currentDate`); this is the
/// grid body. Single-click a day → Day view (`onDrillToDay`); double-click an
/// empty day → new-event prefilled.
struct MacMonthView: View {
    let vm: EventsVM
    /// Switch the Events page to Day view for `date` (the parent owns view mode).
    let onDrillToDay: (Date) -> Void

    var body: some View {
        MonthGridView(
            month: vm.currentDate,
            events: vm.events,
            onSelectDay: onDrillToDay,
            onNewEventOnDay: { date in vm.beginCompose(onDay: date) },
            onSelectEvent: { event in Task { await vm.selectEvent(event) } }
        )
    }
}
