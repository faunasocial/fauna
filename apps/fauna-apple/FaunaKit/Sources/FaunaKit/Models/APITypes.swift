import Foundation

// The `NodeInfo`/`NodeRegistration`/`RegistrationResponse` decodes lived here
// for `APIClient`'s `GET /api/v1/{node-info,register}` calls. Both routes are
// gone nest-side (WS-RPC `fauna.{nest.info,account.register}`), the calls with
// them; the dashboard's Version card reads `FfiAdminStatus.version` instead.

// MARK: - Blobs

public struct BlobResponse: Codable {
    public let hash: String
}

// MARK: - Sync

public struct SyncStatus: Codable {
    public let folder: String
    public let sourceOnline: Bool

    enum CodingKeys: String, CodingKey {
        case folder = "folder"
        case sourceOnline = "source_online"
    }
}

// MARK: - Snapshots

public struct SnapshotResponse: Codable, Identifiable, Hashable {
    public let id: Int
    public let fileCount: Int
    public let totalBytes: Int
    public let createdAt: Int
    public let parentId: Int?
    public let tags: [String]?
    public let deviceId: String?

    enum CodingKeys: String, CodingKey {
        case id, tags
        case fileCount = "file_count"
        case totalBytes = "total_bytes"
        case createdAt = "created_at"
        case parentId = "parent_id"
        case deviceId = "device_id"
    }
}

// MARK: - Quota

public struct QuotaUsage: Codable {
    public let usedBytes: Int
    public let maxBytes: Int

    enum CodingKeys: String, CodingKey {
        case usedBytes = "used_bytes"
        case maxBytes = "max_bytes"
    }
}

public struct QuotaDevices: Codable {
    public let used: Int
    public let max: Int
}

public struct QuotaFeatures: Codable {
    public let versionedBackup: Bool
    public let bridges: Bool
    public let maxFeeds: Int

    enum CodingKeys: String, CodingKey {
        case bridges
        case versionedBackup = "versioned_backup"
        case maxFeeds = "max_feeds"
    }
}

public struct QuotaResponse: Codable {
    public let tier: String
    public let inbox: QuotaUsage
    public let storage: QuotaUsage
    public let devices: QuotaDevices
    public let features: QuotaFeatures
}

// MARK: - Chunks

public struct CheckChunksResponse: Codable {
    public let missing: [String]
}
