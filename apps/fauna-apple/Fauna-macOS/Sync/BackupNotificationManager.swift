import UserNotifications
import FaunaKit

class BackupNotificationManager {
    static let categoryIdentifier = "BACKUP_RESULT"
    static let viewBackupAction = "VIEW_BACKUP"

    /// UNUserNotificationCenter raises an ObjC exception in unsigned SPM debug
    /// builds. Guard every call site — the test itself is `NotificationHost`'s.
    private static var notificationsAvailable: Bool { NotificationHost.isAvailable }

    static func requestPermission() {
        guard notificationsAvailable else { return }
        let viewAction = UNNotificationAction(
            identifier: viewBackupAction,
            title: L.backups.notification.viewBackups,
            options: [.foreground]
        )
        let category = UNNotificationCategory(
            identifier: categoryIdentifier,
            actions: [viewAction],
            intentIdentifiers: []
        )
        let center = UNUserNotificationCenter.current()
        center.setNotificationCategories([category])
        center.requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    static func notifyBackupComplete(folder: String, fileCount: Int, totalBytes: Int) {
        guard notificationsAvailable else { return }
        let size = ValueFormat.byteSize(totalBytes)
        postNotification(
            title: L.backups.notification.completeTitle,
            body: L.backups.notification.completeBody(folder: folder, count: String(fileCount), size: size),
            folder: folder
        )
    }

    static func notifyBackupFailed(folder: String, error: String) {
        guard notificationsAvailable else { return }
        postNotification(
            title: L.backups.notification.failedTitle,
            body: L.backups.notification.failedBody(folder: folder, error: error),
            folder: folder
        )
    }

    private static func postNotification(title: String, body: String, folder: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        content.categoryIdentifier = categoryIdentifier
        content.userInfo = ["folder": folder]
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }
}
