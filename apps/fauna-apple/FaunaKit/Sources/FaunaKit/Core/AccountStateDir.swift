import FaunaFFISwift
import Foundation

/// Apple's **account-scoped** client-state home
/// (`account-scoping.md` § The scoping taxonomy, class 1 + § Serialized
/// switching): `~/Library/Application Support/Fauna/<actor-id-hex>/` holds the
/// stores whose meaning depends on who is signed in — today the conversations
/// MLS store and the mail-segment backup coordinator's state.
///
/// The derivation and the erasure are shared Rust (`account_state_dir` /
/// `account_state_erase_scope`, over `fauna_sync_engine::db`), the same pair the
/// sync state dir resolves through (`SyncStateDir`, on its own app-group base), so
/// no client and no process can invent a second layout. Nothing account-scoped ever
/// rests directly in ``base``: the pre-scoping flat layout and its first-adopter
/// adoption were removed by the compat-remnant sweep (`version-compatibility.md`
/// § Dimension 2, the fourth ratified exception — no pre-scoping install exists).
/// Windows' `AccountStateDir.cs` is the same shape.
public enum AccountStateDir {
    /// `<Application Support>/Fauna` — the base the per-account subdirs live under,
    /// and the install-scoped base the shared account-state, instance-lock and
    /// custodian FFI calls are keyed on. No side effect (test/diagnostic seam).
    public static var base: URL {
        applicationSupportDirectory().appendingPathComponent("Fauna", isDirectory: true)
    }

    /// The conversations-rail MLS store — the ONE MLS engine on apple
    /// (`libs/fauna-ffi/src/folders_author.rs` § "ONE MlsEngine per
    /// mls_state.db"), at `<base>/<actor>/mls.db`, its directory created.
    ///
    /// A nil/malformed actor id resolves under `-unresolved-` — never onto
    /// ``base`` itself, and never onto another account's scoped store: a session
    /// that cannot name its account still runs, against a store of its own.
    public static func mlsDbPath(actorIdHex: String?) -> String {
        scopeDir(actorIdHex: actorIdHex, create: true).appendingPathComponent(mlsDbName).path
    }

    /// The same per-actor MLS store path as `mlsDbPath`, but PURE — creates
    /// nothing (`libs/fauna-ffi/src/recovery.rs`'s `succession_retry_group_sweep`:
    /// its `old_store_path` resolver "must be the pure scope resolution", because
    /// the retry turns on whether that store ALREADY exists — a resolver that
    /// creates one would report a clean run over an empty store while the retired
    /// leaf sits untouched). A nil/malformed hex resolves as `mlsDbPath`'s does, to
    /// a path that does not exist, which the existence check answers "no old
    /// state" — the words tui's `retry_sweep_op` / linux's `retired_mls_db_path`
    /// produce on the same arm. Windows' `PureMlsDbPath` is the same shape.
    public static func pureMlsDbPath(actorIdHex: String?) -> String {
        scopeDir(actorIdHex: actorIdHex, create: false).appendingPathComponent(mlsDbName).path
    }

    /// The nested component a nil/malformed actor id resolves under — never a
    /// valid 64-hex actor id, so it can never collide with a real per-actor scoped
    /// dir, and never empty, so it can never collapse `appendingPathComponent`
    /// back onto ``base`` itself.
    private static let unresolvedActorComponent = "-unresolved-"

    /// `PhotoBackupEngine`'s dedup/upload-state cache — a SwiftData store at
    /// `<base>/<actor>/photo-backup.store` mapping each `PHAsset` local
    /// identifier to its upload state.
    ///
    /// A class-4 replica in the scoping taxonomy (`account-scoping.md` § The
    /// scoping taxonomy): every row is re-derivable by re-scanning the photo
    /// library against the nest's own file listing (worst case, one duplicate
    /// re-upload pass). A nil/malformed actor id resolves as `mlsDbPath`'s does.
    public static func photoBackupStoreURL(actorIdHex: String?) -> URL {
        scopeDir(actorIdHex: actorIdHex, create: true).appendingPathComponent(photoBackupStoreName)
    }

