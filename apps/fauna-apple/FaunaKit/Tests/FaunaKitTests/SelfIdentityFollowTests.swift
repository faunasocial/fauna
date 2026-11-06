import Testing
import Foundation
@testable import FaunaKit

/// The session mint's identity watch writes the account cache
/// (`transport-connection.md` § The dial budget): every identity the
/// subscription yields lands on the account's registry row and on
/// `SessionState.handle`, an empty field never clobbering a cached one, and the
/// loop ends when the subscription does (the session client torn down).
///
/// Hermetic: `FAUNA_E2E_BRIDGE` forces `KeychainStore` into its in-memory mode
/// (same idiom as `AccountSwitcherServedAccountTests`).
@MainActor
@Suite(.sharedKeychain) struct SelfIdentityFollowTests {
    private static let secret = String(repeating: "33", count: 32)

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

    /// A scripted subscription: yields each identity in turn, then `nil`.
    private final class Script {
        var remaining: [LaunchIdentity]
        var calls = 0
        init(_ identities: [LaunchIdentity]) { remaining = identities }
        func next() async -> LaunchIdentity? {
            calls += 1
            return remaining.isEmpty ? nil : remaining.removeFirst()
        }
    }

    @Test func eachMintedIdentityLandsOnTheCacheAndTheSession() async throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let registry = FaunaAccounts.registry(keychain: keychain)
        let actor = try registry.addAccount(
            secretHex: Self.secret, nestUrl: "https://a.example", deviceId: nil)
        let session = SessionState()

        // The first mint, then a server-side rename whose reply leaves domain
        // and tier empty: the rename lands, the cached domain/tier survive.
        let script = Script([
            LaunchIdentity(handle: "alice", domain: "a.example", tier: "free"),
            LaunchIdentity(handle: "alicia", domain: "", tier: ""),
        ])
        await followSelfIdentity(next: { await script.next() }) { identity in
            applySelfIdentity(identity, actorId: actor, registry: registry, session: session)
        }

        let row = try #require(registry.list().first { $0.actorId == actor })
        #expect(row.handle == "alicia")
        #expect(row.domain == "a.example")
        #expect(row.tier == "free")
        #expect(session.handle == "alicia")
        #expect(script.calls == 3, "the loop ends on the subscription's nil")
    }

    @Test func anEmptyHandleNeverClobbersTheCachedOne() async throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let registry = FaunaAccounts.registry(keychain: keychain)
        let actor = try registry.addAccount(
            secretHex: Self.secret, nestUrl: "https://a.example", deviceId: nil)
        try registry.updateCache(actorId: actor, handle: "bob", domain: "a.example", tier: "free")
        let session = SessionState()
        session.handle = "bob"

        applySelfIdentity(
            LaunchIdentity(handle: "", domain: "b.example", tier: ""),
            actorId: actor, registry: registry, session: session)

        let row = try #require(registry.list().first { $0.actorId == actor })
        #expect(row.handle == "bob")
        #expect(row.domain == "b.example")
        #expect(row.tier == "free")
        #expect(session.handle == "bob")
    }
}
