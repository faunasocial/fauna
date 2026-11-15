import FaunaFFISwift
import Foundation
import Observation

// EXCISED BY `FAUNA_EXCISE_P2P_SHARE` (the reason is written once, at the top of
// `SharePlaneModel.swift`): a group set is a share-plane set.
#if !FAUNA_EXCISE_P2P_SHARE && os(iOS)
    /// iOS's group-set review list (`group-set.md` § Build design — the group
    /// set, decision 9; `folders.md` § Conflicts): the rows `FoldersContent`
    /// reads through `setReviewRows`, and the *use the other version* gesture
    /// behind `useRetainedVersion`.
    ///
    /// A phone runs no sync agent, and the set's engine is the File Provider
    /// extension's — another process, the set's one writer (decision 11). So
    /// the read is the app's own, at rest: the set's state DB in the app-group
    /// container, through the shared mapping (`syncSetReviewRows`), on a poll,
    /// because the fold records a row with no gesture of this device. The
    /// restore crosses to the extension's host over the service channel the
    /// share door uses (`FaunaShareIngestService.restoreRetainedVersion`);
    /// no engine is ever built in the app.
    @MainActor @Observable
    public final class GroupSetReviewRows {
        /// How often the page re-reads while it is shown: a cheap WAL read.
        static let pollInterval: Duration = .seconds(2)

        private var rows: [String: [ConflictSummary]] = [:]
        /// The sets the page asked about, with the label it shows each under —
        /// noted while the body reads, so it is not observed.
        @ObservationIgnored private var asked: [String: String] = [:]

        public init() {}

        /// The set's rows as last read, under `setLabel`. Synchronous and
        /// cheap: the page's body calls it.
        public func setReviewRows(folderId: String, setLabel: String) -> [ConflictSummary] {
            asked[folderId] = setLabel
            return rows[folderId] ?? []
        }

        /// Re-read every asked set until the task is cancelled (the page goes).
        public func poll(actorIdHex: String) async {
            while !Task.isCancelled {
                await refresh(actorIdHex: actorIdHex)
                try? await Task.sleep(for: Self.pollInterval)
            }
        }

        /// One read of every asked set. A read that fails keeps the rows last
        /// read — a review row is never lost to a busy DB.
        public func refresh(actorIdHex: String) async {
            let asked = self.asked
            guard !asked.isEmpty,
                let stateDir = SyncStateDir.resolve(actorIdHex: actorIdHex, in: .container)
            else { return }
            let read = await Task.detached { () -> [String: [ConflictSummary]] in
                var read: [String: [ConflictSummary]] = [:]
                for (folderId, label) in asked {
                    do {
                        read[folderId] = try syncSetReviewRows(
                            stateDir: stateDir.path, folderId: folderId, setLabel: label)
                    } catch {
                        logMessage(
                            level: .warn, target: "fauna.fileprovider",
                            message: "[fp] review rows read for \(folderId) failed: \(error)")
                    }
                }
                return read
            }.value
            for (folderId, fresh) in read where rows[folderId] != fresh {
                rows[folderId] = fresh
            }
        }

        /// *Use the other version* on `conflictId` of the set `folderId`, on
        /// the extension's engine, then re-read so the used row goes. A throw
        /// is the host's refusal (the row stands) or no domain for the set.
        public func useRetainedVersion(
            actorIdHex: String, folderId: String, conflictId: Int64
        ) async throws {
            try await Task.detached {
                guard let host = FileProviderReplicaHosts(actorIdHex: actorIdHex).host(folderId: folderId)
                else {
                    throw FfiError.General(msg: "this folder is not in Files on this device")
                }
                try host.restoreRetainedVersion(conflictId: conflictId)
            }.value
            await refresh(actorIdHex: actorIdHex)
        }
    }
#endif
