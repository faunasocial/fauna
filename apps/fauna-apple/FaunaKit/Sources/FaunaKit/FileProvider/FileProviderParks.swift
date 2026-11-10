import FaunaFFISwift
import Foundation
import Observation
import SwiftUI

#if canImport(FileProvider)
    import FileProvider

    /// The sets this device presents through a File Provider domain whose
    /// member row heads with `folder-access-revoked-warning` — the capability
    /// host's park, as the app reads it (`file-sync.md` § Multi-writer shared
    /// sets → *Revocation on a capability host*, "What the app shows, and how
    /// the park reaches it"). Keyed by the set's bare `FolderRef` wire string;
    /// `FoldersContent` unions it with the agent's bound-location parks (macOS),
    /// so a set shown in Finder or Files without a bound location is covered.
    ///
    /// The app watches the park, it is never told: the host writes its mark at
    /// its own edge, which no push announces, so this is re-read on the
    /// Folders page's folder-list refresh, when the app comes back to the
    /// foreground, and after every domain reconcile.
    @MainActor @Observable
    final class FileProviderParks {
        static let shared = FileProviderParks()

        private(set) var folderIds: Set<String> = []
        /// The page's own `reader` readings (member rows whose access is not
        /// `writer`), kept for the reconcile's re-read: a demotion has no push,
        /// so the folder-list refresh reads it before the host's next edge.
        private(set) var readerFolderIds: Set<String> = []

        func noteReaders(_ ids: Set<String>) { readerFolderIds = ids }

        func publish(_ ids: Set<String>) {
            guard ids != folderIds else { return }
            folderIds = ids
            logMessage(
                level: .info, target: "fauna.fileprovider",
                message: "[fp] parked domain set(s): \(ids.sorted().joined(separator: ","))")
        }
    }

    extension FileProviderCoordinator {
        /// The shared fold (`onDemandParkedPresences`) over each presented
        /// set's park read, the page's own `reader` reading taken over the
        /// plan's (the android leg's shape). A read that fails shows the set
        /// parked: the warning fails closed.
        static func parkedFolderIds(
            presented: [PresenceSet], readerFolderIds: Set<String>,
            read: (String) throws -> SetPark
        ) -> Set<String> {
            guard !presented.isEmpty else { return [] }
            let presences = presented.map { set -> PresenceSet in
                var set = set
                set.readOnly = set.readOnly || readerFolderIds.contains(set.folderId)
                return set
            }
            let reads = presences.map { set -> SetPark in
                do {
                    return try read(set.folderId)
                } catch {
                    logMessage(
                        level: .warn, target: "fauna.fileprovider",
                        message:
                            "[fp] park read for \(set.folderId) failed — showing the set parked: \(error)")
                    return SetPark(folderId: set.folderId, accessRevoked: true, unrecorded: 0)
                }
            }
            return Set(onDemandParkedPresences(presences: presences, reads: reads))
        }

        /// Re-read the parks and publish them. Reads only — the registered
        /// domains, the plan over them, each presented set's state DB in the
        /// app-group container (where the host writes it, as the removal
        /// gate reads it) — so, like the OS enable notices, it stays outside
        /// the e2e gate on ``reconcile(sets:boundSets:provisioning:)``. With no
        /// domain registered nothing is presented and nothing is read.
        static func refreshParks(
            actorHex: String, boundSets: Set<String>, readerFolderIds: Set<String>,
            sets: @Sendable () async throws -> [PresenceSet]
        ) async {
            await FileProviderParks.shared.noteReaders(readerFolderIds)
            let registered = (try? await FileProviderDomains.list()) ?? []
            guard !registered.isEmpty else {
                await FileProviderParks.shared.publish([])
                return
            }
            do {
                let plan = try self.plan(
                    actorHex: actorHex, sets: try await sets(), boundSets: boundSets,
                    prefs: FileProviderDomainPrefs.shared, registered: registered)
                let presented = (plan.add + plan.keep).map(\.set)
                let stateDir = SyncStateDir.resolve(actorIdHex: actorHex, in: .container)
                let parked = parkedFolderIds(
                    presented: presented, readerFolderIds: readerFolderIds
                ) { folderId in
                    guard let stateDir else { throw FileProviderParkReadError.noContainer }
                    return try syncSetPark(stateDir: stateDir.path, folderId: folderId)
                }
                await FileProviderParks.shared.publish(parked)
            } catch {
                logMessage(
                    level: .warn, target: "fauna.fileprovider",
                    message: "[fp] park read skipped — presence plan failed: \(error)")
            }
        }
    }

    enum FileProviderParkReadError: Error {
        /// The app-group container is unreachable (a packaging bug).
        case noContainer
    }
#endif

/// Keeps the domain parks current on the Folders page: read on the folder-list
/// refresh (keyed on the member rows' readings, so a demotion read by the
/// refresh re-reads at once) and whenever the app returns to the foreground.
struct FolderParkRefresh: ViewModifier {
    let folders: [FolderSummary]
    let boundSets: () -> Set<String>

    /// Member rows whose access is not `writer` — fail-safe like
    /// `binding_section`: an absent access reads reader.
    static func readerFolderIds(_ folders: [FolderSummary]) -> Set<String> {
        Set(
            folders
                .filter { $0.role == "member" && $0.access != "writer" }
                .compactMap(\.folderRef))
    }

    func body(content: Content) -> some View {
        #if canImport(FileProvider)
            let key = folders.map { "\($0.id):\($0.access ?? "")" }.joined(separator: ",")
            content.modifier(
                FolderPresenceRefresh(key: key) { actorHex, sets in
                    await FileProviderCoordinator.refreshParks(
                        actorHex: actorHex, boundSets: boundSets(),
                        readerFolderIds: Self.readerFolderIds(folders), sets: sets)
                })
        #else
            content
        #endif
    }
}
