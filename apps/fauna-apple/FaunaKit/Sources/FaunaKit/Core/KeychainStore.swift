import Foundation
import Security

public class KeychainStore {
    /// The `kSecAttrService` namespace this store owns. Internal (not private) only so the
    /// copy-not-move unit tests can seed raw rows under the same service the store queries
    /// (`KeychainCopyNotMoveTests`); never a production seam.
    ///
    /// macOS, iOS, **and** watchOS share ONE value (`AppleIdentifiers.KeychainService.account`)
    /// — they are different devices, so the shared name never collides, exactly like
    /// ``AppleIdentifiers/KeychainService/push`` and ``AppleIdentifiers/KeychainService/fileProvider``
    /// already are for macOS+iOS. No per-platform conditional leaf here any more
    /// (`installers/macos.md` § Identifier domain — the keychain-service tier follows the
    /// purpose-leaf convention, never a platform word or a bundle-id echo).
    let service: String = AppleIdentifiers.KeychainService.account

    /// When true, all operations use the E2E store below instead of the real
    /// Keychain. Activated during E2E testing to avoid macOS Keychain access
    /// dialogs that block automation.
    private let inMemory: Bool
    private static var memoryStore: [String: String] = [:]

    /// The four `SecItem*` operations, abstracted behind a seam so the copy-not-move mechanism
    /// (§ iCloud-backup accessibility policy) is verifiable headlessly. In production this is the
    /// real system keychain (`SystemKeychainBackend`); unit tests inject a fake that models the
    /// keychain primary key `(service, account, synchronizable)`, leaving only real cross-device
    /// iCloud propagation as the manual last inch (`KeychainAccessibilityTests`). Unused on the
    /// `inMemory` E2E path, which never reaches the real keychain.
    private let backend: KeychainBackend

    /// Guards `memoryStore` and its optional file backing. The pending-factory-reset
    /// write must be durable **before it returns** (gap CR-1 — the row exists precisely
    /// to survive a SIGKILL landing microseconds later), so it cannot be deferred to a
    /// background queue; a lock keeps the read-back-after-write check
    /// (`mintAndPersistPendingFactoryReset`) honest against the automation agent's threads.
    private static let storeLock = NSLock()

    /// E2E-only **durable** backing for the store. When the harness passes
    /// `FAUNA_E2E_CREDENTIAL_DIR`, `memoryStore` is re-read from — and flushed to —
    /// `<dir>/keychain.json`, so a SIGKILL + relaunch reads back what the previous process
    /// wrote. This is the apple analogue of the credential dir linux already uses, and it
    /// is what `PlatformDriver.preserve_state_across_relaunch()` pins across a relaunch.
    ///
    /// **Why it has to exist at all.** Gaps CR-1/CR-2 are entirely about a slot SURVIVING a
    /// crash. A harness whose store dies with the process cannot test that: the assertion
    /// would be vacuous (it would "pass" on a client that never wrote a slot), which is why
    /// those journeys skip on any driver that cannot pin the store. Apple's store used to be
    /// a process-static Swift dict, so it could not pin by construction.
    ///
    /// Absent the variable this stays `nil` and the store is purely process-static — every
    /// launch starts empty, which is what most journeys want (the mid-claim journey *relies*
    /// on a relaunch forgetting, to assert it lands back on create-identity).
    ///
    /// A computed property, not a frozen `static let`: the environment never changes mid-launch
    /// in production, so this costs nothing there, but a `let` would freeze at whichever value
    /// the FIRST `KeychainStore` operation in the process saw — poisoning every later unit test
    /// in the same test binary that wants a real (throwaway) `FAUNA_E2E_CREDENTIAL_DIR`.
    ///
    /// **The whole read is `#if DEBUG`, not just the `FaunaE2E.isActive`
    /// predicate** (convention 15, `e2e-automation-surface-gating.md` § The
    /// convention): this relocates the store where the identity secret is read
    /// AND written, so severity is the payload's — the same call the shared
    /// `fauna_credential_store::cred_file_dir()` and windows' `CredentialStore.Build`
    /// got. A constant-`nil` twin folded by the optimizer is not a gate here.
    private static var e2eFileURL: URL? {
        #if DEBUG
        guard FaunaE2E.isActive, let dir = E2eEnv.credentialDir, !dir.isEmpty else { return nil }
        return URL(fileURLWithPath: dir).appendingPathComponent("keychain.json")
        #else
        return nil
        #endif
    }
    /// True when the harness handed us a credential dir and therefore owns the store's
    /// lifecycle — a fresh dir per launch IS a clean start, and a pinned dir is a
    /// deliberate `preserve_state_across_relaunch()`. The apps read this to skip their
    /// launch-time E2E wipe, which would otherwise destroy exactly the state the
    /// crash-recovery journeys are asserting survives.
    public static var e2eHarnessOwnsStore: Bool { e2eFileURL != nil }

