import Combine
import FaunaFFISwift
import Foundation
import Testing

@testable import FaunaKit

/// The Folders page's device-activity gate matches a `fauna.sync.changed` push
/// to the expanded set through the shared rule, so a sealed set's nudge — blank
/// `folder`, only `folderHash` — still refreshes it (path-sealing.md § the
/// set-name plane, *Outbound carriers*; file-sync.md § Remote-change nudge).
@Suite struct FolderDeviceActivityChangeTests {
    @Test func aBlankFolderWithTheSetsHashNamesTheSet() {
        let hash = webdavSetNameHash(setName: "docs")
        let change = FolderDeviceActivityChange(folder: "", folderHash: hash)
        #expect(change.names("docs"))
        #expect(!change.names("pics"))
    }

    @Test func aPlaintextNudgeStillMatchesByName() {
        let change = FolderDeviceActivityChange(folder: "docs", folderHash: nil)
        #expect(change.names("docs"))
        #expect(!change.names("pics"))
    }

    /// The notification the push observer posts carries the pair intact, so the
    /// `onFolderDeviceActivityChanged` receiver can gate on it.
    @Test func theNotificationCarriesTheHashToTheReceiver() {
        var received: FolderDeviceActivityChange?
        let sub = NotificationCenter.default
            .publisher(for: .faunaFolderDeviceActivityChanged)
            .sink { received = $0.object as? FolderDeviceActivityChange }
        NotificationCenter.default.post(
            name: .faunaFolderDeviceActivityChanged,
            object: FolderDeviceActivityChange(
                folder: "", folderHash: webdavSetNameHash(setName: "docs")))
        #expect(received?.names("docs") == true)
        sub.cancel()
    }
}
