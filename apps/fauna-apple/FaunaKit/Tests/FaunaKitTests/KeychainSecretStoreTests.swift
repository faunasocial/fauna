import Testing
import Foundation
@testable import FaunaKit

/// `KeychainSecretStore` is apple's only foreign seam into the shared account registry
/// (`docs/goal/architecture/long-term-store.md` § Multi-account evolution → Shared seam),
/// so these tests pin the one thing Swift is still responsible for — **every logical key is
/// stored verbatim** — and, over the real FFI, the registry moments apple's wizard, launch
/// and views now route through instead of the retired single-slot rows
/// (`long-term-store.md` § Downgrade mirror + abandoned-append recovery, RETIRED 2026-09-24).
/// The windows twin is `AccountRegistryStoreTests.cs`.
///
/// Hermetic: `FAUNA_E2E_BRIDGE` forces `KeychainStore` into its in-memory mode, so no real
/// Keychain is touched (same idiom as `KeychainAccessibilityTests`).
@MainActor
struct KeychainSecretStoreTests {
    /// 32 bytes of hex — the same fixture secret the shared crate's tests use.
    private static let secretA = String(repeating: "11", count: 32)
    private static let secretB = String(repeating: "22", count: 32)

    private func makeHermetic() -> (KeychainStore, KeychainSecretStore) {
        setenv("FAUNA_E2E_BRIDGE", "1", 1)
        let keychain = KeychainStore()
        keychain.deleteAll()
        return (keychain, KeychainSecretStore(keychain: keychain))
    }

    private func release(_ keychain: KeychainStore) {
        keychain.deleteAll()
        unsetenv("FAUNA_E2E_BRIDGE")
    }

    // MARK: - The map

    /// Registry-native keys are unbounded (`fauna/index` + a triple per actor) and must be
    /// stored under their logical name verbatim — the passthrough that lets shared Rust add
    /// rows without an apple change.
    @Test func registryKeysPassThroughVerbatim() {
        let (keychain, store) = makeHermetic()
        defer { release(keychain) }

        store.set(key: "fauna/index", value: #"{"active":"aa","accounts":[]}"#)
        store.set(key: "fauna/aabbcc/secret", value: Self.secretA)

        #expect(keychain.load(rawKey: "fauna/index") == #"{"active":"aa","accounts":[]}"#)
        #expect(keychain.load(rawKey: "fauna/aabbcc/secret") == Self.secretA)
        #expect(store.get(key: "fauna/aabbcc/secret") == Self.secretA)
        #expect(store.get(key: "fauna/nonexistent") == nil)
    }

    /// The pre-registry native rows are retired, not bridged: an identity sitting only in
    /// the old `secret_key` / `node_url` rows is invisible to the registry — no active
    /// account, no session material — so launch routes to the wizard. The 2026-09-24
    /// baseline reset removed this compat outright (no installation predates it).
    @Test func thePreRegistryNativeRowsNoLongerResolve() {
        let (keychain, store) = makeHermetic()
        defer { release(keychain) }

        try? keychain.save(rawKey: "secret_key", value: Self.secretA)
        try? keychain.save(rawKey: "node_url", value: "https://nest.example")

        let registry = FfiAccountRegistry(store: store)
        #expect(registry.active() == nil)
        #expect(registry.list().isEmpty)
        let actorA = try? actor_id_from_secret(Self.secretA)
        #expect(actorA.flatMap { registry.sessionMaterial(actorId: $0) } == nil)
    }

    // MARK: - The wizard's registry moments

    /// Moment 1 on a first run registers, reads back and activates the identity — and the
    /// session material resolves it, so the wizard's own later reads need no single slot.
    @Test func confirmIdentityRegistersAndActivatesOnAFirstRun() throws {
        let (keychain, store) = makeHermetic()
        defer { release(keychain) }

        let registry = FfiAccountRegistry(store: store)
        let actor = try registry.confirmIdentity(secretHex: Self.secretA, append: false)

        #expect(registry.active() == actor)
        #expect(registry.sessionMaterial(actorId: actor)?.secretHex == Self.secretA)
    }

    /// Moment 1 in append mode writes NOTHING (`onboarding.md` § Multi-account: an abandoned
    /// append leaves the store untouched) — the live account stays active and the appended
    /// identity is not registered until its own terminal runs.
    @Test func confirmIdentityInAppendModeWritesNothing() throws {
        let (keychain, store) = makeHermetic()
        defer { release(keychain) }

        let registry = FfiAccountRegistry(store: store)
        let live = try registry.confirmIdentity(secretHex: Self.secretA, append: false)
        let before = registry.list().map(\.actorId)

        let appended = try registry.confirmIdentity(secretHex: Self.secretB, append: true)

        #expect(appended != live)
        #expect(registry.active() == live)
        #expect(registry.list().map(\.actorId) == before)
        #expect(registry.sessionMaterial(actorId: appended) == nil)
    }

    /// Moment 4 records the home nest PER-ACTOR — the only place the next launch's routing
    /// tuple reads it (`onboarding.md` § App-launch routing) — and the session material
    /// carries it back with the device id.
    @Test func persistLoggedInRecordsTheHomeNestPerActor() throws {
        let (keychain, store) = makeHermetic()
        defer { release(keychain) }

        let registry = FfiAccountRegistry(store: store)
        _ = try registry.confirmIdentity(secretHex: Self.secretA, append: false)
        let deviceId = String(repeating: "ab", count: 32)
        let actor = try registry.persistLoggedIn(
            secretHex: Self.secretA, nestUrl: "https://a.example", deviceId: deviceId,
            reachIpv4: nil)

        let material = try #require(registry.sessionMaterial(actorId: actor))
        #expect(material.nestUrl == "https://a.example")
        #expect(material.deviceId == deviceId)
        #expect(registry.active() == actor)
    }

    // MARK: - Wizard-exit slot writers (the production path)

    /// **The `test_smoke_i` regression.** The deferred-DNS exit is reached with NO handle
    /// yet — the wizard goes identity → provisioning → `dns_post_instructions` with no
    /// handle stage — and the resume must still survive a force-quit. The shared per-actor
    /// slot carries opaque JSON, where an empty field is data.
    @Test func deferredDnsSlotWithNoHandleYetSurvivesTheRelaunch() throws {
        let (keychain, store) = makeHermetic()
        defer { release(keychain) }

        _ = try FfiAccountRegistry(store: store).persistAwaitingDns(
            secretHex: Self.secretA,
            nestUrl: "https://nest.example.com",
            handle: "", // the exit has none yet — this is the whole point
            dnsRecordsJson: #"[{"record_type":"A","name":"@","value":"203.0.113.7"}]"#,
            claimCode: "DNS-CODE"
        )

        let rec = try #require(
            FfiAccountRegistry(store: store).launchPersistence().loadAwaitingDns(),
            "an empty handle must not compose the deferred-DNS slot away"
        )
        #expect(rec.handle == "")
        #expect(rec.claimCode == "DNS-CODE")
        #expect(rec.dnsRecordsJson.contains("203.0.113.7"))
    }