    /// Re-read the durable backing into `memoryStore`. Call with `storeLock` held, at the top
    /// of EVERY operation — never once per process.
    ///
    /// **The file is the store; `memoryStore` is one operation's view of it** — the discipline
    /// the Rust credential store's File backend keeps (`fauna_credential_store::cred_file_read`
    /// re-reads on every operation), for the same reason: another process writes this file too.
    /// iOS has no native Rust keyring arm, so the shared `fauna-account-store` namespace rides
    /// the foreign seam into this very store, and the e2e harness restores a relaunched
    /// machine's store-principal slot into the file at the actor's first sign-in
    /// (`docs/goal/architecture/e2e-conventions.md` convention 10, the principal-slot carry). A
    /// backing loaded once per process never saw that write, and flushed its stale map over it
    /// on the next save. A missing or unparseable file reads as empty, as `cred_file_read`'s
    /// does. Without a durable backing this is a no-op: the process-static map is the store.
    private static func e2eReload() {
        guard let url = e2eFileURL else { return }
        guard let data = try? Data(contentsOf: url),
              let dict = try? JSONDecoder().decode([String: String].self, from: data)
        else {
            memoryStore = [:]
            return
        }
        memoryStore = dict
    }

    /// Run one mutation as a whole-file read-modify-write under `<file>.lock` — the lock
    /// `fauna_credential_store::lock_cred_file` takes for the same file shape, and the one the
    /// e2e harness's own writes into this file take — so a write another process lands between
    /// two of this process's operations is never flushed over. Call with `storeLock` held;
    /// `body` mutates `memoryStore` and flushes. Advisory degrade, as the Rust lock's: an
    /// unlockable path runs the read-modify-write unserialized rather than refusing the write.
    private static func e2eMutate(_ body: () -> Void) {
        guard let url = e2eFileURL else { return body() }
        let fd = open(url.path + ".lock", O_RDWR | O_CREAT | O_CLOEXEC, 0o600)
        if fd >= 0 { _ = flock(fd, LOCK_EX) }
        defer {
            if fd >= 0 {
                _ = flock(fd, LOCK_UN)
                close(fd)
            }
        }
        e2eReload()
        body()
    }

    /// Flush the durable backing. Call with `storeLock` held. Returns whether the on-disk
    /// file now matches `memoryStore` — `true` when there is no durable backing to flush at
    /// all (the ordinary in-memory-only e2e path), or when the write actually landed.
    ///
    /// The return value exists for exactly one caller (`delete`, below): unlike `save`, whose
    /// in-memory and durable copies simply agree if the write silently fails (best-effort by
    /// signature, like every platform's real store — `SecItemAdd` failures are swallowed too,
    /// which is exactly why `mint_and_persist_pending_factory_reset` reads the row back rather
    /// than trusting the write), a *failed delete* must not remove the row from `memoryStore`
    /// while it is still sitting in the file — that would let this process's own read-back
    /// claim an erase that never reached the durable store.
    @discardableResult
    private static func e2eFlush() -> Bool {
        guard let url = e2eFileURL else { return true }
        guard let data = try? JSONEncoder().encode(memoryStore) else { return false }
        do {
            try data.write(to: url, options: .atomic)
            return true
        } catch {
            return false
        }
    }

    public init() {
        // Use an in-memory store under EITHER e2e driver — the XCUITest bridge
        // (FAUNA_E2E_BRIDGE) or the in-process automation server
        // (FAUNA_E2E_AGENT_PORT). This keeps the real system keychain untouched:
        // a terminal-launched, ad-hoc-signed app whose signature changes each
        // rebuild otherwise blocks on a securityd keychain-ACL prompt with no UI
        // to approve it (the in-process path launches the app directly, like
        // linux).
        self.inMemory = FaunaE2E.isActive
        self.backend = SystemKeychainBackend()
    }

