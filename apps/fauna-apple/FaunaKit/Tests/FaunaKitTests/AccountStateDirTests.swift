import Testing
import Foundation
@testable import FaunaKit

/// Pins `AccountStateDir`'s photo-backup scoping — the fix for the SwiftData
/// isolation gap `account-scoping.md` § Isolation-contract gap ledger names
/// for apple: `PhotoBackupEngine`'s dedup/upload-state store used to be one
/// un-scoped `ModelContainer` for the whole install, read/written by
/// whichever account happened to be signed in and never touched by sign-out
/// or "remove this account".
///
/// These tests operate against the REAL `AccountStateDir.base` (there is
/// no injectable override — matches every other `AccountStateDir` consumer),
/// scoped to fixture actor ids no real account can ever produce, and clean up
/// after themselves unconditionally via `defer` — the same hermetic-by-scope
/// discipline `KeychainSecretStoreTests` uses for the real Keychain.
@MainActor
struct AccountStateDirTests {
    /// 64 lowercase hex chars — the shape `isActorHex` requires — but a
    /// pattern no real actor id will ever collide with.
    private static let actorA = String(repeating: "ee", count: 32)
    private static let actorB = String(repeating: "ff", count: 32)

    /// A throwaway W6 (account-data-plane.md § Workstreams) store root for the tests below. **Never the real one**:
    /// on macOS `AccountStateDir.storeContainerDir` is `nil`, so the erases
    /// would resolve `StoreRoot::platform()` — the developer's own
    /// `~/Library/Group Containers/<app group>/sync`, holding this machine's
    /// live account stores AND per-actor sync-engine state. That is exactly why
    /// both erases carry a container-explicit half, mirroring shared Rust's
    /// `erase_scope_under` / `erase_all_scopes_under`.
    private static func tempStoreRoot() -> String {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("fauna-account-store-tests-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
    }

    private func cleanUp() {
        let root = Self.tempStoreRoot()
        AccountStateDir.erase(actorIdHex: Self.actorA, storeContainerDir: root)
        AccountStateDir.erase(actorIdHex: Self.actorB, storeContainerDir: root)
        try? FileManager.default.removeItem(atPath: root)
    }

    /// The isolation contract's placement half: two accounts must never
    /// resolve to the same photo-backup file, or a switch renders one
    /// account's upload-state cache as the other's.
    @Test func photoBackupStoreURLIsIsolatedPerActor() {
        cleanUp()
        defer { cleanUp() }

        let urlA = AccountStateDir.photoBackupStoreURL(actorIdHex: Self.actorA)
        let urlB = AccountStateDir.photoBackupStoreURL(actorIdHex: Self.actorB)

        #expect(urlA != urlB)
        #expect(urlA.path.contains(Self.actorA))
        #expect(urlB.path.contains(Self.actorB))
        // Scoped under the same per-actor layout the MLS store uses, not a
        // second, divergent base.
        #expect(urlA.path.contains(AccountStateDir.base.path))
    }

    /// The erasure half, single-account remove: `AccountStateDir.erase` is
    /// the "forget this account" path (`AccountSwitcherVM.remove`) —
    /// removing one account's photo-backup store must not touch another's.
    @Test func erasingOneAccountRemovesOnlyItsPhotoBackupStore() throws {
        cleanUp()
        defer { cleanUp() }

        let urlA = AccountStateDir.photoBackupStoreURL(actorIdHex: Self.actorA)
        let urlB = AccountStateDir.photoBackupStoreURL(actorIdHex: Self.actorB)
        try Data("fixture".utf8).write(to: urlA)
        try Data("fixture".utf8).write(to: urlB)

        AccountStateDir.erase(actorIdHex: Self.actorA, storeContainerDir: Self.tempStoreRoot())

        #expect(!FileManager.default.fileExists(atPath: urlA.path), "the removed account's store must be gone")
        #expect(FileManager.default.fileExists(atPath: urlB.path), "the surviving account's store must be untouched")
    }

    /// The erasure half, sign-out: `AccountStateDir.eraseAll` is what
    /// `StatusVM.signOut` calls — every account's photo-backup store must go,
    /// the same way sign-out already sweeps `mls.db` for each account.
    @Test func signOutErasesEveryAccountsPhotoBackupStore() throws {
        cleanUp()
        defer { cleanUp() }

        let urlA = AccountStateDir.photoBackupStoreURL(actorIdHex: Self.actorA)
        let urlB = AccountStateDir.photoBackupStoreURL(actorIdHex: Self.actorB)
        try Data("fixture".utf8).write(to: urlA)
        try Data("fixture".utf8).write(to: urlB)

        AccountStateDir.eraseAll(storeContainerDir: Self.tempStoreRoot())

        #expect(!FileManager.default.fileExists(atPath: urlA.path))
        #expect(!FileManager.default.fileExists(atPath: urlB.path))
    }

    /// **The W6 store root goes too, and missing it is worse than stale bytes.**
    /// The account store is a *sibling* of `AccountStateDir.base` under the per-user root,
    /// never a child, so the pre-2026-08-25 hand-rolled `FileManager` sweep of
    /// this app's own base left it behind entirely — while the credential
    /// namespace the erase accompanies destroyed its Ed25519 writer key, so
    /// every later sign-in was refused with "account store belongs to a
    /// different writer" and the app ran with no account runtime, silently, for
    /// good (`apps/account-scoping.md` § Erasure follows scope; measured on tui
    /// 2026-08-18).
    ///
    /// Red-verifies against the old shape: a sweep that iterates `AccountStateDir.base`
    /// alone leaves `<container>/<actor>/` standing.
    @Test func signOutErasesTheAccountStoreUnderTheW6RootToo() throws {
        cleanUp()
        defer { cleanUp() }

        let root = Self.tempStoreRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }
        let fm = FileManager.default
        let storeA = URL(fileURLWithPath: root).appendingPathComponent(Self.actorA, isDirectory: true)
        let storeB = URL(fileURLWithPath: root).appendingPathComponent(Self.actorB, isDirectory: true)
        for dir in [storeA, storeB] {
            try fm.createDirectory(at: dir, withIntermediateDirectories: true)
            try Data("fixture".utf8).write(to: dir.appendingPathComponent("account-store"))
        }

        AccountStateDir.eraseAll(storeContainerDir: root)

        #expect(!fm.fileExists(atPath: storeA.path), "sign-out must sweep the W6 store root, not only the app's own base")
        #expect(!fm.fileExists(atPath: storeB.path))
    }

