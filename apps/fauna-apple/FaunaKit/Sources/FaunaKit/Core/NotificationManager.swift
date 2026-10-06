import Foundation
import UserNotifications

@Observable
public class NotificationManager {
    /// The knock toast's category and `userInfo` key (`postKnockNotification`
    /// below), matched by each shell's `UNUserNotificationCenterDelegate` to
    /// route a tap to Contacts with the sender pre-selected (macOS
    /// `AppDelegate.swift`, iOS `FaunaApp.swift`) — shared so neither delegate
    /// hand-repeats the string `postKnockNotification` posts.
    public static let knockCategoryIdentifier = "KNOCK"
    public static let knockSenderIdKey = "knockSenderId"

    public init() {}

    /// UNUserNotificationCenter raises an ObjC exception in unsigned SPM debug
    /// builds. Guard every call site — the test itself is `NotificationHost`'s.
    private var notificationsAvailable: Bool { NotificationHost.isAvailable }

    /// Raise the new-message banner for one thread — the *firing* half of
    /// `conversations` outcome 11, whose when/for-whom half is the shared
    /// `MessageNotificationTracker`'s (`docs/goal/ui/conversations.md`
    /// § Where logic lives). `MessageBannerObserver` is the only caller.
    ///
    /// **Returns whether the request reached `UNUserNotificationCenter`**, which
    /// is what makes the fired-banner log honest: `fauna_e2e_agent::
    /// MESSAGE_BANNERS_KEY` requires an entry to mean "a banner was raised for
    /// this thread", and `notificationsAvailable` is false for a build that has
    /// no notification host at all (the bare non-bundle binary — see
    /// `NotificationHost`), where nothing is handed anywhere. A caller that
    /// recorded regardless would log banners this process never attempted.
    @discardableResult
    public func postMessageNotification(from: String, subject: String, conversationId: String) -> Bool {
        guard notificationsAvailable else { return false }
        let content = UNMutableNotificationContent()
        content.title = from
        content.body = subject
        content.sound = .default
        content.categoryIdentifier = "MESSAGE"
        content.threadIdentifier = conversationId
        content.userInfo = ["conversationId": conversationId]

        let request = UNNotificationRequest(
            identifier: "msg-\(conversationId)-\(UUID().uuidString)",
            content: content, trigger: nil
        )
        UNUserNotificationCenter.current().add(request)
        return true
    }

    public func postGroupNotification(groupName: String, from: String, body: String, groupId: String) {
        guard notificationsAvailable else { return }
        let content = UNMutableNotificationContent()
        content.title = groupName
        content.body = "\(from): \(body)"
        content.sound = .default
        content.categoryIdentifier = "GROUP_MESSAGE"
        content.threadIdentifier = groupId
        content.userInfo = ["groupId": groupId]

        let request = UNNotificationRequest(
            identifier: "grp-\(groupId)-\(UUID().uuidString)",
            content: content, trigger: nil
        )
        UNUserNotificationCenter.current().add(request)
    }

    public func postGroupInviteNotification(from: String, groupName: String, groupId: String) {
        guard notificationsAvailable else { return }
        let content = UNMutableNotificationContent()
        content.title = "Group invitation"
        content.body = "\(from) invited you to \(groupName)"
        content.sound = .default
        content.categoryIdentifier = "GROUP_INVITE"
        content.userInfo = ["groupId": groupId]

        let request = UNNotificationRequest(
            identifier: "inv-\(groupId)-\(UUID().uuidString)",
            content: content, trigger: nil
        )
        UNUserNotificationCenter.current().add(request)
    }

    /// Raise the OS toast for an inbound knock (contact request) — apple's
    /// twin of android's `NotificationHelper.postKnockNotification` and
    /// linux's `notify_knock` (windows' own leg is row 876, not yet built).
    /// What it says is the shared decision `knockTextFor`
    /// (`behavior/notifications.md` § The knock toast) resolved through
    /// ``knockToastBody(for:)``: the knock row's own sentence when the push
    /// carries a body this build's catalog knows, else the toast's own
    /// sentence naming the sender's 8-hex prefix — never the knocker's raw
    /// `summary` on its own. `FaunaClient.startKnockObserver` is the only
    /// caller.
    public func postKnockNotification(_ knock: FfiKnock) {
        guard notificationsAvailable else { return }
        let content = UNMutableNotificationContent()
        content.title = L.lookup("notifications.knock_title")
        content.body = knockToastBody(for: knock)
        content.sound = .default
        content.categoryIdentifier = Self.knockCategoryIdentifier
        content.userInfo = [Self.knockSenderIdKey: knock.senderId]

        let request = UNNotificationRequest(
            identifier: "knock-\(knock.senderId)",
            content: content, trigger: nil
        )
        UNUserNotificationCenter.current().add(request)
    }

    public func updateBadgeCount(_ count: Int) {
        guard notificationsAvailable else { return }
        UNUserNotificationCenter.current().setBadgeCount(count)
    }

    public func clearNotifications(for conversationId: String) {
        guard notificationsAvailable else { return }
        UNUserNotificationCenter.current().removeDeliveredNotifications(
            withIdentifiers: [conversationId]
        )
    }

    public func registerCategories() {
        guard notificationsAvailable else { return }
        let replyAction = UNTextInputNotificationAction(
            identifier: "REPLY", title: "Reply",
            options: [], textInputButtonTitle: "Send",
            textInputPlaceholder: "Type a reply..."
        )
        let markReadAction = UNNotificationAction(
            identifier: "MARK_READ", title: "Mark as Read", options: []
        )
        let muteAction = UNNotificationAction(
            identifier: "MUTE", title: "Mute Group", options: []
        )
        let acceptAction = UNNotificationAction(
            identifier: "ACCEPT", title: "Accept", options: .foreground
        )
        let declineAction = UNNotificationAction(
            identifier: "DECLINE", title: "Decline", options: .destructive
        )

        let messageCategory = UNNotificationCategory(
            identifier: "MESSAGE",
            actions: [replyAction, markReadAction],
            intentIdentifiers: []
        )
        let groupCategory = UNNotificationCategory(
            identifier: "GROUP_MESSAGE",
            actions: [replyAction, muteAction],
            intentIdentifiers: []
        )
        let inviteCategory = UNNotificationCategory(
            identifier: "GROUP_INVITE",
            actions: [acceptAction, declineAction],
            intentIdentifiers: []
        )
        let syncCategory = UNNotificationCategory(
            identifier: "SYNC", actions: [], intentIdentifiers: []
        )

        UNUserNotificationCenter.current().setNotificationCategories([
            messageCategory, groupCategory, inviteCategory, syncCategory,
        ])
    }
}
