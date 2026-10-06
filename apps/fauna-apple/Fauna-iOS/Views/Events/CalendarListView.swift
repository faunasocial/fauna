import SwiftUI
import FaunaKit

struct CalendarListView: View {
    /// Bumped on every nav patch (`appState.navGeneration` — the Media/Devices
    /// reloadToken idiom). A patch targeting events must show the events PAGE:
    /// the change pops any pushed event detail (below), the one navigation this
    /// page's own `NavigationStack` otherwise makes unreachable from outside — a
    /// same-value `moreSelectedView` re-set fires no SwiftUI change at all,
    /// which left a stale detail covering the page across e2e navigations.
    var reloadToken: Int = 0
    @Environment(AppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// App-scene-level (`FaunaApp`), so the half-written event outlives the page.
    @Environment(EventsVM.self) private var vm
    /// The view mode lives on `EventsVM` in the shared `FfiCalendarViewMode`
    /// vocabulary, not in a per-target `@State` behind a per-target enum —
    /// macOS's `MacEventListView` reads the same one
    /// (`FaunaKit/Utilities/CalendarViewModeApple.swift`).
    private var viewMode: FfiCalendarViewMode { vm.viewMode }

    // New calendar form
    @State private var newCalendarName = ""

    var body: some View {
        // Project bindings to the @Observable EventsVM so `$vm.selectedEvent`
        // drives the detail push reactively (navigationDestination(item:) below).
        @Bindable var vm = vm
        NavigationStack {
            VStack(spacing: 0) {
                // Persistent date-nav header: prev/next period + the current
                // date/range label, shown on ALL view modes (agenda included) to
                // mirror macOS' always-present toolbar (`MacEventListView`).
                // `calendar-date-label` is a shared required element of the
                // `calendar-view-controls` component (ui.yaml), not grid-only —
                // so the default agenda view carries it too (priority #1 parity).
                HStack {
                    Button { vm.previousPeriod() } label: { Image(systemName: "chevron.left") }
                        .accessibilityIdentifier(Ids.eventsPrevMonth)
                        .automationActivate(Ids.eventsPrevMonth) { vm.previousPeriod() }
                    Spacer()
                    automationText(Ids.calendarDateLabel, vm.currentDateLabel)
                    Spacer()
                    Button { vm.nextPeriod() } label: { Image(systemName: "chevron.right") }
                        .accessibilityIdentifier(Ids.eventsNextMonth)
                        .automationActivate(Ids.eventsNextMonth) { vm.nextPeriod() }
                }
                .padding(.horizontal)

                Group {
                    if viewMode == .month {
                        MonthGridView(
                            month: vm.currentDate,
                            events: vm.events,
                            onSelectDay: { date in
                                // Mode before date: `selectDay` repaints
                                // `calendar-date-label` for whatever mode is
                                // current, so flipping after it would leave the
                                // month label standing over a day grid.
                                vm.viewMode = .day
                                vm.selectDay(date)
                            },
                            onNewEventOnDay: { date in vm.beginCompose(onDay: date) },
                            onSelectEvent: { event in Task { await vm.selectEvent(event) } }
                        )
                    } else if viewMode == .week {
                        WeekView(vm: vm)
                    } else if viewMode == .day {
                        DayTimelineView(vm: vm)
                    } else {
                        // Eager `ScrollView { VStack }`, NOT a lazy `List { Section }`
                        // (rule 6 — apple-e2e-automation.md § Registration rules): an
                        // iOS `List` lazily realizes AND POOLS its rows, so a removed
                        // event-card's hosting view lingers ON-SCREEN (x=32) and its
                        // `_AutomationLifecycle.deinit` fires only when the pool slot
                        // is reused. The in-process registry (geometry-FIRST) counts
                        // that pooled-on-screen card as live — the delete-zombie
                        // (`test_event_create_and_delete`) AND the rsvp/reminder
                        // union-card leak (a prior test's event, shown in the union
                        // view at `navigate()`, survives `select_calendar` into a
                        // fresh empty calendar's `initial_count`, so the new event's
                        // count never grows past it). A non-lazy ScrollView renders
                        // every row eagerly and tears a removed one down at once
                        // (deinit → unregister), so `count` stays accurate — no
                        // `.id()`-teardown band-aid needed. Mirrors `MacCalendarListView`
                        // + every `Admin*View`. Cost: rows lose `.insetGrouped` inset
                        // styling — the accepted rule-6 production-UI tradeoff.
                        ScrollView {
                            VStack(alignment: .leading, spacing: 16) {
                                // `error-message` per ui.yaml's events page + e2e
                                // convention 2. `EventsVM` assigns `errorMessage`
                                // on every failing path (create/select/delete/rsvp/
                                // reminder), but only the detail surface rendered
                                // it, so a failure here was invisible to the user
                                // and read back as `count=0` to the harness —
                                // indistinguishable from "no error happened".
                                if let error = vm.errorMessage {
                                    ErrorBanner(message: error)
                                }
                                // Calendars
                                VStack(alignment: .leading, spacing: 8) {
                                    Text(L.events.calendars)
                                        .font(.headline)
                                    if vm.showNewCalendar {
                                        VStack(spacing: 8) {
                                            TextField(L.events.calendarName, text: $newCalendarName)
                                                .textFieldStyle(.roundedBorder)
                                                .accessibilityIdentifier(Ids.calendarName)
                                                .automationField(Ids.calendarName, text: $newCalendarName)
                                            HStack {
                                                Button(L.common.create) {
                                                    submitCreateCalendar()
                                                }
                                                .accessibilityIdentifier(Ids.createCalendar)
                                                .buttonStyle(.borderedProminent)
                                                .controlSize(.small)
                                                .disabled(newCalendarName.trimmingCharacters(in: .whitespaces).isEmpty
                                                          || vm.creatingCalendar)
                                                .automationActivate(
                                                    Ids.createCalendar,
                                                    isEnabled: { !(newCalendarName.trimmingCharacters(in: .whitespaces).isEmpty
                                                                   || vm.creatingCalendar) }
                                                ) { submitCreateCalendar() }
                                                // `new-calendar-btn` above only
                                                // reveals this form (arming is
                                                // local); Create is the commit,
                                                // and Cancel beside it is local.
                                                .faunaGate("fauna.bridges.provision_calendar")
                                                Button(L.common.cancel) { vm.showNewCalendar = false }
                                                    .controlSize(.small)
                                            }
                                        }
                                    }

                                    ForEach(vm.calendars) { cal in
                                        HStack {
                                            Button {
                                                Task { await vm.selectCalendar(cal) }
                                            } label: {
                                                Text(cal.name)
                                                    .fontWeight(vm.selectedCalendar?.id == cal.id ? .bold : .regular)
                                                    .contentShape(Rectangle())
                                            }
                                            .buttonStyle(.plain)
                                            .tint(.primary)
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

                                    // `.ics` import / export of the selected calendar —
                                    // the shared FaunaKit view both apple apps embed
                                    // (present only while a calendar is selected).
                                    CalendarFileControls(vm: vm)
                                }

                                Divider()

                                // Events — the selected calendar's, or the union of
                                // every owned calendar's when none is selected
                                // (EventsVM.queryEventItems; matches macOS'
                                // unconditional agenda list — priority #4).
                                VStack(alignment: .leading, spacing: 8) {
                                    Text(L.events.title)
                                        .font(.headline)
                                    if vm.isLoading {
                                        ProgressView()
                                    } else if vm.events.isEmpty {
                                        Text(L.events.noEventsInCalendar)
                                            .foregroundStyle(.secondary)
                                    } else {
                                        ForEach(vm.events) { ev in
                                            // A tap-driven `NavigationLink` push does NOT fire in-process
                                            // (the driver's `automationActivate` runs the closure but never
                                            // performs SwiftUI's tap-push), so the detail never mounted and
                                            // `event-detail-summary` stayed count=0. Drive the push off
                                            // `vm.selectedEvent` instead (the `.navigationDestination` below),
                                            // set by `selectEvent` — a state-driven reveal that mounts the
                                            // detail regardless of tap, matching macOS' inline split-view.
                                            EventCardRow(
                                                event: ev,
                                                onSelect: { Task { await vm.selectEvent(ev) } },
                                                onRsvp: { response in Task { await vm.rsvp(eventId: ev.id, response: response) } }
                                            )
                                        }
                                    }
                                }

                                // Invited events
                                if !vm.invitedEvents.isEmpty {
                                    Divider()
                                    VStack(alignment: .leading, spacing: 8) {
                                        Text(L.events.invitedEvents)
                                            .font(.headline)
                                        ForEach(vm.invitedEvents) { ev in
                                            VStack(alignment: .leading, spacing: 2) {
                                                Text(ev.summary).fontWeight(.semibold)
                                                Text("\(ev.dtstart) – \(ev.dtend)")
                                                    .font(.caption).foregroundStyle(.secondary)
                                            }
                                            .frame(maxWidth: .infinity, alignment: .leading)
                                        }
                                    }
                                }
                            }
                            .padding()
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
            }
            .pageTitle(L.events.title)
            // State-driven detail push: `selectEvent` (from an event-card tap OR
            // the in-process driver OR a month-grid select) populates
            // `vm.selectedEvent`, and this presents `EventDetailView` — which reads
            // `vm.selectedEvent` directly, exactly like macOS' `MacEventDetailView`.
            // Back-navigation (or `deleteEvent` clearing `selectedEvent`) pops it.
            // `item:` off the `@Bindable`-projected binding so the push is observed
            // reactively (a plain `Binding { … != nil }` would not re-render body).
            .navigationDestination(item: $vm.selectedEvent) { _ in
                EventDetailView(vm: vm)
            }
            // A nav patch (reloadToken bump) shows the events PAGE: pop any
            // pushed detail — the cross-app semantics of navigating to
            // events (linux/web land on the list), and the only external path
            // to this page-private push state.
            .onChange(of: reloadToken) {
                vm.selectedEvent = nil
            }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    HStack(spacing: 2) {
                        // Toggle order comes from the shared
                        // `calendarViewModes()`, not a local `allCases`.
                        ForEach(calendarViewModes(), id: \.self) { mode in
                            Button(mode.displayLabel) {
                                vm.viewMode = mode
                            }
                            .buttonStyle(.bordered)
                            .tint(viewMode == mode ? .accentColor : .secondary)
                            .controlSize(.small)
                            .accessibilityIdentifier(mode.accessibilityId)
                            // The driver clicks the per-mode id (calendar-view-*),
                            // not the segmented control's own id, to switch views.
                            .automationActivate(mode.accessibilityId) { vm.viewMode = mode }
                        }
                    }
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.eventsViewToggle)
                    .automationSelect(
                        Ids.eventsViewToggle,
                        // The shared lowercase wire word, not a capitalized
                        // Swift `rawValue` — apple was the only app publishing
                        // the latter.
                        value: { viewMode.wire },
                        set: { wire in
                            if let match = calendarViewModes().first(where: { $0.wire == wire }) {
                                vm.viewMode = match
                            }
                        }
                    )
                }
                ToolbarItem(placement: .primaryAction) {
                    HStack(spacing: 12) {
                        Button { vm.showNewCalendar.toggle() } label: {
                            Image(systemName: "calendar.badge.plus")
                        }
                        .accessibilityIdentifier(Ids.newCalendarBtn)
                        .automationActivate(Ids.newCalendarBtn) { vm.showNewCalendar.toggle() }

                        // Always shown (matching macOS' toolbar) so a new event can
                        // be created without a prior calendar select — createEvent
                        // falls back to the first available calendar.
                        Button { vm.showNewEvent.toggle() } label: {
                            Image(systemName: "plus")
                        }
                        .accessibilityIdentifier(Ids.newEventBtn)
                        .automationActivate(Ids.newEventBtn) { vm.showNewEvent.toggle() }
                    }
                }
            }
            .sheet(isPresented: $vm.showNewEvent) {
                NavigationStack {
                    EventFormView(vm: vm)
                }
            }
            .refreshable {
                async let c: () = vm.loadCalendars()
                async let i: () = vm.loadInvitedEvents()
                _ = await (c, i)
                appState.lastEvents = vm.events
            }
            // Keyed on the session's client, not one-shot: More → Events is not
            // unmounted by the switch teardown, so before this the page never
            // re-loaded at a switch at all — the outgoing account's calendars stayed
            // on screen, `pollWhileVisible()` below kept polling as that account, and
            // the events drafts rail stayed ITS rail, so the app delegate's
            // leave-flush would have written the incoming account's typed draft into
            // it (`EventsVM.reset()` spells this out). Re-keying also cancels the poll
            // task itself, which is corollary 2's second rule met: the loops that
            // WRITE the state are retired by the same drop. `account-scoping.md`
            // § The scoping taxonomy, the "reused shell" case.
            //
            // `reloadToken` stays on its own `.onChange` below (it pops a pushed
            // event detail) — it is a nav signal, not an identity one.
            .task(id: SessionKey(client)) {
                guard let client else {
                    vm.reset()
                    return
                }
                vm.configure(api: client.api)
                // Reachable from the app delegate's leave-flush
                // (`reserved-folders.md` § The leave-flush promise) —
                // mirrors macOS `appState.eventsVM`.
                appState.eventsVM = vm
                // Once per session, not per appearance (`hasEventDraftsRail`).
                if !vm.hasEventDraftsRail {
                    let drafts = try? await client.api.eventDrafts()
                    vm.attachEventDrafts(drafts)
                }
                async let c: () = vm.loadCalendars()
                async let i: () = vm.loadInvitedEvents()
                _ = await (c, i)
                appState.lastEvents = vm.events
                // Runs until this view disappears (task cancellation) — surfaces
                // an externally-written CalDAV calendar/event while the user
                // stays on the page (events.md § Implementation status,
                // quick-appearance poll).
                await vm.pollWhileVisible()
            }
            // Re-pull on a fauna.calendar.changed push — cuts the poll's
            // latency down to push latency; the poll above stays as backstop.
            .onCalendarChanged { await vm.refreshFromPush() }
            // A dropped push (offline, or the socket flapped) is recovered on
            // the next reconnect — the same diff-before-swap refresh the push
            // arm uses, mirroring Contacts/Feed/Media's own `.onReconnect`
            // (transport.md § Which surfaces a push invalidates — `events` is
            // part of the full reconnect sweep).
            .onReconnect { await vm.refreshFromPush() }
            // The VM outlives the page, so the form must not (`EventsVM.pageLeft`).
            .onDisappear { vm.pageLeft() }
        }
    }

    /// The create-calendar button's action, factored out so the automation
    /// sibling drives the exact same code path the Button does.
    private func submitCreateCalendar() {
        Task {
            await vm.createCalendar(name: newCalendarName)
            newCalendarName = ""
        }
    }
}
