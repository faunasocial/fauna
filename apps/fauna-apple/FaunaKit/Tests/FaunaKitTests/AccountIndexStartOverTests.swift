import FaunaFFISwift
import Foundation
import Testing
@testable import FaunaKit

/// The unreadable-index floor's confirm asks `start_over_blocked` before its
/// erase (`account-scoping.md` § Concurrent instances → *An erase refuses
/// while a sibling serves the account*): beside a sibling serving an account
/// the erase reaches, it paints the start-over line and erases nothing;
/// alone, it runs the erase as before.
///
/// Hermetic: temp bases for both roots the erase sweeps, and a private
/// in-memory secret store under the registry — never the developer's real
/// Application Support root or keychain.
@Suite struct AccountIndexStartOverTests {
    /// A registry-free secret store, so the suite shares no process-wide
    /// keychain double with any other.
    private final class MemorySecretStore: FfiSecretStore, @unchecked Sendable {
        private var values: [String: String] = [:]
        private let lock = NSLock()
        func get(key: String) -> String? { lock.withLock { values[key] } }
        func set(key: String, value: String) { lock.withLock { values[key] = value } }
        func delete(key: String) { lock.withLock { _ = values.removeValue(forKey: key) } }
    }

    /// One device: an install base, a store-root container, a registry naming
    /// one account, and that account's scope dir with a file in it.
    private struct Device {
        let root: URL
        let base: URL
        let container: URL
        let registry: FfiAccountRegistry
        let actor: String

        init() throws {
            root = FileManager.default.temporaryDirectory
                .appendingPathComponent("fauna-start-over-\(UUID().uuidString)", isDirectory: true)
            base = root.appendingPathComponent("base", isDirectory: true)
            container = root.appendingPathComponent("container", isDirectory: true)
            try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
            try FileManager.default.createDirectory(at: container, withIntermediateDirectories: true)
            registry = FfiAccountRegistry.newWithLockDir(store: MemorySecretStore(), lockDir: base.path)
            actor = try registry.addAccount(
                secretHex: String(repeating: "33", count: 32), nestUrl: "https://a.example", deviceId: nil)
            let scope = base.appendingPathComponent(actor, isDirectory: true)
            try FileManager.default.createDirectory(at: scope, withIntermediateDirectories: true)
            try Data("state".utf8).write(to: scope.appendingPathComponent("mls.db"))
        }

        var scopeExists: Bool {
            FileManager.default.fileExists(atPath: base.appendingPathComponent(actor).path)
        }

        var registered: Bool { registry.list().contains { $0.actorId == actor } }

        /// Another Fauna window serving `actor`, as apple takes its lock.
        func sibling() throws -> FfiAccountInstanceLock {
            let lock = try #require(acquireAccountInstanceLockShared(
                stateBase: base.path, actorIdHex: actor, storeContainerDir: container.path))
            #expect(lock.isHeld(), "a genuinely held lock, not the degrade")
            return lock
        }

        /// The confirm, with the floor's real two-half erase over this device's
        /// bases: the credential namespace and every actor scope.
        @MainActor
        func confirm() async -> (line: String?, erased: Bool) {
            var erased = false
            let line = await AccountIndexStartOver.confirm(
                registry: registry, baseDir: base.path, storeContainerDir: container.path,
                ownLock: nil
            ) {
                erased = true
                _ = registry.clearAll()
                _ = accountStateEraseAllScopes(baseDir: base.path, storeContainerDir: container.path)
            }
            return (line, erased)
        }

        func remove() { try? FileManager.default.removeItem(at: root) }
    }

    @Test @MainActor func aSiblingServingAnAccountRefusesAndErasesNothing() async throws {
        let device = try Device()
        defer { device.remove() }
        let sibling = try device.sibling()

        let outcome = await device.confirm()

        #expect(outcome.line == L.onboarding.launch.indexMalformedResetBlockedOtherWindow,
                "the refusal paints the start-over line, not sign-out's")
        #expect(!outcome.erased, "the erase never ran")
        #expect(device.registered, "the registry is intact — no keychain clear")
        #expect(device.scopeExists, "the account's scope dir is intact")
        withExtendedLifetime(sibling) {}
    }

    @Test @MainActor func aloneTheStartOverErasesAsBefore() async throws {
        let device = try Device()
        defer { device.remove() }

        let outcome = await device.confirm()

        #expect(outcome.line == nil)
        #expect(outcome.erased)
        #expect(!device.registered, "the credential namespace is cleared")
        #expect(!device.scopeExists, "the account's scope dir is erased")
    }

    /// Once the other window closes, pressing the confirm again is the whole
    /// remedy.
    @Test @MainActor func closingTheSiblingLetsTheSameConfirmProceed() async throws {
        let device = try Device()
        defer { device.remove() }
        var sibling: FfiAccountInstanceLock? = try device.sibling()

        #expect(await device.confirm().line != nil)
        sibling = nil
        _ = sibling

        let outcome = await device.confirm()
        #expect(outcome.line == nil)
        #expect(!device.scopeExists)
    }
}
