import Testing
import Foundation
@testable import FaunaKit

/// `FaunaAccounts.deviceId(forActorId:)` — the one FaunaKit call site every apple
/// device-id mint goes through
/// (`docs/goal/architecture/apps/sync-agent-credentials.md` § Credential model, the
/// RULED 2026-09-20 block; § Implementation status today → *The derived named-row
/// id*, the secret-store face paragraph). `OnboardingVM`'s logged-in handoff and
/// both app shells' session-patch door route through it; these tests pin the
/// contract at the one place Swift owns it — the `KeychainSecretStore` seam.
///
/// Hermetic: `FAUNA_E2E_BRIDGE` forces `KeychainStore` into its in-memory mode, so
/// no real Keychain is touched (same idiom as `KeychainSecretStoreTests`).
@MainActor
struct FaunaAccountsDeviceIdTests {
    /// 32 bytes of hex — the same fixture-secret shape `KeychainSecretStoreTests`
    /// and the shared crate's own tests use.
    private static let secretA = String(repeating: "11", count: 32)
    private static let secretB = String(repeating: "22", count: 32)

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

    /// The named-row guarantee the 2026-09-20 ruling exists for: a sign-out
    /// (`registry.clearAll()` — what `StatusVM.signOut` calls) never names the
    /// install-scoped `install/device_secret` slot, so the SAME actor reads back
    /// the SAME derived device id on the next read. Without this, a sign-out →
    /// sign-in registers a brand-new `sync_devices` row every cycle.
    @Test func aSignOutThenSignInComesBackToTheSameDeviceId() throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let actorId = try actor_id_from_secret(Self.secretA)

        let first = try FaunaAccounts.deviceId(forActorId: actorId, keychain: keychain)

        // Sign-out shaped: the registry's own scoped erase, never the wholesale
        // `KeychainStore.deleteAll()` a factory reset uses.
        _ = FaunaAccounts.registry(keychain: keychain).clearAll()

        let second = try FaunaAccounts.deviceId(forActorId: actorId, keychain: keychain)
        #expect(first == second)
    }

    /// Decision 5 (`sync-agent-credentials.md` § Credential model): two accounts
    /// on one install never share an id — the derivation is actor-salted, not
    /// install-flat, so the secret never leaves the machine but two accounts
    /// still get unlinkable ids.
    @Test func twoAccountsOnOneInstallNeverShareADeviceId() throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let actorA = try actor_id_from_secret(Self.secretA)
        let actorB = try actor_id_from_secret(Self.secretB)

        let idA = try FaunaAccounts.deviceId(forActorId: actorA, keychain: keychain)
        let idB = try FaunaAccounts.deviceId(forActorId: actorB, keychain: keychain)
        #expect(idA != idB)
    }

    /// The contrast that makes the first test meaningful: unlike sign-out's
    /// scoped `clearAll()`, a factory reset's wholesale `KeychainStore
    /// .deleteAll()` (what both `resetToFactory` implementations call) DOES
    /// sweep the install secret — and that is the ruling's intended
    /// factory-reset behaviour ("uninstalling or a factory reset of the app's
    /// own data ends it"), not a regression this row should paper over.
    @Test func aFactoryResetEndsTheInstallSecretUnlikeASignOut() throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let actorId = try actor_id_from_secret(Self.secretA)

        let first = try FaunaAccounts.deviceId(forActorId: actorId, keychain: keychain)
        keychain.deleteAll()
        let second = try FaunaAccounts.deviceId(forActorId: actorId, keychain: keychain)
        #expect(first != second)
    }
}
