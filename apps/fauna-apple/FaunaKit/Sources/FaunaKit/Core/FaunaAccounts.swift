import Foundation

/// The one place apple builds the shared account registry.
///
/// Both app targets and every view model go through here, so the store the launch
/// machine routes on, the store the factory-reset mint persists to, and the store the
/// account switcher lists from are the same store by construction — a class of bug
/// (two persistence paths disagreeing about who is active) that simply cannot arise if
/// nobody can build a second one.
///
/// Apple owns exactly one thing in this stack: `KeychainSecretStore`, a key→value map.
/// Everything else — per-actor namespacing, the session material, the
/// `LaunchPersistence` adapter — is shared Rust
/// (`docs/goal/architecture/long-term-store.md` § Multi-account evolution → Shared seam,
/// priority #2).
public enum FaunaAccounts {
    /// The registry over this device's Keychain. Cheap — a stateless view over the store,
    /// so build one wherever you need it rather than passing one around.
    ///
    /// Mutations serialize under the shared cross-process advisory file lock
    /// (`long-term-store.md` § Multi-account evolution → Cross-process mutation lock).
    /// Every registry mutator is a read-modify-write of one `fauna/index` blob and the
    /// Keychain offers no cross-process transaction, so two apple processes writing at
    /// once could lose an update — one rewrite swallowing the other's account. Reads and
    /// the bind gate stay lock-free, and an unusable lock file degrades to today's
    /// unserialized behavior rather than breaking a sign-out or a switch.
    ///
    /// The lock dir is **install-scoped** — `AccountStateDir.base`, the same
    /// `~/Library/Application Support/Fauna/` that holds the per-account subdirs, one per
    /// OS login and never per account (`account-scoping.md` § The scoping taxonomy,
    /// class 2). That is required, not incidental: the lock guards the account index,
    /// which is shared *between* accounts, so an account-scoped lock would let two
    /// instances mutate the same index while each held its own file. The lock file sits
    /// beside the actor subdirs and is deliberately not swept by `AccountStateDir`'s
    /// erasure — it is install furniture, not anyone's data.
    public static func registry(keychain: KeychainStore = KeychainStore()) -> FfiAccountRegistry {
        FfiAccountRegistry.newWithLockDir(
            store: KeychainSecretStore(keychain: keychain),
            lockDir: AccountStateDir.base.path)
    }

    /// Lend this device's Keychain to shared Rust's credential slots — the W3 (account-data-plane.md § Workstreams)
    /// account-store **writer key** (the T10 slot) first of all. Call **once at
    /// app launch, before the first sign-in**, beside `NestTrust.installPinStore()`.
    ///
    /// **Why iOS needs it.** `fauna-credential-store` drives the macOS login
    /// Keychain itself; on iOS it has no arm, and every slot write it made there
    /// was dropped — so the account runtime's writer-key mint could never read
    /// back and the assembly refused on every real device, only ever succeeding
    /// under the e2e redirect (`account-data-plane.md` § Implementation status
    /// today → *Built — W3 the apple host*). The lent store is the SAME
    /// `KeychainSecretStore` the registry above rides, so the writer key lives in
    /// the one keychain namespace beside the identity it was minted for, under
    /// the same `ThisDeviceOnly` policy (`ios.md` § Credential Storage) — Rust
    /// writes it as a namespace-prefixed row (`fauna-account-store/<actor>`).
    ///
    /// **Harmless on macOS**, which is why this is shared FaunaKit rather than an
    /// iOS-only call: there the Rust crate's own login-Keychain arm is the slot
    /// the co-located `fauna-sync-agent` shares, and resolution never consults
    /// the lent store where a native arm exists. Under e2e the store is the same
    /// in-memory (file-backed under `FAUNA_E2E_CREDENTIAL_DIR`) keychain every
    /// other e2e keychain read uses, so the phone e2e exercises this exact seam.
    public static func installPlatformCredentialStore(keychain: KeychainStore = KeychainStore()) {
        // Module-qualified on purpose: an unqualified call inside this enum resolves
        // to THIS static (Swift's member shadowing by base name) and never reaches
        // the UniFFI export — the `FaunaFFISwift.quotaFraction` shape in QuotaBar.
        FaunaFFISwift.installPlatformCredentialStore(store: KeychainSecretStore(keychain: keychain))
    }

