import Testing
import Foundation
@testable import FaunaKit

/// Pins the e2e `keychain.json` backing to the read discipline the Rust credential store's File
/// backend keeps: `fauna_credential_store::cred_file_read` re-reads the file on every operation,
/// and every mutation is a read-modify-write under `<file>.lock`.
///
/// Why the Swift store must match it: iOS has no native Rust keyring arm, so the shared
/// `fauna-account-store` namespace rides the foreign seam into this very store, and the e2e
/// harness restores a relaunched machine's store-principal slot by writing it into the file at the
/// actor's first sign-in (`docs/goal/architecture/e2e-conventions.md` convention 10, the
/// principal-slot carry). A backing that loaded the file once per process never saw that write —
/// and flushed its stale map over it on the next save, so the relaunch minted a new writer key and
/// enrolled a new device row.
@MainActor
struct KeychainE2EFileBackingTests {
    /// Points `KeychainStore` at a throwaway durable directory for the body, then wipes the store
    /// and restores the environment — the same shape as `KeychainDurableDeleteRollbackTests`.
    private func withDurableStore(_ body: (KeychainStore, URL) -> Void) {
        setenv("FAUNA_E2E_BRIDGE", "1", 1)
        let dir = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("fauna-keychain-backing-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        setenv("FAUNA_E2E_CREDENTIAL_DIR", dir.path, 1)
        defer {
            KeychainStore().deleteAll()
            try? FileManager.default.removeItem(at: dir)
            unsetenv("FAUNA_E2E_CREDENTIAL_DIR")
            unsetenv("FAUNA_E2E_BRIDGE")
        }
        body(KeychainStore(), dir)
    }

    /// What another process — the harness — does to the file between two of this process's
    /// operations: a whole-file rewrite adding one row, landed by rename.
    private func writeFromOutside(_ dir: URL, key: String, value: String) {
        let file = dir.appendingPathComponent("keychain.json")
        var rows = (try? JSONDecoder().decode([String: String].self, from: Data(contentsOf: file))) ?? [:]
        rows[key] = value
        try? JSONEncoder().encode(rows).write(to: file, options: .atomic)
    }

    private func rowsOnDisk(_ dir: URL) -> [String: String] {
        let file = dir.appendingPathComponent("keychain.json")
        return (try? JSONDecoder().decode([String: String].self, from: Data(contentsOf: file))) ?? [:]
    }

    @Test func aRowWrittenIntoTheFileFromOutsideIsReadBack() {
        withDurableStore { keychain, dir in
            try? keychain.save(rawKey: "secret_key", value: "seed-abc")
            writeFromOutside(dir, key: "fauna-account-store/aa", value: "writer-key")

            #expect(
                keychain.load(rawKey: "fauna-account-store/aa") == "writer-key",
                """
                a row another process wrote into keychain.json after this process's first read \
                was not read back — the backing loaded the file once and served a stale map
                """
            )
        }
    }

    @Test func aSaveKeepsARowWrittenIntoTheFileFromOutside() {
        withDurableStore { keychain, dir in
            try? keychain.save(rawKey: "secret_key", value: "seed-abc")
            writeFromOutside(dir, key: "fauna-account-store/aa", value: "writer-key")

            try? keychain.save(rawKey: "node_url", value: "https://nest.example")

            let rows = rowsOnDisk(dir)
            #expect(
                rows["fauna-account-store/aa"] == "writer-key",
                "this process's save flushed a stale map over a row another process had written"
            )
            #expect(rows["node_url"] == "https://nest.example")
            #expect(rows["secret_key"] == "seed-abc")
        }
    }

    @Test func aDeleteKeepsARowWrittenIntoTheFileFromOutside() {
        withDurableStore { keychain, dir in
            try? keychain.save(rawKey: "secret_key", value: "seed-abc")
            writeFromOutside(dir, key: "fauna-account-store/aa", value: "writer-key")

            keychain.delete(rawKey: "secret_key")

            let rows = rowsOnDisk(dir)
            #expect(rows["fauna-account-store/aa"] == "writer-key")
            #expect(rows["secret_key"] == nil)
        }
    }
}
