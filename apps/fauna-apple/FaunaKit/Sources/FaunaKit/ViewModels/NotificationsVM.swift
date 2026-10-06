import SwiftUI

/// Shared view-model for the Notifications page, macOS + iOS (one FaunaKit
/// VM). A thin proxy over `APIClient.getNotifications`/`markNotificationsRead`
/// — no notification-specific business logic here (priority #1/#2); both
/// platform views previously hand-rolled identical fetch + state-write logic.
///
/// The one real divergence this lift resolves (priority #4 — pick the richest
/// existing pattern, not the simplest): mac surfaced `errorMessage` on a
/// failed fetch, iOS silently swallowed it via `try?` (a failed fetch looked
/// identical to "no notifications yet"). Both platforms now surface the
/// error — a failed fetch is not the same thing as an empty inbox.
@MainActor @Observable
public final class NotificationsVM {
    public private(set) var notifications: [NotificationItem] = []
    public private(set) var unreadCount = 0
    public var errorMessage: String?

    private var api: APIClient?
    private var actorId: String?

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary), on `SearchVM.reset()`'s shape. Called by
    /// ``configure(api:actorId:)`` on an api-identity change **before** it re-points,
    /// and by the page's nil-client phase: More → Notifications is not unmounted by
    /// the iOS switch teardown (it clears `selectedSettingsPage`, never
    /// `moreSelectedView`), so without it the outgoing account's notification list and
    /// unread count stay rendered under the incoming one .
    ///
    /// `actorId` goes with them — it is the actor ``markAllRead()`` marks read, so a
    /// surviving one would have the incoming account's "mark all read" tap clear the
    /// OUTGOING account's notifications.
    public func reset() {
        api = nil
        actorId = nil
        notifications = []
        unreadCount = 0
        errorMessage = nil
    }

    public func configure(api: APIClient, actorId: String?) {
        if let current = self.api, current !== api { reset() }
        self.api = api
        self.actorId = actorId
    }

    #if DEBUG
    /// Unit-test seam: stand in for a completed fetch, which a real one cannot be
    /// offline. Never called by production — mirrors `AddressBookVM.seedForTest`.
    func seedForTest(notifications: [NotificationItem], unreadCount: Int) {
        self.notifications = notifications
        self.unreadCount = unreadCount
    }
    #endif

    public func loadNotifications() async {
        guard let api, let actorId else { return }
        do {
            let data = try await api.getNotifications(actorId: actorId)
            // The in-flight clause: cancelling is never sufficient — a read already
            // suspended for the outgoing account still returns and would still assign.
            guard self.api === api else { return }
            notifications = data.items
            unreadCount = data.unread
            errorMessage = nil
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    /// `configure` + `loadNotifications`, the fetch sequence both platform
    /// Notifications pages call on appear/reconnect/push — previously
    /// hand-duplicated in each page's own `loadNotifications()` wrapper. A nil
    /// `client` is a no-op, matching `loadNotifications`'s own missing-`api`
    /// contract. Each page still mirrors the resulting `unreadCount` into its
    /// own (differently-typed) app state itself — this only dedups the fetch.
    public func loadNotifications(client: FaunaClient?, actorId: String?) async {
        guard let client else { return }
        configure(api: client.api, actorId: actorId)
        await loadNotifications()
    }

    public func markAllRead() async {
        guard let api, let actorId else { return }
        do {
            try await api.markNotificationsRead(actorId: actorId)
            guard self.api === api else { return }   // the in-flight clause
            unreadCount = 0
            errorMessage = nil
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }
}
