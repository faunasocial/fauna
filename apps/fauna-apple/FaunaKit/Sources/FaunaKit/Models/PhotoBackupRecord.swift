import SwiftData
import Foundation

@Model
public class PhotoBackupRecord {
    @Attribute(.unique) public var localIdentifier: String
    public var manifestHash: String?
    public var sizeBytes: Int
    public var mediaType: String
    public var creationDate: Date
    public var state: SyncFileState
    public var remotePath: String?

    public init(localIdentifier: String, manifestHash: String? = nil,
         sizeBytes: Int, mediaType: String, creationDate: Date,
         state: SyncFileState = .localOnly, remotePath: String? = nil) {
        self.localIdentifier = localIdentifier
        self.manifestHash = manifestHash
        self.sizeBytes = sizeBytes
        self.mediaType = mediaType
        self.creationDate = creationDate
        self.state = state
        self.remotePath = remotePath
    }

    /// Build the photo-backup `ModelContainer` for `actorIdHex` (`nil` = the
    /// flat, no-actor container used pre-auth). One call site for both a
    /// shell's `init()` and every later rebuild, so the schema can't drift
    /// between the two — was a byte-identical per-shell private static func
    /// on `FaunaApp` (iOS) / `FaunaMacApp` (macOS) until this harvest pass
    /// found it .
    public static func buildModelContainer(actorIdHex: String?) -> ModelContainer {
        let schema = Schema([PhotoBackupRecord.self])
        let url = AccountStateDir.photoBackupStoreURL(actorIdHex: actorIdHex)
        let config = ModelConfiguration(schema: schema, url: url)
        do {
            return try ModelContainer(for: schema, configurations: [config])
        } catch {
            fatalError("Failed to create ModelContainer: \(error)")
        }
    }
}
