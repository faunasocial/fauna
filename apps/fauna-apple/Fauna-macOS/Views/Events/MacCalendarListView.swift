import SwiftUI
import FaunaKit

private let calendarColors: [(String, Color)] = [
    ("blue", .blue), ("red", .red), ("green", .green),
    ("orange", .orange), ("purple", .purple), ("pink", .pink),
    ("yellow", .yellow), ("cyan", .cyan),
]

struct MacCalendarListView: View {
    let vm: EventsVM

    @State private var newCalName = ""
    @State private var selectedDate = Date()

    // Rendered as an eager `ScrollView { VStack }` rather than a lazy
    // `List { Section }`: in-process automation only registers an element on its
    // `.onAppear`, and a lazy `List` never `.onAppear`s rows below the fold. This
    // pane shares the left split ~50/50 with `MacEventListView` (`EventSplitView`),
    // so under the 280pt graphical DatePicker the whole calendar-management cluster
    // (`new-calendar-btn`, `calendar-name`, `calendar-visibility`, `create-calendar`,
    // `calendar-item`) fell below the fold → `count=0`, blocking the events e2e
    // cluster (harness-confirmed 2026-06-23). A plain (non-lazy) ScrollView renders
    // every child eagerly → all register. Cost: the calendar rows lose `.listStyle`
    // sidebar inset styling (the graphical DatePicker is unaffected) — an accepted,
    // minor production-UI tradeoff to satisfy the in-process registration limit
    // (the iOS lazy-`Form` analogue stays route-(b) accept).
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                // Mini calendar
                DatePicker("", selection: $selectedDate, displayedComponents: .date)
                    .datePickerStyle(.graphical)
                    .frame(maxHeight: 280)
                    .onChange(of: selectedDate) { _, _ in
                        // Could filter events by date here
                    }

                Divider()

                // Calendar management + list
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text(L.events.calendars)
                            .font(.headline)
                        Spacer()
                        Button(vm.showNewCalendar ? L.common.cancel : L.events.newCalendar) {
                            vm.showNewCalendar.toggle()
                        }
                        .controlSize(.small)
                        .accessibilityIdentifier(Ids.newCalendarBtn)
                        .automationActivate(Ids.newCalendarBtn) { vm.showNewCalendar.toggle() }
                    }

                    if vm.showNewCalendar {
                        VStack(spacing: 8) {
                            TextField(L.events.calendarName, text: $newCalName)
                                .textFieldStyle(.roundedBorder)
                                .accessibilityIdentifier(Ids.calendarName)
                                .automationField(Ids.calendarName, text: $newCalName)
                            Button(L.common.create) {
                                submitCreateCalendar()
                            }
                            .disabled(newCalName.trimmingCharacters(in: .whitespaces).isEmpty || vm.creatingCalendar)
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                            .accessibilityIdentifier(Ids.createCalendar)
                            .automationActivate(
                                Ids.createCalendar,
                                isEnabled: { !(newCalName.trimmingCharacters(in: .whitespaces).isEmpty || vm.creatingCalendar) }
                            ) { submitCreateCalendar() }
                            // `new-calendar-btn` above only reveals this form
                            // (arming is local); the Create is the commit.
                            .faunaGate("fauna.bridges.provision_calendar")
                        }
                        .padding(.vertical, 4)
                    }

                    ForEach(vm.calendars) { cal in
                        HStack {
                            Button {
                                Task { await vm.selectCalendar(cal) }
                            } label: {
                                HStack {
                                    Circle()
                                        .fill(colorForCalendar(cal))
                                        .frame(width: 10, height: 10)
                                    Text(cal.name)
                                        .fontWeight(vm.selectedCalendar?.id == cal.id ? .bold : .regular)
                                }
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier(Ids.calendarItem)
                            .automationActivate(Ids.calendarItem, value: { cal.name }) {
                                Task { await vm.selectCalendar(cal) }
                            }
                            Spacer()
                            Toggle("", isOn: Binding(
                                get: { vm.visibleCalendarIds.contains(cal.id) },
                                set: { _ in vm.toggleCalendarVisibility(calendarId: cal.id) }
                            ))
                            .labelsHidden()
                            .accessibilityIdentifier(Ids.calendarVisibility)
                            .automationActivate(
                                Ids.calendarVisibility,
                                value: { vm.visibleCalendarIds.contains(cal.id) ? "on" : "off" }
                            ) {
                                vm.toggleCalendarVisibility(calendarId: cal.id)
                            }
                        }
                        .padding(.vertical, 2)
                    }

                    // `.ics` import / export of the selected calendar — the shared
                    // FaunaKit view both apple apps embed (present only while a
                    // calendar is selected).
                    CalendarFileControls(vm: vm)
                }

                // Invited events
                if !vm.invitedEvents.isEmpty {
                    Divider()
                    VStack(alignment: .leading, spacing: 8) {
                        Text(L.events.invitedEvents)
                            .font(.headline)
                        ForEach(vm.invitedEvents) { ev in
                            Button {
                                Task { await vm.selectEvent(ev) }
                            } label: {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(ev.summary).fontWeight(.semibold).font(.caption)
                                    Text(ev.dtstart).font(.caption2).foregroundStyle(.secondary)
                                    if let loc = ev.location {
                                        Text(loc).font(.caption2).foregroundStyle(.secondary)
                                    }
                                }
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// The create-calendar button's action, factored out so the automation
    /// sibling drives the exact same code path the Button does.
    private func submitCreateCalendar() {
        Task {
            await vm.createCalendar(name: newCalName)
            newCalName = ""
        }
    }

    private func colorForCalendar(_ cal: FaunaCalendar) -> Color {
        if let name = cal.color,
           let match = calendarColors.first(where: { $0.0 == name }) {
            return match.1
        }
        return .accentColor
    }
}
