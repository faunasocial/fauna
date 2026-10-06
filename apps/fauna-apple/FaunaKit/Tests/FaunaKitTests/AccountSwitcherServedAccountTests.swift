import Testing
import Foundation
@testable import FaunaKit

/// The switcher keys "the account in use" on the account this window SERVES,
/// never the registry's active pointer, and remove-account refuses that account
/// (`account-scoping.md` § Concurrent instances → *Remove-account also refuses
/// the account THIS instance serves*).
///
/// The shape these pin is macOS's bound secondary: it serves B while the
/// registry's active account is A. Keyed on the registry, its switcher offered
/// B for removal, and the remove erased the stores the window was running from.
///
/// Hermetic: `FAUNA_E2E_BRIDGE` forces `KeychainStore` into its in-memory mode
/// (same idiom as `FaunaAccountsDeviceIdTests`).
@MainActor
struct AccountSwitcherServedAccountTests {
    private static let secretA = String(repeating: "11", count: 32)
    private static let secretB = String(repeating: "22", count: 32)

    /// Registers A then B, leaves A registry-active, and returns both ids.
    private func twoAccounts(_ keychain: KeychainStore) throws -> (a: String, b: String) {
        let registry = FaunaAccounts.registry(keychain: keychain)
        let a = try registry.addAccount(secretHex: Self.secretA, nestUrl: "https://a.example", deviceId: nil)
        let b = try registry.addAccount(secretHex: Self.secretB, nestUrl: "https://b.example", deviceId: nil)
        try registry.setActive(actorId: a)
        return (a, b)
    }

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

    /// A window serving B marks B as the account in use (no switch, no remove),
    /// and A (the registry's active account) as an ordinary removable row.
    @Test func theActiveRowIsTheServedAccountNotTheRegistryActiveOne() throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let (a, b) = try twoAccounts(keychain)

        let vm = AccountSwitcherVM(servedActorId: { b })
        vm.reload()

        let rows = Dictionary(uniqueKeysWithValues: vm.accounts.map { ($0.actorId, $0) })
        #expect(vm.isActive(try #require(rows[b])), "the served account is the one in use")
        #expect(!vm.isActive(try #require(rows[a])),
                "the registry's active account is not this window's account")
    }

    /// Before a session is admitted, nothing is served yet, so the registry's
    /// active account is the one in use. That is what a primary resolves to.
    @Test func withNoServedAccountTheRegistryActiveOneIsInUse() throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let (a, _) = try twoAccounts(keychain)

        let vm = AccountSwitcherVM(servedActorId: { nil })
        vm.reload()

        #expect(vm.activeActorId == a)
    }

    /// ⚠ The self-inflicted erase. The remove itself refuses, not only the
    /// button: B stays registered, and the refusal paints the served-here line.
    @Test func removingTheServedAccountIsRefusedAndTouchesNothing() throws {
        let keychain = makeHermetic()
        defer { release(keychain) }
        let (_, b) = try twoAccounts(keychain)

        let vm = AccountSwitcherVM(servedActorId: { b })
        vm.reload()
        vm.remove(actorId: b)

        #expect(FaunaAccounts.registry(keychain: keychain).list().contains { $0.actorId == b },
                "a refused remove must leave the account registered")
        #expect(vm.error == L.lookup("settings.remove_account_blocked_this_window"),
                "the refusal names its own remedy: close THIS window")
    }
}
