import Foundation

public struct SyncConfiguration {
    public let maxConcurrentUploads: Int
    public let maxInMemoryFileSize: Int

    public init(maxConcurrentUploads: Int, maxInMemoryFileSize: Int) {
        self.maxConcurrentUploads = maxConcurrentUploads
        self.maxInMemoryFileSize = maxInMemoryFileSize
    }

    public static var platformDefault: SyncConfiguration {
        #if os(macOS)
        SyncConfiguration(maxConcurrentUploads: 4, maxInMemoryFileSize: .max)
        #else
        SyncConfiguration(maxConcurrentUploads: 2, maxInMemoryFileSize: 200_000_000)
        #endif
    }
}
