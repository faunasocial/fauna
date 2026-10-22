import FaunaFFISwift
import Foundation

/// The payload of `.faunaFolderDeviceActivityChanged` — the pair a
/// `fauna.sync.changed` push names its set by. `folder` is the plaintext name
/// and is BLANK on a sealed set's nudge, which carries only `folderHash`
/// (path-sealing.md § the set-name plane, *Outbound carriers*), so a receiver
/// never compares `folder` itself: it asks `names(_:)`, which is the shared
/// `SyncChangedPayload::names_set` match every app makes.
public struct FolderDeviceActivityChange {
    public let folder: String
    public let folderHash: Data?

    public init(folder: String, folderHash: Data?) {
        self.folder = folder
        self.folderHash = folderHash
    }

    /// Whether the push names the set this app knows as `name`.
    public func names(_ name: String) -> Bool {
        syncChangedNamesSet(folder: folder, folderHash: folderHash, name: name)
    }
}
