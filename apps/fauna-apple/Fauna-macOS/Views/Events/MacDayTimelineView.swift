import SwiftUI
import FaunaKit

/// macOS day view: navigation chrome (prev/next/today + date label) over the
/// shared `WeekDayTimeGrid` rendered as a single column (priority #2 — the
/// "same day-column renderer" the contract calls for). The displayed date is the
/// VM's `currentDate`, so the month-grid single-click drill-in lands here.
struct MacDayTimelineView: View {
    let vm: EventsVM

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button { vm.previousDay() } label: { Image(systemName: "chevron.left") }
                    .buttonStyle(.plain)

                Spacer()
                Text(dateLabel).font(.headline)
                Spacer()

                Button(L.common.today) { vm.goToToday() }
                    .controlSize(.small)

                Button { vm.nextDay() } label: { Image(systemName: "chevron.right") }
                    .buttonStyle(.plain)
            }
            .padding(.horizontal)
            .padding(.vertical, 8)

            Divider()

            WeekDayTimeGrid(
                days: [vm.currentDate],
                events: vm.events,
                containerId: Ids.calendarDayTimeline,
                onSelectEvent: { event in Task { await vm.selectEvent(event) } },
                onEmptySlot: { date in vm.beginCompose(atSlot: date) }
            )
        }
    }

    private var dateLabel: String {
        let fmt = DateFormatter()
        fmt.dateStyle = .full
        return fmt.string(from: vm.currentDate)
    }
}
