import SwiftUI
import FaunaKit

struct MacEventListView: View {
    @Bindable var vm: EventsVM

    /// The view mode lives on `EventsVM` in the shared `FfiCalendarViewMode`
    /// vocabulary, not in a per-target `@State` behind a per-target enum. Both
    /// shells declared their own four-case copy of the same four modes while the
    /// pan ignored the mode entirely; the id, the wire word, the toggle order
    /// and the pan distance are all shared now
    /// (`FaunaKit/Utilities/CalendarViewModeApple.swift`).
    private var viewMode: FfiCalendarViewMode { vm.viewMode }

    var body: some View {
        VStack(spacing: 0) {
            // Toolbar
            HStack {
                Text(L.events.title)
                    .font(.headline)

                Button { vm.previousPeriod() } label: { Image(systemName: "chevron.left") }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.eventsPrevMonth)
                    .automationActivate(Ids.eventsPrevMonth) { vm.previousPeriod() }

                automationText(Ids.calendarDateLabel, vm.currentDateLabel)
                    .font(.subheadline)

                Button { vm.nextPeriod() } label: { Image(systemName: "chevron.right") }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.eventsNextMonth)
                    .automationActivate(Ids.eventsNextMonth) { vm.nextPeriod() }

                Spacer()
                Button(vm.showNewEvent ? L.common.cancel : L.events.newEvent) {
                    vm.showNewEvent.toggle()
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.newEventBtn)
                .automationActivate(Ids.newEventBtn) { vm.showNewEvent.toggle() }

                // Toggle order comes from the shared `calendarViewModes()`, not a
                // local `allCases`, so the four apple modes cannot drift out of
                // the order the other six apps render.
                Picker("", selection: $vm.viewMode) {
                    ForEach(calendarViewModes(), id: \.self) { mode in
                        Text(mode.displayLabel)
                            .tag(mode)
                            .accessibilityIdentifier(mode.accessibilityId)
                            // The driver clicks the per-mode id (calendar-view-*),
                            // not the segmented control's own id, to switch views.
                            .automationActivate(mode.accessibilityId) { vm.viewMode = mode }
                    }
                }
                .pickerStyle(.segmented)
                .frame(width: 240)
                .accessibilityIdentifier(Ids.eventsViewToggle)
                .automationSelect(
                    Ids.eventsViewToggle,
                    // The shared lowercase wire word, not a capitalized Swift
                    // `rawValue` — apple was the only app publishing the latter.
                    value: { viewMode.wire },
                    set: { wire in
                        if let match = calendarViewModes().first(where: { $0.wire == wire }) {
                            vm.viewMode = match
                        }
                    }
                )
            }
            .padding(.horizontal)
            .padding(.vertical, 8)

            // `error-message` per ui.yaml's events page + e2e convention 2. Every
            // failing path in `EventsVM` — create/select/delete/rsvp/reminder —
            // assigns `errorMessage`, but until this banner existed only the
            // *detail* surface rendered it, so a failed create on this page was
            // invisible to the user AND read back as `error-message: count=0` to
            // the harness, i.e. indistinguishable from "no error happened".
            if let error = vm.errorMessage {
                ErrorBanner(message: error)
                    .padding(.horizontal)
            }

            if vm.showNewEvent {
                MacEventFormView(vm: vm)
                    .padding(.horizontal)
                Divider()
            }

            if vm.isLoading {
                ProgressView()
                    .frame(maxHeight: .infinity)
            } else {
                // The month / week / day grids render even when the calendar is
                // empty (an empty grid is still a valid view, and its container id
                // must be present); only the agenda list shows the empty state.
                switch viewMode {
                case .agenda:
                    if vm.events.isEmpty {
                        Text(L.events.noEventsYet)
                            .foregroundStyle(.secondary)
                            .frame(maxHeight: .infinity)
                    } else {
                        // Eager `ScrollView { VStack }`, NOT a lazy `List`
                        // (apple-e2e-automation.md registration rule 6). A macOS
                        // `List` is an NSTableView: it realizes rows lazily, so a
                        // row appended to `vm.events` can simply never be built —
                        // no view, no `.onAppear`, no registration. Measured
                        // 2026-07-30: after a successful second create the agenda
                        // listed only the first event for the full 30 s budget,
                        // with the registry holding exactly ONE `event-card` slot
                        // (1/1 visible — not a second slot hidden or geo-parked),
                        // while a month→agenda round trip that ran NO query
                        // surfaced both — i.e. the model had the event all along
                        // and only the table had not built its row. That is
                        // `test_event_create_and_delete[macos]`, red ~35% under
                        // machine load and the reason this module read as flaky.
                        //
                        // It is also why a row COUNT here was never a measure of
                        // the data (the count saturated at the rows the pane
                        // happened to realize), which cost this module a sweep of
                        // count barriers. Rendering eagerly makes both the count
                        // and the identity honest again.
                        //
                        // Mirrors the iOS agenda (`CalendarListView`, converted
                        // 2026-07-16 for the same test and the same rule), the
                        // pane directly above (`MacCalendarListView`), and every
                        // `Admin*View` — priorities #1/#3. Cost: the rows lose
                        // `.listStyle(.plain)` styling, the accepted rule-6
                        // production-UI tradeoff those siblings already paid.
                        ScrollView {
                            VStack(alignment: .leading, spacing: 8) {
                                ForEach(vm.events) { ev in
                                    EventCardRow(
                                        event: ev,
                                        onSelect: { Task { await vm.selectEvent(ev) } },
                                        onRsvp: { response in Task { await vm.rsvp(eventId: ev.id, response: response) } }
                                    )
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal)
                            .padding(.vertical, 4)
                        }
                    }
                case .month:
                    MacMonthView(vm: vm, onDrillToDay: { date in
                        // Mode before date: `selectDay` repaints
                        // `calendar-date-label` for whatever mode is current, so
                        // flipping after it would leave the month label standing
                        // over a day grid.
                        vm.viewMode = .day
                        vm.selectDay(date)
                    })
                case .week:
                    WeekView(vm: vm)
                case .day:
                    MacDayTimelineView(vm: vm)
                }
            }
        }
    }
}
