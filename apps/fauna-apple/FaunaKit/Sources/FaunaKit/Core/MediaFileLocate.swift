import Foundation
import Observation

/// A pending "open this file" request by its durable `(folder_id, path_hash)`
/// identity — the `search-result-item` File-arm deep link
/// (`ui/search.md` § Where logic lives → Result navigation (deep link)).
///
/// Sibling of `MediaDeepOpen` (the FP-context version-history door), which
/// keys on folder NAME + relative path instead — that pair is renameable, so
/// it cannot back a durable search-index identity (the same id-space warning
/// `ui/search.md`'s own File-arm section states: carrying the name would move
/// every file identity the moment its set is renamed). This type keys on the
/// wire's actual identity pair (`FolderSummary.id` + `blake3(normalized
/// relative path)`) instead.
///
/// Process-wide singleton for the same reason `MediaDeepOpen` is: one pending
/// open at a time is exactly the deep-link semantic, and both platform shells
/// + the shared explorer reach it without threading a binding through every
/// navigation layer.
@Observable
@MainActor
public final class MediaFileLocate {
    public static let shared = MediaFileLocate()

    /// The staged target.
    public private(set) var pending: (folderId: Int64, pathHash: String)?

    /// Bumped on every `stage` — what an ALREADY-MOUNTED explorer observes to
    /// consume a new link. Machine readiness alone fires only on a fresh mount;
    /// iOS's search is an overlay above the tab content, so a result activated
    /// while Media is already the visible tab lands on a page whose machine
    /// has long been configured, and nothing else would consume it.
    public private(set) var generation = 0

    private init() {}

    /// Stage a target (replacing any previous one — last link wins).
    public func stage(folderId: Int64, pathHash: String) {
        pending = (folderId, pathHash)
        generation += 1
    }

    /// Actively resolve the staged target via `MediaMachine.locateFile` — an
    /// aggregate-wide lookup independent of the page's active
    /// `media-folder-filter`/sort, so (unlike `MediaDeepOpen`'s passive
    /// snapshot-match) this cannot silently miss a file the current view has
    /// filtered out. `nil` machine (not configured yet) leaves the request
    /// pending for a later retry; a machine that genuinely can't find the
    /// file (deleted, renamed, or in a set this actor can no longer see)
    /// consumes the request and degrades to a no-op, the same posture every
    /// other stale-id open takes.
    public func consume(machine: MediaMachine?) -> MediaItemSummary? {
        guard let pending else { return nil }
        guard let machine else { return nil }
        self.pending = nil
        return machine.locateFile(folderId: pending.folderId, pathHash: pending.pathHash)
    }
}
