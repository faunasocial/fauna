import Foundation

public struct NotificationItem: Identifiable {
    public let id: String
    /// What the row paints — the shared `notification_text_for` decision,
    /// already resolved: the localized catalog sentence for a key this
    /// build's catalog carries, else the raw `summary` verbatim
    /// (`notifications.md` § Localized body). Never re-derived here.
    public let body: String
    /// The nest's own English rendering of the same row — carried alongside
    /// `body` (never painted directly) so a reconstructed `FfiNotifItem`
    /// (`NotificationOpen.swift`'s router call) can hand back the row's real
    /// summary rather than `body`'s already-resolved text, mirroring web's own
    /// `UnifiedNotification.summary` field (`notifications.ts`) — linux and
    /// android need no equivalent since they resolve display text on demand
    /// off a type that already carries `summary` (`NotifItem` / `FfiNotifItem`
    /// itself), rather than mapping into a pre-resolved app model first.
    public let summary: String
    public let createdAt: Int
    public let read: Bool
    /// The `notif_type` wire string (`like` / `reply` / `follow` / …) the row's
    /// per-kind icon is derived from — `notifications.md` § Where logic lives.
    /// Carried raw rather than pre-mapped so the classification stays shared
    /// Rust's (`notificationGlyphForType`), not this model's.
    public let notifType: String
    /// The originating protocol (`fauna` / `bluesky` / `nostr` / `activitypub`).
    /// Carried because the shared deep-link router keys on `source` **then**
    /// `notif_type`, never the type alone: a bridged row reuses the native type
    /// vocabulary while its `content_id` is a dedup token, so a type-only match
    /// would deep-link every bridged like to a post that cannot exist
    /// (`notifications.md` § Deep-link destinations).
    public let source: String
    /// The row's content key when it has one — a native `like`'s is the liked
    /// post's id verbatim. Raw, for the same reason as `source`.
    public let contentId: String?
    /// The row's sender when it has one — a `knock` *is* its sender, so this is
    /// that row's whole destination. Raw, for the same reason as `source`.
    public let senderId: String?
    /// What a bridged row is ABOUT, as an AT-URI (the AppView's
    /// `reasonSubject`: the liked / replied-to / quoted post). Raw, for the
    /// router's `External` arm — shared Rust turns it into the post's bsky.app
    /// address, which the app hands to the OS browser; the app never reads it
    /// itself. Dropped here until 2026-09-25, which is why no apple row could
    /// carry the off-app destination.
    public let subjectUri: String?

    public init(id: String, body: String, summary: String = "", createdAt: Int, read: Bool,
                notifType: String = "", source: String = "", contentId: String? = nil,
                senderId: String? = nil, subjectUri: String? = nil) {
        self.id = id
        self.body = body
        self.summary = summary
        self.createdAt = createdAt
        self.read = read
        self.notifType = notifType
        self.source = source
        self.contentId = contentId
        self.senderId = senderId
        self.subjectUri = subjectUri
    }
}
