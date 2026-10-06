import Testing
import Foundation
@testable import FaunaKit

/// Pins the durable e2e credential backing's delete path against reporting a clean erase that
/// never reached disk (`docs/goal/architecture/apps/account-scoping.md` § Erasure follows scope
/// → *the credential half is a residue class too*).
///
/// `KeychainStore`'s in-memory e2e mode is not really in-memory once the harness passes
/// `FAUNA_E2E_CREDENTIAL_DIR`: `memoryStore` is the fast, process-local view, and
/// `keychain.json` on disk is the durable one an external reader — a relaunched process, or an
/// e2e test reading the file straight off disk — actually trusts. `delete()` used to drop the
/// row from `memoryStore` unconditionally and only THEN best-effort flush to disk, so a flush
/// refused by the filesystem (a read-only credential directory, e.g.) left the file still
/// holding the row while this process's own read-back (`load()`, which reads `memoryStore`)
/// reported it gone — a sign-out whose credential wipe was refused by the store painted a clean
/// "Signed out" over a device that still held its identity secret.
@MainActor
struct KeychainDurableDeleteRollbackTests {
    /// Points `KeychainStore` at a throwaway durable directory for the body, then restores the
    /// directory to writable and wipes the store before cleaning up — so a failing `body` never
    /// leaves `memoryStore` or `FAUNA_E2E_CREDENTIAL_DIR` poisoned for a later test.
    private func withDurableStore(_ body: (KeychainStore, URL) -> Void) {
        setenv("FAUNA_E2E_BRIDGE", "1", 1)
        let dir = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("fauna-keychain-durable-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        setenv("FAUNA_E2E_CREDENTIAL_DIR", dir.path, 1)
        defer {
            try? FileManager.default.setAttributes(
                [.posixPermissions: 0o755], ofItemAtPath: dir.path)
            KeychainStore().deleteAll()
            try? FileManager.default.removeItem(at: dir)
            unsetenv("FAUNA_E2E_CREDENTIAL_DIR")
            unsetenv("FAUNA_E2E_BRIDGE")
        }
        body(KeychainStore(), dir)
    }

    /// **The regression.** A delete whose durable flush is refused must leave the row readable
    /// in-process too — matching what a fresh process reading the file back would see. This is
    /// the Swift twin of `_credential_erase_refused`'s POSIX shape in
    /// `test_a_sign_out_whose_credentials_cannot_be_erased_says_so`: a read-only credential
    /// directory, so every rewrite of `keychain.json` (a temp-file-plus-rename in that
    /// directory) fails while the file itself stays readable.
    @Test func aDeleteWhoseDurableFlushFailsKeepsTheRowReadable() {
        withDurableStore { keychain, dir in
            try? keychain.save(rawKey: "secret_key", value: "seed-abc")
            #expect(
                keychain.load(rawKey: "secret_key") == "seed-abc",
                "precondition: the durable write landed while the directory was writable")

            try? FileManager.default.setAttributes(
                [.posixPermissions: 0o555], ofItemAtPath: dir.path)

            keychain.delete(rawKey: "secret_key")

            #expect(
                keychain.load(rawKey: "secret_key") == "seed-abc",
                """
                the durable flush was refused, so the row is still on disk — the in-process \
                view must keep reporting it, not claim a clean erase the file never received
                """
            )

            try? FileManager.default.setAttributes(
                [.posixPermissions: 0o755], ofItemAtPath: dir.path)
            let onDisk = try? String(
                contentsOf: dir.appendingPathComponent("keychain.json"), encoding: .utf8)
            #expect(
                onDisk?.contains("seed-abc") == true,
                "the file itself was never actually rewritten by the refused delete")
        }
    }

    /// The contrast that makes the regression test meaningful: an ordinary delete, with the
    /// directory writable throughout, really does erase the row everywhere.
    @Test func anOrdinaryDeleteRemovesTheRowFromMemoryAndDisk() {
        withDurableStore { keychain, dir in
            try? keychain.save(rawKey: "secret_key", value: "seed-abc")
            keychain.delete(rawKey: "secret_key")
            #expect(keychain.load(rawKey: "secret_key") == nil)
            let onDisk = try? String(
                contentsOf: dir.appendingPathComponent("keychain.json"), encoding: .utf8)
            #expect(onDisk?.contains("seed-abc") != true)
        }
    }
}