    /// Try to become one of the instances serving `actorIdHex` — macOS's
    /// (OS login, account) single-instance guard (`account-scoping.md`
    /// § Concurrent instances). **macOS is RETIRED (W5.6, 2026-08-16): a
    /// SHARED acquire** — any number of same-account instances coexist (the
    /// three genuinely exclusive critical sections carry their own locks;
    /// the conversations-engine role is one, surfaced honestly via
    /// `ConversationsVM.pageError`). `nil` still means refuse terminally,
    /// never falling back onto a different account, but fires only
    /// when the lock acquire itself fails — a refusal, never an
    /// unguarded pair.
    ///
    /// The returned lock is RAII — hold it for the session's lifetime; a
    /// crashed holder releases automatically (kernel file lock), so there is
    /// no stale-lock state. The lock file is keyed per account but lives at
    /// the same install-scoped base as the mutation lock above, as a sibling
    /// of the actor subdirs — install furniture the erasure sweeps leave
    /// alone (unlinking a held lock file would re-open the race). A degraded
    /// acquire (`isHeld() == false`, I/O failure) proceeds unguarded rather
    /// than refusing the launch; the caller logs it.
    ///
    /// The lock also declares this instance at the shared account-store root
    /// (the serving lock), under the same `storeContainerDir` the erase pair
    /// sweeps, so another app's sign-out sees this window. Pass the returned
    /// object as `ownLock` to `signOutBlocked` / `removeAccountBlocked`: without
    /// it a lone window meets its own lock and refuses every sign-out.
    public static func acquireInstanceLock(actorIdHex: String) -> FfiAccountInstanceLock? {
        acquireAccountInstanceLockShared(
            stateBase: AccountStateDir.base.path, actorIdHex: actorIdHex,
            storeContainerDir: AccountStateDir.storeContainerDir)
    }

    /// The `LaunchPersistence` over the **active** account — what `LaunchMachine` routes on
    /// and what `mintAndPersistPendingFactoryReset` persists through.
    ///
    /// This replaced apple's hand-rolled `KeychainLaunchPersistence` wholesale rather than
    /// sitting beside it. Keeping both would have re-introduced CR-3 by the back door: the
    /// old adapter read a single **global** pending-factory-reset row, so with two accounts
    /// on one device, account B's reset would overwrite account A's pending row and destroy
    /// A's claim code. Per-actor slots are the fix, and they only hold if *every* reader
    /// goes through the registry (`nest/common.md` § Client-state recoverability, CR-3).
    public static func launchPersistence(keychain: KeychainStore = KeychainStore()) -> LaunchPersistence {
        registry(keychain: keychain).launchPersistence()
    }

    /// This install's sync device id for `actorId` — the persisted per-account
    /// slot when one exists, else `derive_device_id(install_secret, actor_id)`
    /// (`sync-agent-credentials.md` § Credential model, the RULED 2026-09-20
    /// block; § Implementation status today → *The derived named-row id*, the
    /// secret-store face). This is the ONE FaunaKit call site every apple
    /// device-id mint goes through — `OnboardingVM`'s logged-in handoff and
    /// both app shells' session-patch door's non-injected arm — so macOS and
    /// iOS cannot re-diverge on how the id is minted (priority #1/#2).
    ///
    /// `installStore` is this registry's own `KeychainSecretStore` — no second
    /// store is needed. Sign-out (`clearAll()`, `StatusVM.signOut`) never
    /// names the install-scoped `install/device_secret` slot, so the secret
    /// survives every sign-out → sign-in cycle on one machine; only a factory
    /// reset's wholesale `KeychainStore.deleteAll()` sweeps the whole service
    /// namespace and ends it, which is exactly what the ruling intends
    /// ("uninstalling or a factory reset of the app's own data ends it").
    ///
    /// Throws only when no stable id exists (the minted secret did not read
    /// back, or a stored one is malformed) — callers should log and fall back
    /// to their own no-device-id behaviour rather than invent one this cannot
    /// reproduce on the next read.
    public static func deviceId(
        forActorId actorId: String, keychain: KeychainStore = KeychainStore()
    ) throws -> String {
        try registry(keychain: keychain).deviceIdForActor(
            installStore: KeychainSecretStore(keychain: keychain), actorId: actorId)
    }

