import Testing
import Foundation
@testable import FaunaKit

/// The append flow's refused add (`long-term-store.md` § Multi-account evolution →
/// *The index is bounded*): a full account list makes `registry.addAccount` throw the
/// shared `add_refused_copy` line as `FfiError.General`, and
/// `completeAppendedAccount` hands it to the switcher (`reportRefused`) instead of only
/// logging it after the wizard has already dismissed.
///
/// Hermetic: `FAUNA_E2E_BRIDGE` forces `KeychainStore` into its in-memory mode.
@MainActor
struct AccountAddRefusedTests {
    private static func secret(_ n: Int) -> String {
        String(format: "%064x", n + 1)
    }

    private func makeHermetic() -> (KeychainStore, FfiAccountRegistry) {
        setenv("FAUNA_E2E_BRIDGE", "1", 1)
        let keychain = KeychainStore()
        keychain.deleteAll()
        return (keychain, FfiAccountRegistry(store: KeychainSecretStore(keychain: keychain)))
    }

    private func release(_ keychain: KeychainStore) {
        keychain.deleteAll()
        unsetenv("FAUNA_E2E_BRIDGE")
    }

    /// Add accounts until the registry refuses; returns the refusal.
    private func fill(_ registry: FfiAccountRegistry) throws -> Error {
        for n in 0..<200 {
            do {
                _ = try registry.addAccount(
                    secretHex: Self.secret(n), nestUrl: "https://nest.example",
                    deviceId: String(repeating: "ab", count: 32))
            } catch {
                return error
            }
        }
        throw RefusalNeverCame()
    }

    private struct RefusalNeverCame: Error {}

    /// A full list throws the shared line, verbatim, as the FFI's `General` message.
    @Test func aFullListThrowsTheSharedLine() throws {
        let (keychain, registry) = makeHermetic()
        defer { release(keychain) }

        let refusal = try fill(registry)

        #expect(DisplayError.message(refusal) == L.settings.accountListFull)
    }

    /// The switcher paints the line the app root posted, on its `error-message`.
    @Test func theSwitcherPaintsThePostedRefusal() throws {
        let (keychain, registry) = makeHermetic()
        defer { release(keychain) }
        let refusal = try fill(registry)

        let vm = AccountSwitcherVM()
        let token = NotificationCenter.default.addObserver(
            forName: .faunaAccountAddRefused, object: nil, queue: nil
        ) { note in
            MainActor.assumeIsolated { vm.reportAddRefused(note) }
        }
        defer { NotificationCenter.default.removeObserver(token) }

        AccountSwitcherVM.postAddRefused(refusal)

        #expect(vm.error == L.settings.accountListFull)
    }
}