    /// The single-account remove reaches the W6 root as well — and touches
    /// nothing outside that actor's scope under either root.
    @Test func removingOneAccountErasesOnlyItsAccountStore() throws {
        cleanUp()
        defer { cleanUp() }

        let root = Self.tempStoreRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }
        let fm = FileManager.default
        let storeA = URL(fileURLWithPath: root).appendingPathComponent(Self.actorA, isDirectory: true)
        let storeB = URL(fileURLWithPath: root).appendingPathComponent(Self.actorB, isDirectory: true)
        for dir in [storeA, storeB] {
            try fm.createDirectory(at: dir, withIntermediateDirectories: true)
            try Data("fixture".utf8).write(to: dir.appendingPathComponent("account-store"))
        }

        AccountStateDir.erase(actorIdHex: Self.actorA, storeContainerDir: root)

        #expect(!fm.fileExists(atPath: storeA.path), "the removed account's W6 store must be gone")
        #expect(fm.fileExists(atPath: storeB.path), "the surviving account's W6 store must be untouched")
    }

    /// `pureMlsDbPath`'s well-formed half: a real hex resolves to
    /// the per-actor scoped store — same shape as `mlsDbPath`'s, without
    /// calling it (it creates the directory; this stays pure like the function
    /// under test, so it needs no `cleanUp()`).
    @Test func pureMlsDbPathResolvesTheScopedStoreForAWellFormedHex() {
        let path = AccountStateDir.pureMlsDbPath(actorIdHex: Self.actorA)

        #expect(path == AccountStateDir.base.appendingPathComponent(Self.actorA).appendingPathComponent("mls.db").path)
    }

    /// A refused hex — nil, empty, or malformed — resolves under `-unresolved-`
    /// in BOTH resolvers: never onto `AccountStateDir.base` itself (the `""`
    /// case: `appendingPathComponent("")` is a documented no-op that would
    /// otherwise collapse onto `<base>/mls.db`), and never onto a real actor's
    /// scoped store. Windows' `AccountStateDir.ScopeDir` is the same shape.
    @Test func aRefusedHexResolvesUnderTheUnresolvedComponent() {
        let unresolved = AccountStateDir.base.appendingPathComponent("-unresolved-")
            .appendingPathComponent("mls.db").path

        for refused in [nil, "", "not-64-hex-chars", String(repeating: "z", count: 64)] {
            #expect(AccountStateDir.pureMlsDbPath(actorIdHex: refused) == unresolved,
                    "refused hex \(refused ?? "nil") must resolve under -unresolved-")
            #expect(AccountStateDir.mlsDbPath(actorIdHex: refused) == unresolved,
                    "both resolvers must agree for refused hex \(refused ?? "nil")")
        }
    }

    /// **Refusal pin (the compat-remnant sweep, `version-compatibility.md`
    /// § Dimension 2):** a flat `conv-mls.db` at the base is never adopted into an
    /// account's scoped dir — resolving the store creates the directory and
    /// nothing else. Red against the pre-sweep resolver, which copied it in.
    /// Seeds the flat file only when the real base holds none, and removes only
    /// what it seeded.
    @Test func aFlatStoreAtTheBaseIsNeverAdopted() throws {
        cleanUp()
        defer { cleanUp() }
        let fm = FileManager.default
        let flat = AccountStateDir.base.appendingPathComponent("conv-mls.db")
        try #require(!fm.fileExists(atPath: flat.path), "the real base already holds a flat store — not ours to touch")
        try fm.createDirectory(at: AccountStateDir.base, withIntermediateDirectories: true)
        try Data("flat".utf8).write(to: flat)
        defer { try? fm.removeItem(at: flat) }

        let path = AccountStateDir.mlsDbPath(actorIdHex: Self.actorA)

        #expect(!fm.fileExists(atPath: path), "the flat store must not be copied into the scoped dir")
        #expect(fm.fileExists(atPath: URL(fileURLWithPath: path).deletingLastPathComponent().path))
    }

    /// macOS resolves the shared-with-the-agent root itself, so its container is
    /// `nil` — and that is load-bearing, not an omission. `StoreRoot::platform()`
    /// already lands on `<home>/Library/Group Containers/<app group>/sync` there;
    /// naming a second spelling of it here is how the runtime's root and the
    /// erase's would drift apart, which is the stranding bug above by another
    /// route. (iOS is the target that must name one — it has no host running
    /// this test.)
    @Test func macOSPassesNoContainerSoTheAgentSharedRootIsResolvedOnce() {
        #expect(AccountStateDir.storeContainerDir == nil)
    }
}
