import SwiftUI

@MainActor @Observable
public class EncryptionSettingsVM {
    public var keyPackageCount: Int?
    public var isPublishing = false
    public var errorMessage: String?

    public var isKeyPackageLow: Bool {
        guard let count = keyPackageCount else { return false }
        return count < 5
    }

    private var api: APIClient?
    private var actorId: String?
    private var manager: ConversationsManager?

    public init() {}

    public func configure(api: APIClient, actorId: String, manager: ConversationsManager?) {
        self.api = api
        self.actorId = actorId
        self.manager = manager
    }

    public func loadKeyCount() async {
        guard let api, let actorId else { return }
        do {
            keyPackageCount = try await api.getKeyPackageCount(actorId: actorId)
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    public func refreshKeys() async {
        guard let api, let actorId, let manager else { return }
        isPublishing = true
        errorMessage = nil
        defer { isPublishing = false }
        do {
            // Replenish the one-time pool through the durable session manager
            // (mints on the session MLS engine + notifies the replica autosave),
            // the SAME surface login and web/linux/android drive — NOT a
            // throwaway engine, whose fresh private init keys would exist
            // nowhere durable and a provider swap wipes, stranding peers
            // (docs/goal/behavior/devices.md § Cross-device MLS group-state
            // sync). The pre-login bare manager no-ops (returns 0) — fine.
            _ = try await manager.ensureKeypackages(target: Self.keypackageTarget)
            keyPackageCount = try await api.getKeyPackageCount(actorId: actorId)
        } catch {
            errorMessage = DisplayError.http(error)
        }
    }

    /// One-time key packages to keep published on the nest (mirrors the shared
    /// Rust `fauna_conversations::KEYPACKAGE_TARGET` = 20; web/linux/android
    /// mirror it too).
    private static let keypackageTarget: UInt64 = 20
}
