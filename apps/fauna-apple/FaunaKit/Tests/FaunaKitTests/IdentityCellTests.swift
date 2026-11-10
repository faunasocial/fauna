import Testing
import Foundation
@testable import FaunaKit

/// The Settings Status identity cell and the launch surfaces' identity line
/// read off the registry's server-data cache (`settings.md` § Layout & flow
/// item 1; `onboarding.md` § App-launch routing → *The cached identity on the
/// launch surfaces*): absent while the cache holds no handle, `handle@domain`
/// and the bare tier once it does.
///
/// Hermetic: `FAUNA_E2E_BRIDGE` forces `KeychainStore` into its in-memory mode
/// (same idiom as `SelfIdentityFollowTests`).
@MainActor
@Suite(.sharedKeychain) struct IdentityCellTests {
    private static let secret = String(repeating: "44", count: 32)

    private func makeHermetic() -> KeychainStore {
        setenv("FAUNA_E2E_BRIDGE", "1", 1)
        let keychain = KeychainStore()
        keychain.deleteAll()
        return keychain
    }

    private func release(_ keychain: KeychainStore) {
        keychain.deleteAll()
        unsetenv("FAUNA_E2E_BRIDGE")
    }

    @Test func absentWhileTheCacheHoldsNoHandle() throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let registry = FaunaAccounts.registry(keychain: keychain)
        let actor = try registry.addAccount(
            secretHex: Self.secret, nestUrl: "https://a.example", deviceId: nil)

        #expect(IdentityCell.read(actorId: actor, registry: registry) == nil)
        #expect(IdentityCell.read(actorId: nil, registry: registry) == nil)
        #expect(FaunaAccounts.launchIdentityLine(keychain: keychain) == nil)
    }

    @Test func paintsTheCachedAddressAndTier() throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let registry = FaunaAccounts.registry(keychain: keychain)
        let actor = try registry.addAccount(
            secretHex: Self.secret, nestUrl: "https://a.example", deviceId: nil)
        try registry.setActive(actorId: actor)
        try registry.updateCache(actorId: actor, handle: "alice", domain: "a.example", tier: "free")

        let cell = try #require(IdentityCell.read(actorId: actor, registry: registry))
        #expect(cell.handle == "alice@a.example")
        #expect(cell.tier == "free")

        let line = try #require(FaunaAccounts.launchIdentityLine(keychain: keychain))
        #expect(line.contains("alice@a.example"))
        #expect(line.contains("free"))
    }

    @Test func aHandleWithNoTierPaintsTheHandleAlone() throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let registry = FaunaAccounts.registry(keychain: keychain)
        let actor = try registry.addAccount(
            secretHex: Self.secret, nestUrl: "https://a.example", deviceId: nil)
        try registry.updateCache(actorId: actor, handle: "bob", domain: "a.example", tier: nil)

        let cell = try #require(IdentityCell.read(actorId: actor, registry: registry))
        #expect(cell.handle == "bob@a.example")
        #expect(cell.tier == nil)
    }
}