    // MARK: - Keychain plane (macOS: data-protection vs legacy)
    //
    // macOS has TWO keychains (`ios.md` § Credential Storage, *keychain plane*): the
    // legacy file-based `login.keychain-db`, whose per-item ACLs bind to a code identity —
    // one "Fauna wants to access key …" dialog per item whenever the identity changes, and
    // "Always Allow" that cannot stick across an ad-hoc rebuild — and the iOS-style
    // **data-protection** keychain, where access is by entitlement (no ACL dialogs, no
    // session/default-keychain semantics). The store reads and writes exactly ONE of them —
    // the data-protection plane whenever the running binary's signature can reach it, the
    // legacy plane otherwise — and never consults the other: there is no cross-plane read
    // or copy-forward (`version-compatibility.md` § Dimension 2, the fourth ratified
    // exception, removed it — no row predating the data-protection plane exists).
    //
    // Which plane is writable is a property of the SIGNATURE, not a preference: the
    // data-protection keychain is reachable only through an access group the app is entitled
    // to — the app-only `AppleIdentifiers.accountKeychainGroup`, an app group rather than the
    // File Provider's shared one so the sandboxed extension can never read the identity seed —
    // and an ad-hoc dev build gets `errSecMissingEntitlement` (−34018) on every query there
    // (measured 2026-07-20). So the plane is PROBED once per process and an ad-hoc build
    // lives on the legacy plane. iOS has one keychain: both planes decorate a query
    // identically (no-op).

    /// Which of macOS's two keychains a query addresses.
    enum Plane: Equatable {
        case dataProtection
        case legacy
    }

    /// Whether this platform has a second (legacy) plane at all (macOS only).
    static var hasLegacyPlane: Bool {
        #if os(macOS)
            return true
        #else
            return false
        #endif
    }

    /// The row used only to probe plane availability — never written.
    private static let planeProbeAccount = "__plane_probe__"

    /// The system keychain's probed plane, once per process (the answer is a property of the
    /// running binary's signature, so re-probing per instance would only re-log it). Test
    /// backends probe per instance. Guarded by `storeLock`.
    private static var systemWritePlane: Plane?

    /// The plane this store WRITES (and reads first): probed lazily on first real-keychain use.
    private lazy var writePlane: Plane = {
        let isSystem = backend is SystemKeychainBackend
        if isSystem {
            Self.storeLock.lock()
            let cached = Self.systemWritePlane
            Self.storeLock.unlock()
            if let cached { return cached }
        }
        let plane = probeWritePlane()
        if isSystem {
            Self.storeLock.lock()
            Self.systemWritePlane = plane
            Self.storeLock.unlock()
        }
        return plane
    }()

    /// One read of a row that need not exist, addressed to the data-protection plane:
    /// `errSecItemNotFound` is the plane answering "reachable, empty", `errSecSuccess`
    /// "reachable, populated"; anything else — `errSecMissingEntitlement` above all — is the
    /// signature not reaching it, and the store stays on the legacy plane. A read never
    /// prompts (the ACL dialogs are a legacy-plane, per-item affair), so the probe is silent.
    private func probeWritePlane() -> Plane {
        guard Self.hasLegacyPlane else { return .dataProtection }
        var query = Self.matchQuery(
            service: service, account: Self.planeProbeAccount, plane: .dataProtection)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        let (status, _) = backend.copyMatching(query)
        let plane: Plane =
            (status == errSecSuccess || status == errSecItemNotFound) ? .dataProtection : .legacy
        logMessage(
            level: .info, target: "fauna.keychain",
            message:
                "[keychain] write plane: \(plane == .dataProtection ? "data-protection" : "legacy") "
                + "(probe status \(status); −34018 = this signature is not entitled to the account access group)"
        )
        return plane
    }

