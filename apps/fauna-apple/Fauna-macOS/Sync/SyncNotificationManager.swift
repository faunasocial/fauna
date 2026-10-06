import UserNotifications
import FaunaKit

/// The injectable notification surface for per-file completed-sync events —
/// production posts a real `UNUserNotificationCenter` notification; tests
/// inject a recording stub so the forwarding seam never touches the OS
/// notification center (testing.md § point 10).
protocol SyncCompleteNotifying: Sendable {
    func notifySyncComplete(filename: String)
}

/// Posts the per-file completed-sync desktop notification — the macOS parity
/// of linux's `notify_sync_complete` (`sync-agent.md` § Implementation status,
/// A3 remainder). Same strings, same collapse behavior: a fixed request
/// identifier makes rapid uploads replace one banner instead of flooding
/// Notification Center (linux uses replace-ID 1005 for the same reason), and
/// no sound — sync completions are low-priority events.
final class SyncNotificationManager: SyncCompleteNotifying {
    /// UNUserNotificationCenter raises an ObjC exception in unsigned SPM debug
    /// builds. Guard every call site — the test itself is `NotificationHost`'s.
    private static var notificationsAvailable: Bool { NotificationHost.isAvailable }

    func notifySyncComplete(filename: String) {
        guard Self.notificationsAvailable else { return }
        let content = UNMutableNotificationContent()
        content.title = L.notifications.syncCompleteTitle
        content.body = L.notifications.syncCompleteBody(filename: filename)
        let request = UNNotificationRequest(
            identifier: "sync-complete", content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }
}

/// Forwards the shared Rust event listener's completed-sync basenames
/// (`FfiSyncCompleteObserver` — filter + self-healing socket loop live in
/// `fauna_ipc::events`) to the injected notification surface. Called on the
/// listener's background thread; `UNUserNotificationCenter` is thread-safe,
/// so no main-actor hop is needed.
final class SyncCompleteEventObserver: FfiSyncCompleteObserver {
    private let notifier: SyncCompleteNotifying

    init(notifier: SyncCompleteNotifying) {
        self.notifier = notifier
    }

    func onSyncComplete(filename: String) {
        notifier.notifySyncComplete(filename: filename)
    }
}
