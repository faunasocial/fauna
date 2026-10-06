import SwiftUI
import FaunaKit

/// iOS day view: navigation chrome over the shared `WeekDayTimeGrid` rendered as
/// a single column (priority #2). The displayed date is the VM's `currentDate`,
/// so the month-grid single-click drill-in lands here.
struct DayTimelineView: View {
    let vm: EventsVM

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button { vm.previousDay() } label: {
                    Image(systemName: "chevron.left").font(.body).padding(8)
                }
                .buttonStyle(.plain)

                Spacer()
                Text(dayLabel).font(.headline)
                Spacer()

                Button(L.common.today) { vm.goToToday() }
                    .font(.subheadline)
                    .padding(.trailing, 4)

                Button { vm.nextDay() } label: {
                    Image(systemName: "chevron.right").font(.body).padding(8)
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 8)
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

    private var dayLabel: String {
        let fmt = DateFormatter()
        fmt.dateFormat = "EEEE, MMM d"
        return fmt.string(from: vm.currentDate)
    }
}