    /// The attributes that address `plane`. macOS: the data-protection plane is selected by
    /// `kSecUseDataProtectionKeychain` and addressed through the app-only account access
    /// group; the legacy plane is the default file-based keychain, addressed exactly as the
    /// store always has (no key at all — a synchronizable backup copy written there lands in
    /// iCloud Keychain regardless, and an explicit `false` would refuse it). iOS: one
    /// keychain, nothing to add for either.
    static func planeAttributes(_ plane: Plane) -> [String: Any] {
        #if os(macOS)
            switch plane {
            case .dataProtection:
                return [
                    kSecUseDataProtectionKeychain as String: true,
                    kSecAttrAccessGroup as String: AppleIdentifiers.accountKeychainGroup,
                ]
            case .legacy:
                return [:]
            }
        #else
            return [:]
        #endif
    }

    /// Test seam: force the **real-keychain** branch against an injected fake backend, so the
    /// copy-not-move mechanism (§ iCloud-backup accessibility policy) can be exercised in a plain
    /// unsigned SPM test process — which cannot create a real `synchronizable` `SecItem` (see
    /// `KeychainAccessibilityTests`). `inMemory` is forced off so `save`/`load`/`setICloudBackup`
    /// take the same code path production does, only reading/writing the fake.
    init(testBackend: KeychainBackend) {
        self.inMemory = false
        self.backend = testBackend
    }

    // MARK: - iCloud-backup accessibility policy
    //
    // Ratified 2026-07-10 (`docs/goal/architecture/apps/ios.md` § Credential Storage;
    // cross-app contract `common.md` § Credential storage). The identity secret must
    // never *silently* migrate off the device, so the store writes every row **device-bound
    // by default** (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, non-synchronizable). An
    // opt-in apple-only Settings toggle ("Back up identity to iCloud Keychain") adds a
    // `synchronizable` + `AfterFirstUnlock` backup copy. iCloud sync requires
    // `kSecAttrSynchronizable`, which is *incompatible* with the `ThisDeviceOnly` accessibility
    // classes, so the backup copy is a distinct keychain primary key, not a flag flip.
    //
    // **Copy-not-move.** The toggle keeps each row's
    // device-bound *working* copy and, when enabled, ADDS the synchronizable *backup* copy;
    // opt-out deletes only the backup copy. This is the one rule that makes the toggle's
    // "per-device choice" framing true at the keychain layer: a `synchronizable` item is ONE
    // shared row per (service, account) across the whole Apple-ID iCloud-Keychain circle, and
    // its **deletion propagates circle-wide**. If opt-in *moved* the secret (deleted the
    // device-bound source), an opted-in device's only copy would be that shared circle row — so
    // one device toggling OFF, or any circle-wide delete, would destroy the *other* opted-in
    // devices' secret remotely (lockout). Keeping a `ThisDeviceOnly` working copy on every
    // device means no device's working copy is ever the shared row, so deleting the circle
    // (backup) copy can never take a working copy with it.
    //
    // `load` prefers the device-bound copy over the circle copy (falling back to the circle copy
    // only on a freshly-restored device that holds nothing else), so that under a shared Apple ID
    // with *different* fauna identities — where fixed-key rows collide on one circle
    // row and clobber each other — each device still reads its own device-bound truth, never a
    // sibling identity's clobbered value (face (b) of the review). The per-actor account-registry
    // rows are already identity-namespaced and never collide; only a fixed-key row's
    // *backup* copy is best-effort under that narrow config — the working copy is always correct.
    //
    // The policy applies uniformly to the whole service namespace, not just the secret row:
    // `KeychainSecretStore` (the account-registry seam) is deliberately policy-free — a
    // key→value map that must not learn which logical keys are identity material — so the
    // store cannot single out "just the secret". This matches `deleteAll()`'s existing
    // sweep-the-service philosophy: a row added later is covered because it is in the
    // namespace, not because someone remembered to list it.

    /// Device-local "back up identity to iCloud Keychain" preference row. Lives in this
    /// service (so a factory reset's namespace sweep clears it — a fresh identity defaults
    /// back to device-bound), but is ALWAYS written device-bound + non-synchronizable and is
    /// excluded from the policy rewrite: whether *this device* pushes to iCloud is a
    /// per-device choice, never itself a synced value.
    private static let backupPreferenceAccount = "icloud_backup_enabled"

    /// The accessibility class + `synchronizable` flag a credential row is written under.
    struct AccessPolicy: Equatable {
        let accessible: CFString
        let synchronizable: Bool

