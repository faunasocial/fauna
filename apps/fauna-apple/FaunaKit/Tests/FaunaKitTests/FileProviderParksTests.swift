import Foundation
import Testing

@testable import FaunaKit

#if canImport(FileProvider)

    /// Headless pins for the app's read of a capability host's park
    /// (`file-sync.md` § Multi-writer shared sets → *Revocation on a capability
    /// host*): the presented sets' park reads folded through the shared
    /// `onDemandParkedPresences`, the page's own `reader` reading taken over the
    /// plan's, and a failed read failing closed. The fold's rule is pinned in
    /// Rust (`fauna_folders_machine::on_demand_presence::parked_presences`);
    /// what stays here is the Swift glue reaching it. State DBs run at injected
    /// temp paths, never the app-group container (testing.md § conventions
    /// point 10).
    @Suite struct FileProviderParksTests {
        private let shared = PresenceSet(
            name: "docs (owner)", setName: "docs", folderId: "local:1",
            thisDeviceAccepts: true, role: .sharedWithMe)
        private let own = PresenceSet(
            name: "pics", folderId: "local:2", thisDeviceAccepts: true, role: .own)

        private struct ReadFailed: Error {}

        @Test func theHostsMarkParksTheSet() {
            let parked = FileProviderCoordinator.parkedFolderIds(
                presented: [shared, own], readerFolderIds: []
            ) { id in
                SetPark(folderId: id, accessRevoked: id == "local:1", unrecorded: 0)
            }
            #expect(parked == ["local:1"])
        }

        @Test func aFailedReadShowsTheSetParked() {
            let parked = FileProviderCoordinator.parkedFolderIds(
                presented: [shared], readerFolderIds: []
            ) { _ in throw ReadFailed() }
            #expect(parked == ["local:1"])
        }

        /// Arm (b): the page's folder-list refresh read the demotion before the
        /// host's next edge — the row reads `reader` while the domain holds
        /// edits the nest never recorded. The plan alone (still `writer`) reads
        /// the same un-recorded edits as an upload in flight, not a park.
        @Test func thePagesReaderReadingCatchesUnsentEditsBeforeTheHostsEdge() {
            let read: (String) throws -> SetPark = { id in
                SetPark(folderId: id, accessRevoked: false, unrecorded: 2)
            }
            #expect(
                FileProviderCoordinator.parkedFolderIds(
                    presented: [shared], readerFolderIds: [], read: read
                ).isEmpty)
            #expect(
                FileProviderCoordinator.parkedFolderIds(
                    presented: [shared], readerFolderIds: ["local:1"], read: read)
                    == ["local:1"])
        }

        @Test func aSetThisDeviceDoesNotPresentIsNeverRead() {
            var reads = 0
            let parked = FileProviderCoordinator.parkedFolderIds(
                presented: [], readerFolderIds: ["local:1"]
            ) { id in
                reads += 1
                return SetPark(folderId: id, accessRevoked: true, unrecorded: 0)
            }
            #expect(parked.isEmpty)
            #expect(reads == 0)
        }

        /// The real FFI read over a state dir the host never wrote: a set with
        /// no DB reads unparked and empty, so it is not warned.
        @Test func aSetTheHostNeverOpenedReadsUnparked() throws {
            let dir = FileManager.default.temporaryDirectory
                .appendingPathComponent("fauna-parks-\(UUID().uuidString)")
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            defer { try? FileManager.default.removeItem(at: dir) }
            let parked = FileProviderCoordinator.parkedFolderIds(
                presented: [shared], readerFolderIds: ["local:1"]
            ) { id in
                try syncSetPark(stateDir: dir.path, folderId: id)
            }
            #expect(parked.isEmpty)
        }
    }
#endif
