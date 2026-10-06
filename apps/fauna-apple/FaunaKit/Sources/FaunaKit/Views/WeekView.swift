import SwiftUI

/// Week view: navigation chrome (prev/next/today + range label) over the shared
/// `WeekDayTimeGrid` (priority #2 — same grid on both platforms + the day view).
///
/// Lifted 2026-09-02 from
/// byte-for-byte-identical `WeekView`/`MacWeekView` per-app copies. The only
/// real divergence — iOS's larger chevron tap targets and Today-button style —
/// is handled inline via `#if os(iOS)`, the same per-platform-styling pattern
/// already used elsewhere in this file's siblings (e.g. `AccountSettingsView`).
public struct WeekView: View {
    let vm: EventsVM

    @State private var weekStart: Date = Calendar.current.startOfWeek(for: .now)

    public init(vm: EventsVM) {
        self.vm = vm
    }

    public var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button {
                    weekStart = Calendar.current.date(byAdding: .weekOfYear, value: -1, to: weekStart) ?? weekStart
                } label: {
                    #if os(iOS)
                    Image(systemName: "chevron.left").font(.body).padding(8)
                    #else
                    Image(systemName: "chevron.left")
                    #endif
                }
                .buttonStyle(.plain)

                Spacer()
                Text(weekLabel).font(.headline)
                Spacer()

                Button(L.common.today) {
                    weekStart = Calendar.current.startOfWeek(for: .now)
                }
                #if os(iOS)
                .font(.subheadline)
                .padding(.trailing, 4)
                #else
                .controlSize(.small)
                #endif

                Button {
                    weekStart = Calendar.current.date(byAdding: .weekOfYear, value: 1, to: weekStart) ?? weekStart
                } label: {
                    #if os(iOS)
                    Image(systemName: "chevron.right").font(.body).padding(8)
                    #else
                    Image(systemName: "chevron.right")
                    #endif
                }
                .buttonStyle(.plain)
            }
            #if os(iOS)
            .padding(.horizontal, 8)
            #else
            .padding(.horizontal)
            #endif
            .padding(.vertical, 8)

            Divider()

            WeekDayTimeGrid(
                days: weekDays,
                events: vm.events,
                containerId: Ids.calendarWeekGrid,
                onSelectEvent: { event in Task { await vm.selectEvent(event) } },
                onEmptySlot: { date in vm.beginCompose(atSlot: date) }
            )
        }
    }

    private var weekDays: [Date] { WeekFormat.days(from: weekStart) }

    private var weekLabel: String { WeekFormat.label(from: weekStart) }
}