    /// The deferred-DNS slot is per-actor, so it survives on a multi-account install — and
    /// the claim terminal clears it.
    @Test func deferredDnsSlotSurvivesOnAMultiAccountInstall() throws {
        let (keychain, store) = makeHermetic()
        defer { release(keychain) }

        let registry = FfiAccountRegistry(store: store)
        _ = try registry.addAccount(secretHex: Self.secretB, nestUrl: "https://b.example", deviceId: nil)
        _ = try registry.persistAwaitingDns(
            secretHex: Self.secretA,
            nestUrl: "https://nest.example.com",
            handle: "alice@example.com",
            dnsRecordsJson: "[]",
            claimCode: "DNS-CODE"
        )

        #expect(registry.launchPersistence().loadAwaitingDns()?.claimCode == "DNS-CODE")

        registry.clearAwaitingDns()
        #expect(registry.launchPersistence().loadAwaitingDns() == nil)
    }

    /// The pending-invite twin, for the same multi-account reason.
    @Test func pendingInviteSlotSurvivesOnAMultiAccountInstall() throws {
        let (keychain, store) = makeHermetic()
        defer { release(keychain) }

        let registry = FfiAccountRegistry(store: store)
        _ = try registry.addAccount(secretHex: Self.secretB, nestUrl: nil, deviceId: nil)
        _ = try registry.persistPendingInvite(
            secretHex: Self.secretA,
            nestUrl: "https://nest.example.com",
            handle: "alice@example.com",
            requestId: "req-1",
            statusJson: "{}"
        )

        #expect(registry.launchPersistence().loadPendingInvite()?.requestId == "req-1")
    }

    // MARK: - Cleanup contract

    /// `deleteAll()` used to iterate `Key.allCases`, which cannot name a registry row — so a
    /// factory reset would have left `fauna/index` and every per-actor secret on the device.
    /// A "factory reset" that silently preserves the account index is the same bug class the
    /// enum sweep was introduced to fix, wearing the multi-account hat.
    @Test func deleteAllSweepsRegistryRowsAndNotJustTypedKeys() {
        let (keychain, store) = makeHermetic()
        defer { release(keychain) }

        try? keychain.save(rawKey: "some_row", value: "1")
        store.set(key: "fauna/index", value: #"{"active":"aa","accounts":[]}"#)
        store.set(key: "fauna/aabbcc/secret", value: Self.secretA)

        keychain.deleteAll()

        #expect(keychain.load(rawKey: "some_row") == nil)
        #expect(keychain.load(rawKey: "fauna/index") == nil, "the account index must not survive a reset")
        #expect(keychain.load(rawKey: "fauna/aabbcc/secret") == nil, "per-actor secrets must not survive a reset")
    }
}
