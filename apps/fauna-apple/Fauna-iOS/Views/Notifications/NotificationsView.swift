import SwiftUI
import FaunaKit

struct NotificationsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @Environment(AppState.self) private var appState
    /// The Post arm's destination seam — `pendingPostOpen` is the same
    /// cross-page deep-link slot a `search-result-item` activation uses.
    @Environment(FeedVM.self) private var feedVM
    @State private var vm = NotificationsVM()

    var body: some View {
        NavigationStack {
            List(vm.notifications) { notif in
                row(notif)
            }
            .pageTitle(L.common.notifications)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    HStack {
                        Text("\(vm.unreadCount) unread")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier(Ids.notificationCountBadge)
                            // Re-reads live `unreadCount` per lookup (mirrors macOS). Env-gated no-op.
                            .automationValue(Ids.notificationCountBadge, text: { "\(vm.unreadCount) unread" })
                        Button(L.common.markAllRead) {
                            Task { await markAllRead() }
                        }
                        .accessibilityIdentifier(Ids.notificationMarkRead)
                        // Same `markAllRead()` the Button runs. Env-gated no-op.
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
            // Keyed on the session's client instance, not one-shot and not on
            // `client != nil`: More → Notifications is not unmounted by the switch
            // teardown (it clears `selectedSettingsPage`, never `moreSelectedView`),
            // so the outgoing account's list would otherwise stay on screen. A
            // `SessionKey` fires across the nil phase AND on a same-nil-ness swap,
            // which is what the plain `client != nil` change it replaces could not see
            // (`account-scoping.md` § The scoping taxonomy, the "reused shell" case).
            .task(id: SessionKey(client)) { await loadNotifications() }
            // Re-pull notifications on WS-RPC reconnect (mirrors linux).
            .onReconnect { await loadNotifications() }
            // Re-pull live on a `fauna.notification` push — grows a new row with no
            // navigation (mirrors android's `notificationTick`; `transport.md` § Push events).
            .onPushNotification { await loadNotifications() }
        }
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
                .font(.caption)
            VStack(alignment: .leading, spacing: 4) {
                Text(notif.body)
                    .font(.subheadline)
                Text(ValueFormat.relativeTime(thenMs: Int64(notif.createdAt) / 1000))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.notificationItem)

        if let destination {
            content
                // `.onTapGesture` on the whole row rather than a wrapping
                // Button, so the row's own inner content keeps working —
                // mirrors the feed card's whole-card open.
                .contentShape(Rectangle())
                .onTapGesture { open(destination) }
                .automationActivate(Ids.notificationItem,
                                    text: { notif.body }) { open(destination) }
        } else {
            // Reads the notification body per row (mirrors macOS). Env-gated no-op.
            content.automationValue(Ids.notificationItem, text: { notif.body })
        }
    }

    /// Route the row's typed destination into the destination page's own
    /// gesture, exactly as a direct visit there would — the shape
    /// `SearchResultsView.openResult` uses for `SearchNav`. Switches the
    /// destination tab FIRST, synchronously, then lets any resolve round trip
    /// fill in behind it.
    private func open(_ destination: FfiNotificationDestination) {
        switch destination {
        case .post(let postId):
            appState.selectedTab = "feed"
            feedVM.pendingPostOpen = postId
        case .knock(let senderId):
            appState.selectedTab = "contacts"
            appState.pendingKnockSenderId = senderId
        case .family:
            // The Family page lives in the More stack, the same destination
            // `{"view":"family"}` takes (`SettingsView`'s own entry).
            appState.moreSelectedView = "family"
            appState.selectedTab = "more"
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
            // The nil-client phase of a switch: drop the outgoing account's rows
            // rather than leaving them rendered until the incoming client lands.
            // (`appState.notificationsUnreadCount` is on `ActorScope`'s app-owned
            // drop — not re-cleared here.)
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

