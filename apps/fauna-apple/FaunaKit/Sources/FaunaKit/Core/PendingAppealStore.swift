import Foundation

/// The one-shot handoff a `dm-message-withheld-badge` tap leaves for the Moderation
/// queue (`takedown-and-block.md` § Legal takedown → *App render*, the fourth surface).
/// The badge lives on the conversation detail and the appeal form on the queue page —
/// two destinations with their own view models — so the tap stores the queue row's
/// `content_id` here immediately before navigating, and `ModerationQueueVM` consumes it
/// once its queue is loaded and opens the queue's own appeal draft on that row (one
/// flow, the `appeal-*` ids). The twin of android's `PendingAppealStore` and web's
/// `lib/pending-appeal.ts`.
@MainActor
public enum PendingAppealStore {
    private static var pending: String?

    public static func set(_ contentId: String) { pending = contentId }

    /// Returns AND CLEARS the pending `content_id`; `nil` when no badge tap routed here.
    public static func consume() -> String? {
        defer { pending = nil }
        return pending
    }
}

public extension Notification.Name {
    /// A `dm-message-withheld-badge` tap asks the shell to open the Moderation page
    /// (``PendingAppealStore`` already holds the row to appeal). Each app's shell
    /// observes it and selects its Moderation destination.
    static let faunaOpenModerationAppeal = Notification.Name("faunaOpenModerationAppeal")
}