    /// Local state file for the backup-audit's own observation evidence
    /// (`ui/backups.md` § Audit-alert surface — the freshness check's
    /// independent ground truth) at `<base>/<actor>/backup-audit-state.json`.
    ///
    /// A class-4 replica: the observation high-water is re-derivable by
    /// observing activity again. **Must be actor-scoped**: two accounts sharing
    /// one path would let one account's high-water silently suppress the other's
    /// freshness failures — the exact failure actor-scoping exists to prevent.
    /// Mirrors android's `AccountStores.backupAuditStatePath()`. A nil/malformed
    /// actor id resolves as `mlsDbPath`'s does.
    public static func backupAuditStatePath(actorIdHex: String?) -> String {
        scopeDir(actorIdHex: actorIdHex, create: true).appendingPathComponent(backupAuditStateName).path
    }

    /// The **W6 (account-data-plane.md § Workstreams) account-store container** this app hosts its account runtime
    /// from — the ONE accessor `FaunaClient.startAccountRuntime` and both erases
    /// below read (`account-data-plane.md` § The account store → *The
    /// client-side lifecycle*; android's `AccountStores.accountStoreContainerDir`
    /// is the same accessor, for the same reason).
    ///
    /// ⚠ **The same value must reach the start AND both erases, which is why it
    /// is an accessor and not three literals.** Shared Rust resolves
    /// `Some(dir) → StoreRoot::at(dir)`, `None → StoreRoot::platform()`, in ONE
    /// mapping (`libs/fauna-ffi/src/account_state.rs::store_root_for`) that is
    /// deliberately identical to the runtime's. An erase resolving a different
    /// root than the runtime opened sweeps a directory the store was never in,
    /// leaves the real store behind, and destroys its Ed25519 writer key with
    /// the credential wipe alongside — every later sign-in then refused with
    /// "account store belongs to a different writer", the app silently on the
    /// blob rail for good (measured on tui 2026-08-18).
    ///
    /// **`nil` on macOS, and that is not a shortcut.** `StoreRoot::platform()`
    /// already resolves the **user-domain** base
    /// `~/Library/Application Support/Fauna/sync` there
    /// (`libs/fauna-account-store/src/root.rs`'s macOS `production_base` —
    /// moved out of the app-group container 2026-08-25, `SyncStateDir`'s
    /// two-domain doc) — the very base `SyncStateDir.userDomainSyncDir` names
    /// — so the desktop derivation is the shared-with-the-agent root, and
    /// passing it explicitly would only be a second spelling of it.
    ///
    /// **iOS must pass one.** `platform()` takes the generic unix branch there
    /// and resolves `$HOME/.config/fauna/sync` *inside* the sandbox: writable,
    /// openable, and unreachable by the app extensions that share the container,
    /// so the failure is a silently unshared store rather than an error. E2e
    /// launches keep the in-sandbox `SyncStateDir.appSupportSyncDir` for the same
    /// machine-global-state reason `FaunaClient.syncStateDir` does on iOS
    /// (`e2e-conventions.md` point 10) — the app-group container is shared by
    /// every install on the device, and a test launch must never touch it.
    ///
    /// Not per-actor: `StoreRoot::store_dir(actor_hex)` scopes underneath this
    /// itself, so handing it a per-actor dir would scope the path twice.
    public static var storeContainerDir: String? {
        #if os(macOS)
        return nil
        #else
        if !FaunaE2E.isActive, let base = SyncStateDir.containerSyncDir() {
            try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
            return base.path
        }
        let dir = SyncStateDir.appSupportSyncDir
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
        #endif
    }

    /// Erase one account's scoped stores **and its W3 account store under the W6
    /// root** — the *remove this account* half of § Erasure follows scope.
    ///
    /// The scope sweep is the **shared** `account_state_erase_scope`, not a
    /// hand-rolled `removeItem`: the account store is a *sibling* of ``base``
    /// under the per-user root, never a child, so a sweep that iterated this
    /// app's own base alone would miss it entirely — see `storeContainerDir` for
    /// what that costs.
    public static func erase(actorIdHex: String) {
        erase(actorIdHex: actorIdHex, storeContainerDir: storeContainerDir)
    }