    /// The account **this process serves**: the bound account of a secondary instance
    /// (`sessionLaunchBinding()` — the environment's binding or the launch-collision
    /// chooser's pick), else the registry's active account. Every session-identity read
    /// keys on this, never on `active()` alone: a bound secondary serves an account the
    /// active pointer does not name (`account-scoping.md` § Concurrent instances).
    public static func servedActorId(keychain: KeychainStore = KeychainStore()) -> String? {
        sessionLaunchBinding() ?? registry(keychain: keychain).active()
    }

    /// The served account's session material — secret, home nest, device id and the
    /// server-data cache (handle/domain/tier) in ONE read (`account-scoping.md`
    /// § Concurrent instances → *Session identity resolves through the session's
    /// account*). This is apple's only session-identity read: the registry is the only
    /// store (`long-term-store.md` § Downgrade mirror + abandoned-append recovery,
    /// RETIRED 2026-09-24), so there is no single-slot row to fall back to. `nil` when no
    /// account is served or its secret does not resolve — fail closed, never another
    /// account's material.
    public static func sessionMaterial(keychain: KeychainStore = KeychainStore()) -> FfiSessionMaterial? {
        guard let actorId = servedActorId(keychain: keychain) else { return nil }
        return registry(keychain: keychain).sessionMaterial(actorId: actorId)
    }

    /// Boot: the launch persistence to route on, over the active account. The registry
    /// is the only store, so boot routes on it directly — no downgrade mirror, and no
    /// eager migration (`long-term-store.md` § Native boot does not eagerly migrate).
    public static func bootLaunchPersistence(keychain: KeychainStore = KeychainStore()) -> LaunchPersistence {
        registry(keychain: keychain).launchPersistence()
    }

    /// How this process should launch: as the ordinary **primary** instance on the
    /// active account, as a **secondary** instance bound to one named account, or not
    /// at all.
    public enum LaunchBinding {
        /// No binding requested — the ordinary launch, on the active account.
        case primary(LaunchPersistence)
        /// Bound to `actorId` for this process's whole lifetime: reads and writes that
        /// account's slots, never consults or moves the active pointer.
        case bound(actorId: String, persistence: LaunchPersistence)
        /// A binding was requested and the gate refused it. The instance must NOT run —
        /// see `resolveLaunchBinding`.
        case refused(actorId: String, reason: String)
    }

