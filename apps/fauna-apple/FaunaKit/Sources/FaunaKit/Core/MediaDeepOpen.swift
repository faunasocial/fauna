import Foundation
import Observation

/// A pending "open this file's version history" request — the hand-off between
/// a `fauna://folder/<set>?action=versions&path=<rel>` deep link (the FP
/// context action's **Version history** leaf) and the Media explorer, which
/// owns the `media-item-detail` overlay the `file-version-history` surface
/// lives in. The app's URL handler navigates to Media and stages the target
/// here; `MediaExplorerContent` consumes it once the snapshot holds the item
/// (consume-on-match, so a stale request for a since-deleted file simply ages
/// out with the next staging instead of wedging).
///
/// Process-wide singleton by design: one pending open at a time is exactly the
/// deep-link semantic, and both platform shells + the shared explorer reach it
/// without threading a binding through every navigation layer.
@Observable
@MainActor
public final class MediaDeepOpen {
    public static let shared = MediaDeepOpen()

    /// The staged target: folder name + set-relative path.
    public private(set) var pending: (set: String, rel: String)?

    private init() {}

    /// Stage a target (replacing any previous one — last link wins).
    public func stage(set: String, rel: String) {
        pending = (set, rel)
    }

    /// Consume the staged target if `items` contains it; returns the matched
    /// item. Leaves the request pending while the item is absent (the snapshot
    /// may still be loading; a later refresh retries).
    public func consume(matching items: [MediaItemSummary]) -> MediaItemSummary? {
        guard let pending else { return nil }
        guard
            let match = items.first(where: {
                $0.folder == pending.set && $0.path == pending.rel
            })
        else { return nil }
        self.pending = nil
        return match
    }
}