    /// The container-explicit half of `erase(actorIdHex:)` — the Swift twin of
    /// shared Rust's `erase_scope_under`, and for the same reason: on macOS the
    /// production root is the developer's own app-group container, so a test
    /// that let it resolve would erase real data.
    static func erase(actorIdHex: String, storeContainerDir: String?) {
        do {
            try accountStateEraseScope(
                baseDir: base.path, actorIdHex: actorIdHex,
                storeContainerDir: storeContainerDir)
        } catch {
            logMessage(
                level: .warn, target: "fauna.accounts",
                message: "[state] erase of one account's scoped stores failed: \(error)")
        }
    }

    /// Erase EVERY account's scoped stores **and every account store under the
    /// W6 root** — the sign-out / delete-account
    /// half of § Erasure follows scope, the store-side counterpart of the
    /// registry's whole-namespace `clearAll()`. A "Sign Out" that left the MLS
    /// store on disk would be the same bug as one that left `fauna/{actor}/secret`
    /// behind: the user asked for their data off this device.
    ///
    /// One shared call: `erase_all_account_scopes` owns the actor-scope sweep.
    /// Anything that is not an
    /// actor scope survives, which is how install-scoped state keeps its class-2
    /// disposition through a sign-out.
    ///
    /// Returns what the sweep could NOT remove. A non-empty `survivors` means
    /// the signed-out user's readable data is still on this device, and the
    /// caller owes them a line — the count, never the paths (§ Erasure follows
    /// scope).
    @discardableResult
    public static func eraseAll() -> FfiEraseSweep {
        eraseAll(storeContainerDir: storeContainerDir)
    }

    /// The container-explicit half of `eraseAll()` — see `erase(actorIdHex:
    /// storeContainerDir:)` for why the root is a parameter rather than a
    /// resolution.
    ///
    /// ⚠ **It no longer throws, and the `do`/`catch` that used to wrap it is
    /// gone deliberately.** Until 2026-09-09 a survivor arrived as an error, so
    /// every seat handled the outcome in a `catch` and painted a clean "Signed
    /// out" on the path that fell through — which is the defect. The sweep now
    /// reports survivors on the success path, and it has no failure mode of its
    /// own (a Rust panic traps rather than throwing, as everywhere else in this
    /// binding), so there is nothing left to catch.
    ///
    /// The outcome is recorded by `StatusVM.signOut` →
    /// `SignOutResidueSurface.record` → `SessionState.signOutResidue` →
    /// `OnboardingVM.signOutResidue` → `IdentityChoiceView`'s `sign-out-residue`
    /// view: the paths to the log (here), the count to the user, and nothing at
    /// all when the sweep was clean (`account-scoping.md` § Erasure follows
    /// scope → *Telling the USER is the erase's own duty*).
    @discardableResult
    static func eraseAll(storeContainerDir: String?) -> FfiEraseSweep {
        let sweep = accountStateEraseAllScopes(
            baseDir: base.path,
            storeContainerDir: storeContainerDir)
        if !sweep.survivors.isEmpty {
            logMessage(
                level: .warn, target: "fauna.accounts",
                message:
                    "[state] \(sweep.survivors.count) path(s) SURVIVED the erase and still "
                    + "hold this user's data: \(sweep.survivors.joined(separator: ", "))")
        }
        return sweep
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    /// The MLS store's name inside the scoped dir — the uniform cross-app one.
    static let mlsDbName = "mls.db"
    static let backupAuditStateName = "backup-audit-state.json"
    static let photoBackupStoreName = "photo-backup.store"

    /// The account's scoped dir, `<base>/<actor>` — or `<base>/-unresolved-` for
    /// a nil/malformed actor id (shared Rust refuses anything that is not a
    /// 64-char actor id rather than producing a stray directory). `create`
    /// makes the directory; the pure resolver passes `false`.
    private static func scopeDir(actorIdHex: String?, create: Bool) -> URL {
        let root = Self.base
        var dir = root.appendingPathComponent(unresolvedActorComponent, isDirectory: true)
        if let actorIdHex, !actorIdHex.isEmpty,
            let scoped = try? accountStateDir(baseDir: root.path, actorIdHex: actorIdHex)
        {
            dir = URL(fileURLWithPath: scoped, isDirectory: true)
        }
        if create {
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        return dir
    }
}