    /// Resolve this process's launch binding (`account-scoping.md` § Concurrent
    /// instances).
    ///
    /// A **primary** launch is what every instance does today: route on the active
    /// account. A **secondary** instance is told which account it serves by the spawning
    /// instance as launch wiring (bucket-1 IPC — the user's *choice* happens in that
    /// instance's switcher UI, never in a file a human edits), and differs in two ways
    /// that both follow from "the active pointer belongs to the primary": it resolves the
    /// bound account's slots, and it does not move `active`.
    ///
    /// **A refused binding must not degrade into a primary launch.** Silently falling
    /// back would make the secondary a second window onto the active account — the
    /// confusion this stage exists to remove — and for a `require_confirm_to_activate`
    /// account it would be a way around the very prompt the flag demands. So the gate
    /// (`bind_account`, which mirrors every activation guard 1:1) is honoured: on
    /// `ConfirmationRequired` this runs the same native re-auth the switcher runs and
    /// retries via `bind_account_confirmed`; on any other refusal, and on a declined
    /// re-auth, it returns `.refused` and the caller exits.
    public static func resolveLaunchBinding(
        keychain: KeychainStore = KeychainStore()
    ) async -> LaunchBinding {
        let registry = registry(keychain: keychain)
        // Resolves through the succession chain (seeding the process binding
        // cell from the environment on first read, then re-pointing it to the
        // terminal successor) rather than reading the environment directly, so
        // a spawn minted with a retired id serves the successor instead of
        // meeting the nest's `superseded` refusal on its first connect
        // (`account-scoping.md` § Concurrent instances, rider 2).
        guard let actorId = registry.resolveLaunchBinding() else {
            return .primary(registry.launchPersistence())
        }
        // Pre-read the flag and pick the gate, exactly as the switcher does for
        // activation (`AccountSwitcherVM.requestSwitch`) — same concept, same shape
        // (priority #3). `bindAccount`'s own refusal stays the backstop for a caller
        // that forgets the prompt; it is not the signal we branch on.
        let flagged = registry.list().first { $0.actorId == actorId }?
            .requireConfirmToActivate ?? false
        if flagged {
            guard await AccountReauth.confirmActivation() else {
                logMessage(level: .info, target: "fauna.accounts",
                           message: "[bound-launch] re-auth declined for \(actorId) — not launching")
                return .refused(actorId: actorId, reason: "re-auth declined")
            }
        }
        do {
            if flagged {
                try registry.bindAccountConfirmed(actorId: actorId)
            } else {
                try registry.bindAccount(actorId: actorId)
            }
        } catch {
            logMessage(level: .warn, target: "fauna.accounts",
                       message: "[bound-launch] refusing to launch bound to \(actorId): \(error)")
            return .refused(actorId: actorId, reason: "\(error)")
        }
        logMessage(level: .info, target: "fauna.accounts",
                   message: "[bound-launch] launching bound to \(actorId) (active pointer untouched)")
        return .bound(
            actorId: actorId,
            persistence: registry.boundLaunchPersistence(actorId: actorId))
    }

    /// The admin auto-default (`long-term-store.md` § Multi-account evolution:
    /// *"Default off; a client turns it on for its admin identity"*): call at every
    /// `am-i-admin = true` observation — apple's is the nav gate that reveals
    /// `admin-tab` — to flip the ACTIVE account's `require_confirm_to_activate` on,
    /// unless the user has ever touched that account's toggle (an explicit OFF
    /// sticks; the registry enforces that, this is just the client-side observation
    /// hook, since the registry never learns admin-ness itself).
    public static func autoEnableRequireConfirmForActiveAdmin(keychain: KeychainStore = KeychainStore()) {
        let registry = registry(keychain: keychain)
        guard let actorId = registry.active() else {
            // Never silent: a nil active pointer and a legitimate no-op are the
            // same observation from outside, which is exactly what left the
            // iOS auto-default failure a 4-link mystery for five runs.
            logMessage(level: .warn, target: "fauna.accounts",
                       message: "[admin-auto-default] no active account → nothing to flag")
            return
        }
        do {
            if try registry.autoEnableRequireConfirm(actorId: actorId) {
                logMessage(level: .info, target: "fauna.accounts",
                           message: "[admin-auto-default] require_confirm_to_activate auto-enabled for \(actorId)")
                // This write bypasses `AccountSwitcherVM` entirely, so a switcher
                // already on screen would keep rendering the pre-write row — toggle
                // OFF while the store says ON, and the next tap derives its new value
                // from that stale entry and writes the value already there. Posted
                // only on an actual mutation (the refusal branch below changes
                // nothing, so it has nothing to invalidate).
                NotificationCenter.default.post(name: .faunaAccountRegistryChanged, object: nil)
            } else {
                // The registry legitimately refused (already on, or the user
                // pinned it with `require_confirm_user_set`). Logged for the same
                // reason as the nil-active guard: the OFF-sticks half of the
                // journey asserts precisely this refusal, and a silent one is
                // indistinguishable from the write never being attempted.
                logMessage(level: .info, target: "fauna.accounts",
                           message: "[admin-auto-default] refused for \(actorId) (already on, or user-set)")
            }
        } catch {
            // Best-effort: a failed auto-default must never break the admin gate.
            logMessage(level: .warn, target: "fauna.accounts",
                       message: "[admin-auto-default] autoEnableRequireConfirm(\(actorId)) failed: \(error)")
        }
    }
}
