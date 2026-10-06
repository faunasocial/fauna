import SwiftUI
import FaunaKit

struct MacNotificationsView: View {
    @Environment(MacAppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The Post arm's destination seam — `pendingPostOpen` is the same
    /// cross-page deep-link slot a `search-result-item` activation uses.
    @Environment(FeedVM.self) private var feedVM
    @State private var vm = NotificationsVM()

    var body: some View {
        List(vm.notifications) { notif in
            row(notif)
        }
        .overlay {
            if vm.notifications.isEmpty && vm.errorMessage == nil {
                ContentUnavailableView(L.common.noNotifications,
                                       systemImage: "bell.slash")
            }
        }
        .pageTitle(L.common.notifications)
        .toolbar {
            ToolbarItem(placement: .automatic) {
                HStack(spacing: 12) {
                    automationText(Ids.notificationCountBadge, "\(vm.unreadCount) unread")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Button(action: { Task { await markAllRead() } }) {
                        Label(L.common.markAllRead, systemImage: "checkmark.circle")
                    }
                    .accessibilityIdentifier(Ids.notificationMarkRead)
                    .automationActivate(Ids.notificationMarkRead) {
                        Task { await markAllRead() }
                    }
                }
            }
        }
        .safeAreaInset(edge: .bottom) {
            if let errorMessage = vm.errorMessage {
                ErrorBanner(message: errorMessage)
                    .padding()
            }
        }
        // The drop below is REDUNDANT on macOS and carried for uniformity, as
        // `SearchVM`'s is: `tearDownSessionForSwitch()` sets `isOnboarded = false`,
        // which unmounts `MainWindowView` wholesale, so this view dies with the window
        // and its view model with it. That unmount IS the guarantee here
        // (`account-scoping.md` § The scoping taxonomy, the in-memory corollary: an app
        // whose drop rides a shell teardown must say where the guarantee comes from) —
        // which is exactly what iOS does not have, and why the seam lives on the view
        // model rather than at either site .
        // A `SessionKey` replaces the `client != nil` change for the reason its iOS
        // twin gives: the boolean cannot see a client swap that keeps it true.
        .task(id: SessionKey(client)) { await loadNotifications() }
        // Re-pull notifications on WS-RPC reconnect (mirrors linux).
        .onReconnect { await loadNotifications() }
        // Re-pull live on a `fauna.notification` push — grows a new row with no
        // navigation (mirrors android's `notificationTick`; `transport.md` § Push events).
        .onPushNotification { await loadNotifications() }
    }

    /// One notification row. A row the shared router can place is a CONTROL —
    /// tappable, and registered with its activation; one it cannot place is a
    /// plain label, exactly as tui paints a non-navigable row
    /// (`behavior/notifications.md` § Deep-link destinations).
    ///
    /// Never an always-registered activate behind `isEnabled: { false }`: the
    /// automation server refuses a disabled control with HTTP 409, so an inert
    /// row would REJECT the gesture where the page's contract is to ignore it.
    /// And the activation carries `text:` rather than sitting beside a separate
    /// `.automationValue` for the same id — `AutomationRegistry` puts a second
    /// modifier in a second entry, which would split the activate and the read
    /// across two index slots.
    @ViewBuilder
    private func row(_ notif: NotificationItem) -> some View {
        let destination = notificationDestination(
            for: notif, hasFamily: appState.familyStatus.hasRelationship)
        let content = HStack(alignment: .top, spacing: 8) {
            NotificationTypeIcon(notifType: notif.notifType)
            VStack(alignment: .leading, spacing: 4) {
                Text(notif.body)
                    .font(.body)
                if notif.read {
                    Text(ValueFormat.relativeTime(thenMs: Int64(notif.createdAt) / 1000))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    Text(ValueFormat.relativeTime(thenMs: Int64(notif.createdAt) / 1000))
                        .font(.caption)
                        .foregroundStyle(.blue)
                }
            }
        }
        .accessibilityIdentifier(Ids.notificationItem)

        if let destination {
            content
                // `.onTapGesture` on the whole row rather than a wrapping
                // Button, so the row's own inner content keeps working —
                // mirrors `MacFeedDetailView`'s whole-card open.
                .contentShape(Rectangle())
                .onTapGesture { open(destination) }
                .automationActivate(Ids.notificationItem,
                                    text: { notif.body }) { open(destination) }
        } else {
            content.automationValue(Ids.notificationItem, text: { notif.body })
        }
    }

    /// Route the row's typed destination into the destination page's own
    /// gesture, exactly as a direct visit there would — the shape
    /// `SearchResultsView.openResult` uses for `SearchNav`. Switches the
    /// destination page FIRST, synchronously, then lets any resolve round trip
    /// fill in behind it.
    private func open(_ destination: FfiNotificationDestination) {
        switch destination {
        case .post(let postId):
            appState.selectedSidebar = .feed
            feedVM.pendingPostOpen = postId
        case .knock(let senderId):
            appState.selectedSidebar = .contacts
            appState.pendingKnockSenderId = senderId
        case .family:
            appState.selectedSidebar = .family
        case .external(let url):
            // Off-app: a bridged post's page on its network's own website,
            // through the one opener every external link shares (fire-and-
            // forget, suppressed + logged under the e2e harness). The URL is
            // shared Rust's — a fixed origin plus validated segments — so it
            // is opened as-is; a string `URL` cannot parse is simply dropped.
            if let url = URL(string: url) { OpenURL.open(url) }
        }
    }

    private func loadNotifications() async {
        guard client != nil else {
            vm.reset()
            return
        }
        await vm.loadNotifications(client: client, actorId: appState.session.actorId)
        appState.notificationsUnreadCount = vm.unreadCount
    }

    private func markAllRead() async {
        await vm.markAllRead()
        appState.notificationsUnreadCount = vm.unreadCount
    }
}
