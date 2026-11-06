import Foundation
import Observation

/// The two share routes' staged targets — the hand-off between the one FaunaKit
/// door (``ConsentHandoff``) and the pages the routes land on
/// (`docs/goal/architecture/apps/macos.md` § Sync → *Finder's Share entry*,
/// link 4; linux's `open_share_route` pair is the same shape):
///
/// - `fauna://share-link?folder=<id>&path=<rel>` — the Media explorer re-reads
///   the aggregate, locates the file through the shared
///   `MediaMachine.locatePath(folderId:path:)`, opens its `media-item-detail`
///   and the machine's create surface (`openShareCreate`).
/// - `fauna://folder-share?folder=<id>` — the Folders page expands that owner
///   row and opens its `folder-share-button` flow (the share sheet).
///
/// Navigation only: nothing is minted or granted, and a target this account
/// cannot see degrades to the page with nothing open. One pending route at a
/// time — the user's last link is the one they mean, so staging either clears
/// the other. Process-wide singleton for the reason ``MediaFileLocate`` is.
@Observable
@MainActor
public final class ShareRouteOpen {
    public static let shared = ShareRouteOpen()

    /// The staged `share-link` target, for the Media explorer.
    public private(set) var shareLink: (folderId: Int64, path: String)?
    /// The staged `folder-share` set id, for the Folders page.
    public private(set) var folderShare: Int64?
    /// Bumped on every stage — what an already-mounted page observes.
    public private(set) var generation = 0

    /// Internal, not private: the unit tests build their own instance.
    init() {}

    /// Whether a staged route still waits for its page.
    public var isPending: Bool { shareLink != nil || folderShare != nil }

    func stageShareLink(folderId: Int64, path: String) {
        shareLink = (folderId, path)
        folderShare = nil
        generation += 1
    }

    func stageFolderShare(folderId: Int64) {
        folderShare = folderId
        shareLink = nil
        generation += 1
    }

    /// The Media explorer takes the staged file once its machine exists.
    public func takeShareLink() -> (folderId: Int64, path: String)? {
        defer { shareLink = nil }
        return shareLink
    }

    /// The Folders page resolves the staged set against its owner rows once the
    /// list is loaded: the matching row's name (the page expands it), or `nil` —
    /// a set this account does not own consumes the route and opens nothing.
    /// The route stays staged (for the row's own section to take,
    /// ``takeFolderShare(id:)``) only while a match exists.
    public func resolveFolderShare(ownerRows: [(id: Int64, name: String)]) -> String? {
        guard let folderShare else { return nil }
        guard let row = ownerRows.first(where: { $0.id == folderShare }) else {
            self.folderShare = nil
            return nil
        }
        return row.name
    }

    /// The expanded row's "Shared with" section opens its share sheet when the
    /// staged set is its own; `true` consumes the route.
    public func takeFolderShare(id: Int64) -> Bool {
        guard folderShare == id else { return false }
        folderShare = nil
        return true
    }
}
