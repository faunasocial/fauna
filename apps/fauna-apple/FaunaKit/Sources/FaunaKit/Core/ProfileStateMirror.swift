import Foundation

/// The profile page's cross-app state-protocol read — ui.yaml's `profile` state
/// fields: `is_self`, `actor_id` and `home_nest_url` (the actor-home slot the
/// follow, the knock and the remote header read all go to; `null` = this nest,
/// the read-back of the nav patch's field of the same name).
///
/// The page owns the slot (``ProfileKnockVM``), the app shell serializes state, so
/// the page mirrors the three values here on every open and after every header
/// read — a plain field read for the state provider (e2e convention 11: no
/// blocking I/O, no round trip). Same precedent as ``FeedVM/lastLoadedPosts``.
/// `nil` actor = the profile page is not mounted.
@MainActor
public enum ProfileStateMirror {
    public private(set) static var isSelf = true
    public private(set) static var actorId: String?
    public private(set) static var homeNestUrl: String?

    public static func publish(isSelf: Bool, actorId: String, homeNestUrl: String?) {
        self.isSelf = isSelf
        self.actorId = actorId
        self.homeNestUrl = isSelf ? nil : homeNestUrl
    }

    public static func clear() {
        isSelf = true
        actorId = nil
        homeNestUrl = nil
    }

    /// The `data.profile` payload both app shells serialize.
    public static func state() -> [String: Any] {
        [
            "is_self": isSelf,
            "actor_id": actorId as Any? ?? NSNull(),
            "home_nest_url": homeNestUrl as Any? ?? NSNull(),
        ]
    }
}
