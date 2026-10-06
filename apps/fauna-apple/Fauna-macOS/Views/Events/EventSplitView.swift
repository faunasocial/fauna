import SwiftUI
import FaunaKit

struct EventSplitView: View {
    @Environment(MacAppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// App-scene-level (`FaunaMacApp`), so the half-written event outlives the page.
    @Environment(EventsVM.self) private var vm

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                MacCalendarListView(vm: vm)
                Divider()
                MacEventListView(vm: vm)
            }
            .frame(minWidth: 300, maxWidth: 400)

            if vm.selectedEvent != nil {
                MacEventDetailView(vm: vm)
            } else {
                ContentUnavailableView(L.events.title,
                    systemImage: "calendar",
                    description: Text(L.events.selectCalendar))
            }
        }
        .accessibilityElement(children: .contain)
        .pageTitle(L.events.title)
        // The calendar `.ics` import / export controls used to be two toolbar buttons
        // here; they now sit beside the calendar list (`MacCalendarListView`) in the
        // shared `CalendarFileControls`, with the ids and the outcome line the toolbar
        // pair never had (`ui/events.md` § Import / Export).
        // The drop below is REDUNDANT on macOS and carried for uniformity, as
        // `SearchVM`'s is: `tearDownSessionForSwitch()` sets `isOnboarded = false`,
        // which unmounts `MainWindowView` wholesale, so this view dies with the
        // window. That unmount IS the guarantee here (`account-scoping.md` § The
        // scoping taxonomy, the in-memory corollary: an app whose drop rides a shell
        // teardown must say where the guarantee comes from) — which iOS does not
        // have . The key also moves from `client != nil`
        // to the client INSTANCE: the boolean cannot see a swap that keeps it true.
        .task(id: SessionKey(client)) {
            guard let client else {
                vm.reset()
                return
            }
            vm.configure(api: client.api)
            // Reachable from `AppDelegate.applicationShouldTerminate` for the
            // leave-flush (`reserved-folders.md` § The leave-flush promise) —
            // mirrors `appState.pushManager`'s own reachability precedent.
            appState.eventsVM = vm
            // Once per session, not per appearance (`hasEventDraftsRail`).
            if !vm.hasEventDraftsRail {
                let drafts = try? await client.api.eventDrafts()
                vm.attachEventDrafts(drafts)
            }
            await vm.loadCalendars()
            await vm.loadInvitedEvents()
            appState.lastEvents = vm.events
            // Runs until this view disappears (task cancellation) — surfaces an
            // externally-written CalDAV calendar/event while the user stays on
            // the page (events.md § Implementation status, quick-appearance poll).
            await vm.pollWhileVisible()
        }
        // Re-pull on a fauna.calendar.changed push — cuts the poll's latency
        // down to push latency; the poll above stays as backstop.
        .onCalendarChanged { await vm.refreshFromPush() }
        // A dropped push (offline, or the socket flapped) is recovered on the
        // next reconnect — the same diff-before-swap refresh the push arm uses,
        // mirroring Contacts/Feed/Media's own `.onReconnect` (transport.md
        // § Which surfaces a push invalidates — `events` is part of the full
        // reconnect sweep).
        .onReconnect { await vm.refreshFromPush() }
        // The VM outlives the page, so the form must not (`EventsVM.pageLeft`).
        .onDisappear { vm.pageLeft() }
    }
}