        /// Device-bound (default, opt-out) vs. iCloud-synchronizable (opt-in). The
        /// `ThisDeviceOnly` class excludes the item from iCloud Keychain sync *and* from
        /// encrypted-device-backup restore; `AfterFirstUnlock` + `synchronizable` opts into
        /// both.
        static func forBackup(enabled: Bool) -> AccessPolicy {
            enabled
                ? AccessPolicy(accessible: kSecAttrAccessibleAfterFirstUnlock, synchronizable: true)
                : AccessPolicy(
                    accessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, synchronizable: false)
        }

        /// The `kSecAttrAccessible*` value as a plain string — for assertions and logging
        /// (`CFString` has no synthesized `Equatable`).
        var accessibleString: String { accessible as String }

        static func == (lhs: AccessPolicy, rhs: AccessPolicy) -> Bool {
            lhs.accessibleString == rhs.accessibleString && lhs.synchronizable == rhs.synchronizable
        }
    }

    /// Query that matches a single row **regardless of its synchronizable state**. This is
    /// the one rule that keeps opting into iCloud backup from locking the user out: once a
    /// row is re-written `synchronizable`, a plain (implicitly non-sync) lookup no longer
    /// finds it — so every read/delete matches `kSecAttrSynchronizableAny`.
    static func matchQuery(service: String, account: String, plane: Plane) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: kSecAttrSynchronizableAny,
        ].merging(planeAttributes(plane)) { _, plane in plane }
    }

    /// The `SecItemAdd` payload for a row under a given policy.
    static func addQuery(
        service: String, account: String, data: Data, policy: AccessPolicy, plane: Plane
    ) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrAccessible as String: policy.accessible,
            kSecAttrSynchronizable as String: policy.synchronizable,
            kSecValueData as String: data,
        ].merging(planeAttributes(plane)) { _, plane in plane }
    }

    /// Service-wide enumeration of every row in both sync states, returning account name,
    /// data, and the current `synchronizable` flag — the input to `rewriteAllCredentials`.
    static func enumerateQuery(service: String, plane: Plane) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrSynchronizable as String: kSecAttrSynchronizableAny,
            kSecReturnAttributes as String: true,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll,
        ].merging(planeAttributes(plane)) { _, plane in plane }
    }

    // MARK: - Raw (string-keyed) access

    /// The store is **string-keyed** only. The shared account registry
    /// (`fauna_client_accounts`, reached over the `FfiSecretStore` seam — see
    /// `KeychainSecretStore`) addresses it by *logical* keys that are unbounded by
    /// construction — `fauna/index` plus a `fauna/{actor_id}/…` triple **per
    /// account** — and is deliberately the only thing that knows the layout (the
    /// platform's job is a key→value store and nothing else —
    /// `docs/goal/architecture/long-term-store.md` § Multi-account evolution →
    /// Shared seam). The typed `Key` enum that once named the pre-registry
    /// single-slot rows is retired with them (§ Downgrade mirror +
    /// abandoned-append recovery, RETIRED 2026-09-24).
    public func save(rawKey: String, value: String) throws {
        if inMemory {
            Self.storeLock.lock()
            defer { Self.storeLock.unlock() }
            Self.e2eMutate {
                Self.memoryStore[rawKey] = value
                Self.e2eFlush()
            }
            return
        }
        let data = Data(value.utf8)
        // Remove any prior copies in EITHER sync state, then re-land the copy-set the current
        // policy requires. Copy-not-move (§ iCloud-backup accessibility policy): the device-bound
        // working copy is written FIRST and always exists; the synchronizable *backup* copy is
        // added only when opted in, so no device's only copy is ever the shared iCloud-circle row.
        let plane = writePlane
        _ = backend.delete(Self.matchQuery(service: service, account: rawKey, plane: plane))
        let status = backend.add(
            Self.addQuery(
                service: service, account: rawKey, data: data, policy: .forBackup(enabled: false),
                plane: plane))
        guard status == errSecSuccess else {
            throw KeychainError.saveFailed(status)
        }
        // Synchronizable backup copy, best-effort like the rest of the store — a failure leaves the
        // working copy intact and `reconcileCredentialAccessibility()` re-lands the backup next launch.
        if iCloudBackupEnabled() {
            _ = backend.add(
                Self.addQuery(
                    service: service, account: rawKey, data: data, policy: .forBackup(enabled: true),
                    plane: plane))
        }
    }

    public func load(rawKey: String) -> String? {
        if inMemory {
            Self.storeLock.lock()
            defer { Self.storeLock.unlock() }
            Self.e2eReload()
            return Self.memoryStore[rawKey]
        }
        return loadValue(account: rawKey)
    }

    /// The read behind `load` and the backup preference: this device's own device-bound
    /// copy first, then either sync state (`kSecAttrSynchronizableAny`) — the anti-lockout
    /// invariant, which finds the iCloud-circle copy on a freshly-restored device holding
    /// nothing else. Always the write plane, always the current service.
    private func loadValue(account: String) -> String? {
        loadCopy(account: account, synchronizable: false)
            ?? loadCopy(account: account, synchronizable: nil)
    }

    /// Read one row's value from the write plane, matching a specific sync state (`false` =
    /// device-bound only) or — when `synchronizable` is nil — either state
    /// (`kSecAttrSynchronizableAny`).
    private func loadCopy(account: String, synchronizable: Bool?) -> String? {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ].merging(Self.planeAttributes(writePlane)) { _, plane in plane }
        if let synchronizable {
            query[kSecAttrSynchronizable as String] = synchronizable
        } else {
            query[kSecAttrSynchronizable as String] = kSecAttrSynchronizableAny
        }
        let (status, item) = backend.copyMatching(query)
        guard status == errSecSuccess, let data = item as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    public func delete(rawKey: String) {
        if inMemory {
            Self.storeLock.lock()
            defer { Self.storeLock.unlock() }
            Self.e2eMutate {
                let removed = Self.memoryStore.removeValue(forKey: rawKey)
                if !Self.e2eFlush(), let removed {
                    // The durable backing refused the write (a read-only credential directory,
                    // e.g.) — the row is still on disk, so the in-process view must keep
                    // reporting it too, or this process's own read-back would see a clean erase
                    // that never actually reached the file.
                    Self.memoryStore[rawKey] = removed
                }
            }
            return
        }
        // Match either sync state so both the device-bound working copy and the synchronizable
        // backup copy are removed.
        _ = backend.delete(Self.matchQuery(service: service, account: rawKey, plane: writePlane))
    }

    /// Delete **every** row this store owns — the whole service namespace, not a list.
    ///
    /// Exists because the e2e between-tests reset (`resetToFactory`) used to delete a
    /// hand-listed three — `secret_key`, `node_url`, `device_id` — and silently left the
    /// rest behind: the cached handle/domain/tier, the pending-invite slot, and (the one
    /// that bit) the **pending-factory-reset slot**. The apple e2e app process is
    /// session-scoped and reused across the tests in a module, so a slot one test wrote
    /// survived into the next, whose app then launched straight onto a pre-filled claim
    /// page for a nest that no longer existed. That went unnoticed only because nothing
    /// durable was ever *successfully* written to those slots before (the mint's handle
    /// was always empty under an injected session, so it always refused).
    ///
    /// It used to iterate `Key.allCases`, which fixed *that* bug but re-armed it one layer
    /// down the moment the account registry landed: the registry's rows (`fauna/index` and
    /// a `fauna/{actor_id}/…` triple per account) are **not** `Key` cases, so an
    /// enum-driven sweep would leave every identity on the device behind — a "factory
    /// reset" that silently preserves the account index is the same class of bug, wearing
    /// the multi-account hat. Sweeping the *service* is the shape that cannot rot: a row
    /// added later — by an enum case, by the registry, by anything — is wiped because it is
    /// in the namespace, not because someone remembered to list it.
    ///
    /// This is the apple half of the § Cleanup contract
    /// (`docs/goal/architecture/long-term-store.md`), whose shared twin is
    /// `AccountRegistry::clear_all()`.
    public func deleteAll() {
        if inMemory {
            Self.storeLock.lock()
            defer { Self.storeLock.unlock() }
            Self.e2eMutate {
                Self.memoryStore.removeAll()
                Self.e2eFlush()
            }
            return
        }
        // Sweep the whole service in BOTH sync states — a factory reset that silently
        // preserved an iCloud-synchronizable identity row would be the same class of bug the
        // enum-driven sweep once was, wearing the iCloud-backup hat.
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrSynchronizable as String: kSecAttrSynchronizableAny,
        ].merging(Self.planeAttributes(writePlane)) { _, plane in plane }
        _ = backend.delete(query)
    }

    public enum KeychainError: Error {
        case saveFailed(OSStatus)
    }

    // MARK: - iCloud backup toggle

    /// Whether this device backs the identity up to iCloud Keychain. Default `false`
    /// (device-bound) — a fresh or upgraded install never syncs until the user opts in.
    public func iCloudBackupEnabled() -> Bool {
        if inMemory {
            Self.storeLock.lock()
            defer { Self.storeLock.unlock() }
            Self.e2eReload()
            return Self.memoryStore[Self.backupPreferenceAccount] == "1"
        }
        // The same read as every credential row.
        return loadValue(account: Self.backupPreferenceAccount) == "1"
    }

    /// Persist the device-local preference. ALWAYS device-bound + non-synchronizable and
    /// written directly (not via `save`, which would apply the very policy this drives) —
    /// the "do I push to iCloud" choice is per-device and must never itself sync.
    private func writeBackupPreference(_ enabled: Bool) {
        let value = Data((enabled ? "1" : "0").utf8)
        if inMemory {
            Self.storeLock.lock()
            defer { Self.storeLock.unlock() }
            Self.e2eMutate {
                Self.memoryStore[Self.backupPreferenceAccount] = enabled ? "1" : "0"
                Self.e2eFlush()
            }
            return
        }
        let plane = writePlane
        _ = backend.delete(
            Self.matchQuery(service: service, account: Self.backupPreferenceAccount, plane: plane))
        _ = backend.add(
            Self.addQuery(
                service: service, account: Self.backupPreferenceAccount, data: value,
                policy: .forBackup(enabled: false), plane: plane))
    }

    /// Flip iCloud backup on/off. Records the device-local preference first (so any
    /// subsequent write already lands under the new policy), then re-writes every existing
    /// credential row to match. Best-effort by signature like the rest of the store: a
    /// partial failure never strands the user (reads match `kSecAttrSynchronizableAny`), and
    /// `reconcileCredentialAccessibility()` re-converges the rows on the next launch.
    public func setICloudBackup(enabled: Bool) {
        writeBackupPreference(enabled)
        rewriteAllCredentials(toBackupEnabled: enabled)
    }

    /// Bring every credential row's copy-set in line with the current preference. Runs at
    /// launch (idempotent, self-healing): it re-converges any row a crash mid-`setICloudBackup`
    /// left behind.
    public func reconcileCredentialAccessibility() {
        rewriteAllCredentials(toBackupEnabled: iCloudBackupEnabled())
    }

    /// Converge every row in the service namespace (except the device-local preference) to the
    /// copy-set the target policy requires. The in-memory store has no accessibility to migrate.
    ///
    /// Enumerates BOTH sync states and groups by account, because copy-not-move means an opted-in
    /// row exists as two copies (device-bound + synchronizable) under the same account.
    private func rewriteAllCredentials(toBackupEnabled enabled: Bool) {
        guard !inMemory else { return }
        let (status, result) = backend.copyMatching(
            Self.enumerateQuery(service: service, plane: writePlane))
        guard status == errSecSuccess, let items = result as? [[String: Any]] else { return }
        var deviceBoundData: [String: Data] = [:]
        var syncData: [String: Data] = [:]
        for item in items {
            guard let account = item[kSecAttrAccount as String] as? String,
                  let data = item[kSecValueData as String] as? Data,
                  account != Self.backupPreferenceAccount else {
                continue
            }
            if (item[kSecAttrSynchronizable as String] as? Bool) ?? false {
                syncData[account] = data
            } else {
                deviceBoundData[account] = data
            }
        }
        for account in Set(deviceBoundData.keys).union(syncData.keys) {
            convergeRow(
                account: account, deviceBoundData: deviceBoundData[account],
                syncData: syncData[account], enabled: enabled)
        }
    }

    /// Converge one account's copy-set to the target policy without ever leaving the device
    /// without a readable device-bound copy. Target OFF → exactly `{ device-bound }`; target ON →
    /// exactly `{ device-bound, synchronizable }`.
    ///
    /// Order is lockout-safe (§ policy): the device-bound *working* copy is ensured FIRST (added +
    /// read-back-verified when it has to be landed from the synchronizable value), and only then
    /// is the synchronizable *backup* copy added (ON) or deleted (OFF). A crash at any point leaves
    /// at least the device-bound copy, and deleting the circle-global synchronizable copy never
    /// removes a device's working copy. The device-bound copy's DATA is this device's own truth;
    /// the synchronizable value is only ever promoted to it on a restored device that has no
    /// device-bound copy yet (never re-promoting a possibly cross-identity-clobbered circle value
    /// when a real working copy exists).
    private func convergeRow(account: String, deviceBoundData: Data?, syncData: Data?, enabled: Bool) {
        guard let data = deviceBoundData ?? syncData else { return }
        let deviceBoundPolicy = AccessPolicy.forBackup(enabled: false)
        if deviceBoundData == nil {
            // No device-bound copy yet (a restored device holding only the synced item). Add it
            // from the synced value and verify it reads back before touching the sync copy.
            let addStatus = backend.add(
                Self.addQuery(
                    service: service, account: account, data: data, policy: deviceBoundPolicy,
                    plane: writePlane))
            if addStatus == errSecDuplicateItem {
                _ = backend.update(
                    syncStateKey(account: account, synchronizable: false),
                    [kSecAttrAccessible as String: deviceBoundPolicy.accessible])
            } else if addStatus != errSecSuccess {
                return  // could not land the working copy — leave everything, retry next launch
            } else if !verifyReadable(account: account, synchronizable: false, expected: data) {
                return  // working copy not verified — keep the source, retry next launch
            }
        }
        if enabled {
            // Ensure the synchronizable backup copy exists carrying the device-bound value.
            let syncPolicy = AccessPolicy.forBackup(enabled: true)
            let addStatus = backend.add(
                Self.addQuery(
                    service: service, account: account, data: data, policy: syncPolicy,
                    plane: writePlane))
            if addStatus == errSecDuplicateItem {
                _ = backend.update(
                    syncStateKey(account: account, synchronizable: true),
                    [kSecAttrAccessible as String: syncPolicy.accessible, kSecValueData as String: data])
            }
        } else {
            // Opt-out: delete ONLY the synchronizable (circle) copy; the device-bound copy stays.
            _ = backend.delete(syncStateKey(account: account, synchronizable: true))
        }
    }

    /// Primary-key query for one copy of a row in a specific sync state, in the write plane.
    private func syncStateKey(account: String, synchronizable: Bool) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: synchronizable,
        ].merging(Self.planeAttributes(writePlane)) { _, plane in plane }
    }

    /// True iff the given sync-state copy reads back with exactly `expected`.
    private func verifyReadable(account: String, synchronizable: Bool, expected: Data) -> Bool {
        var query = syncStateKey(account: account, synchronizable: synchronizable)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        let (status, item) = backend.copyMatching(query)
        return status == errSecSuccess && (item as? Data) == expected
    }
}

// MARK: - Keychain operation seam

/// The four `SecItem*` operations `KeychainStore` performs on the real keychain, behind a seam so
/// the copy-not-move mechanism is unit-testable against a fake (`KeychainCopyNotMoveTests`).
protocol KeychainBackend {
    func add(_ attributes: [String: Any]) -> OSStatus
    func copyMatching(_ query: [String: Any]) -> (status: OSStatus, item: AnyObject?)
    func update(_ query: [String: Any], _ attributesToUpdate: [String: Any]) -> OSStatus
    func delete(_ query: [String: Any]) -> OSStatus
}

/// Production backend: the real system keychain.
struct SystemKeychainBackend: KeychainBackend {
    func add(_ attributes: [String: Any]) -> OSStatus {
        SecItemAdd(attributes as CFDictionary, nil)
    }
    func copyMatching(_ query: [String: Any]) -> (status: OSStatus, item: AnyObject?) {
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        return (status, result)
    }
    func update(_ query: [String: Any], _ attributesToUpdate: [String: Any]) -> OSStatus {
        SecItemUpdate(query as CFDictionary, attributesToUpdate as CFDictionary)
    }
    func delete(_ query: [String: Any]) -> OSStatus {
        SecItemDelete(query as CFDictionary)
    }
}
