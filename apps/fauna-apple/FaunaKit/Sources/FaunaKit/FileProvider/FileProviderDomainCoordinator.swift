import FaunaFFISwift
import Foundation

#if canImport(FileProvider)
    import FileProvider

    /// A domain removal was refused because un-recorded local edits still exist
    /// under it (`on-demand-files.md` § Multi-account × File Provider, consequence 2:
    /// iOS has no `.preserveDirtyUserData`, so a plain removal would let the OS
    /// discard them). The domain stays registered — the OS's pending-change
    /// retry drains the edits — and the caller retries the removal later.
    /// `pendingCount` is `-1` when the state DB was unreadable (fail closed).
    public struct FileProviderRemovalRefused: Error, Sendable {
        public let domainId: String
        public let pendingCount: Int
    }

    /// Async wrappers over the `NSFileProviderManager` domain lifecycle. A domain's
    /// identifier IS the set's actor-scoped identity, `<ref-component>@<actor-id-hex>`
    /// (`local%3A1@…` — `FileProviderDomainIdentity`; `on-demand-files.md` § Apple
    /// File Provider binding, *the actor-scoped device identity*; the appex hands
    /// `domain.identifier` to `makeFileProviderHost(domainId:)`), so
    /// add/remove/list speak domain identifiers; the set name is only the
    /// display name.
    ///
    /// Shared FaunaKit (not app-local) so macOS and iOS (M4) drive domains
    /// identically (priority #2). watchOS has no FileProvider framework — this whole
    /// surface is `canImport`-gated and absent there.
    public enum FileProviderDomains {
        /// Register the File Provider domain for one folder. The calling
        /// process must embed the Fauna FP appex in its own bundle (the
        /// `.xcodeproj`-built app); from a bare binary the OS refuses — callers treat
        /// this as best-effort.
        public static func add(domainId: String, displayName: String) async throws {
            let domain = NSFileProviderDomain(
                identifier: NSFileProviderDomainIdentifier(rawValue: domainId),
                displayName: displayName
            )
            try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
                NSFileProviderManager.add(domain) { error in
                    if let error { cont.resume(throwing: error) } else { cont.resume() }
                }
            }
        }

        /// The removal of one identifier that reaches here outside a reconcile
        /// plan (sign-out, the bind-time yield), its scope from the same shared
        /// Rust parse the plan's `PresenceRemoval.scope` comes from, so both
        /// paths gate alike: a scoped identifier on ITS account's state for ITS
        /// set, and an identifier that scopes no set to any account (a bare ref
        /// or a set name from before actor scoping — never served, so nothing
        /// under it can drain; gating it would only wedge it, and the sign-out
        /// capability hold behind it, for ever) not at all.
        static func removal(domainId: String) -> PresenceRemoval {
            PresenceRemoval(
                identifier: domainId,
                scope: FileProviderDomainIdentity.parse(domainId).map {
                    PresenceScope(actorIdHex: $0.actorIdHex, folderId: $0.folderId)
                })
        }

        /// Remove one set's domain (the OS tears down its Finder/Files presence; the
        /// extension's private state under the app-group container is left on disk so
        /// re-adding resumes instead of re-populating from scratch).
        ///
        /// macOS: always `.preserveDirtyUserData` — every caller (toggle-off
        /// reconcile, the bind-time `yieldDomain`, sign-out, account switch)
        /// reaches a file whose upload is un-acked only through here, and the
        /// default `.removeAll` would delete it — the one removal-shaped breach
        /// of "the OS never discards a local copy whose upload is un-acked"
        /// (`on-demand-files.md` § Apple File Provider binding). The OS moves preserved
        /// dirty files aside and reports where. iOS: the SDK offers NO preserve
        /// mode (the headers mark both cases unavailable; the compiler is the
        /// authority), so removal is guarded by the **upload-drain gate**: while
        /// the set's state DB holds un-recorded local edits the removal throws
        /// [`FileProviderRemovalRefused`] instead of letting the OS discard them
        /// — the domain stays registered so the OS's pending-change retry can
        /// drain, and the caller retries later (`on-demand-files.md`
        /// § Multi-account × File Provider, consequence 2).
        public static func remove(domainId: String) async throws {
            try await remove(removal(domainId: domainId))
        }

        /// ``remove(domainId:)`` for a removal whose gate scope is already
        /// decided — the reconcile passes its plan's removals straight through.
        static func remove(_ removal: PresenceRemoval) async throws {
            let domainId = removal.identifier
            // Removal matches on the identifier alone; the display name is inert.
            let domain = NSFileProviderDomain(
                identifier: NSFileProviderDomainIdentifier(rawValue: domainId),
                displayName: domainId
            )
            #if os(macOS)
                let preservedAt: URL? = try await withCheckedThrowingContinuation { cont in
                    NSFileProviderManager.remove(domain, mode: .preserveDirtyUserData) {
                        url, error in
                        if let error {
                            cont.resume(throwing: error)
                        } else {
                            cont.resume(returning: url)
                        }
                    }
                }
                if let preservedAt {
                    logMessage(
                        level: .info, target: "fauna.fileprovider",
                        message:
                            "[fp] domain \(domainId) removed; un-uploaded local edits preserved at \(preservedAt.path)"
                    )
                }
            #else
                // The upload-drain gate (headless-pinned at the engine seam:
                // `set_unrecorded_rels`): the account is IN the identifier, so
                // the gate consults that account's scoped state — never the
                // currently-provisioned account's — and a foreign dirty domain
                // lingering across a switch still refuses. An unreadable DB
                // refuses (fail closed); an identifier that scopes no set to
                // any account skips the gate (`scope == nil`).
                if let scope = removal.scope,
                    let stateDir = SyncStateDir.resolve(actorIdHex: scope.actorIdHex, in: .container)
                {
                    let pending: [String]
                    do {
                        pending = try syncSetUnrecordedRels(
                            stateDir: stateDir.path, folderId: scope.folderId)
                    } catch {
                        logMessage(
                            level: .warn, target: "fauna.fileprovider",
                            message:
                                "[fp] domain \(domainId): un-recorded check failed — refusing removal (fail closed): \(error)"
                        )
                        throw FileProviderRemovalRefused(domainId: domainId, pendingCount: -1)
                    }
                    if !pending.isEmpty {
                        logMessage(
                            level: .info, target: "fauna.fileprovider",
                            message:
                                "[fp] domain \(domainId): \(pending.count) un-recorded local edit(s) pending upload — removal refused until they drain"
                        )
                        throw FileProviderRemovalRefused(
                            domainId: domainId, pendingCount: pending.count)
                    }
                }
                try await withCheckedThrowingContinuation {
                    (cont: CheckedContinuation<Void, Error>) in
                    NSFileProviderManager.remove(domain) { error in
                        if let error { cont.resume(throwing: error) } else { cont.resume() }
                    }
                }
            #endif
            FileProviderDomainOwners.clear(domainId: domainId)
        }

        /// The currently-registered domain identifiers (= the sets' actor-scoped
        /// identities; an identifier that scopes no set to an account — a bare ref or a
        /// set name — shows up here too, and the next reconcile removes it as
        /// undesired).
        public static func list() async throws -> [String] {
            try await withCheckedThrowingContinuation { cont in
                NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
                    if let error {
                        cont.resume(throwing: error)
                    } else {
                        cont.resume(returning: domains.map(\.identifier.rawValue))
                    }
                }
            }
        }

        /// The registered domains a `fauna.sync.changed` push names
        /// (file-sync.md § Remote-change nudge): every held set the push names
        /// — matched by its own name or name hash through the shared rule
        /// (`syncChangedNamesSet`, never a display-name compare: a shared set's
        /// label carries its owner, and a member can hold a set named like one
        /// of their own) — scoped to `actorHex` by its ref, the domain's
        /// identifier. A push the account holds no set for names nothing.
        static func domainIds(
            namedBy folder: String, folderHash: Data?, in sets: [PresenceSet], actorHex: String
        ) -> Set<String> {
            Set(
                sets
                    .filter {
                        syncChangedNamesSet(folder: folder, folderHash: folderHash, name: $0.setName)
                    }
                    .compactMap {
                        FileProviderDomainIdentity(actorIdHex: actorHex, folderId: $0.folderId)?
                            .domainId
                    })
        }

        /// Nudge the File Provider extension hosting each of `domainIds` to pull
        /// out-of-cadence (file-sync.md § Remote-change nudge — the
        /// `fauna.sync.changed` push arm; ``FileProviderCoordinator/signalChanged(folder:folderHash:)``
        /// decides which domains a push names). `signalEnumerator` is the only
        /// hook into the extension's separate process reachable from here; the
        /// OS answers it by invoking the extension's own `enumerateChanges`
        /// (`Fauna-FileProvider/FileProviderEnumerator.swift`), where the actual
        /// `host.refresh()` runs — this call only wakes that path sooner than its
        /// rescan-cadence backstop. Returns how many domains were signalled (0 =
        /// none of them is registered: this device's place does not accept,
        /// toggled off, or bound); callers treat it as best-effort.
        @discardableResult
        public static func signal(domainIds: Set<String>) async throws -> Int {
            guard !domainIds.isEmpty else { return 0 }
            let domains: [NSFileProviderDomain] = try await withCheckedThrowingContinuation {
                cont in
                NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
                    if let error {
                        cont.resume(throwing: error)
                    } else {
                        cont.resume(returning: domains)
                    }
                }
            }
            var signalled = 0
            for domain in domains where domainIds.contains(domain.identifier.rawValue) {
                try await NSFileProviderManager(for: domain)?.signalEnumerator(
                    for: .rootContainer)
                signalled += 1
            }
            return signalled
        }
    }

    /// The identity/nest material the app supplies for provisioning the app-dead
    /// capability. The seed never enters the store — only the derived `BackupKey`
    /// (`backupKeyDerive`) plus a minted renewable bearer are written.
    public struct FileProviderProvisioningContext: Sendable {
        public let nestUrl: String
        public let secretHex: String
        public let deviceIdHex: String
        public let deviceLabel: String

        public init(nestUrl: String, secretHex: String, deviceIdHex: String, deviceLabel: String) {
            self.nestUrl = nestUrl
            self.secretHex = secretHex
            self.deviceIdHex = deviceIdHex
            self.deviceLabel = deviceLabel
        }
    }

    /// Auto-appear reconcile + sign-out teardown for the per-set File Provider
    /// domains (`on-demand-files.md` § Apple File Provider binding: domains for
    /// the user's own folders this device accepts in, and § Shared sets on a
    /// capability host, decision 3: the folders shared with the account —
    /// behind `folder-on-demand-toggle`, default ON).
    ///
    /// Enforces the **one-local-presence-per-set** rule (`on-demand-files.md`
    /// § Apple File Provider binding; the banned shape is two engines writing one
    /// set from one device id): per set per device the local presence is exactly
    /// one of — a **bound always-resident folder** (the user's explicit
    /// `folder-location-*` binding; its engine runs in the agent/app host), the **FP
    /// domain** (the default-ON on-demand toggle; its engine runs in the
    /// extension), or **none** (RemoteOnly). A binding outranks the toggle: it is
    /// an explicit user act with a chosen on-disk location, while the toggle is
    /// the ambient zero-config default — so a bound set never gets a domain, and
    /// unbinding lets the domain reappear on the next reconcile.
    public enum FileProviderCoordinator {
        /// The sets the last reconcile was handed and the account they belong
        /// to — what a `fauna.sync.changed` push is matched against
        /// (``signalChanged(folder:folderHash:)``). In-process only: the push
        /// observer and the reconcile both run in the app.
        private static let held = HeldSets()

        private final class HeldSets: @unchecked Sendable {
            private let lock = NSLock()
            private var actorHex = ""
            private var sets: [PresenceSet] = []

            func set(actorHex: String, sets: [PresenceSet]) {
                lock.lock()
                defer { lock.unlock() }
                self.actorHex = actorHex
                self.sets = sets
            }

            func get() -> (actorHex: String, sets: [PresenceSet]) {
                lock.lock()
                defer { lock.unlock() }
                return (actorHex, sets)
            }

            func clear() { set(actorHex: "", sets: []) }
        }

        /// Nudge the domains a `fauna.sync.changed` push names
        /// (file-sync.md § Remote-change nudge — called from
        /// `FaunaClient.swift`'s push observer in the MAIN app process): the
        /// held sets the push names, each signalled by its domain identifier
        /// (``FileProviderDomains/domainIds(namedBy:folderHash:in:actorHex:)``).
        /// A push before the first reconcile names nothing and waits out the
        /// rescan backstop.
        @discardableResult
        public static func signalChanged(folder: String, folderHash: Data?) async throws -> Int {
            let (actorHex, sets) = held.get()
            return try await FileProviderDomains.signal(
                domainIds: FileProviderDomains.domainIds(
                    namedBy: folder, folderHash: folderHash, in: sets, actorHex: actorHex))
        }

        /// The one-local-presence decision (`on-demand-files.md` § Apple File
        /// Provider binding, arbitration precedence): the shared-Rust
        /// `fauna_folders_machine::on_demand_presence` plan — the sets this
        /// device is a delivery seat for (own folders its place accepts in, and
        /// the folders shared with the account) ∩ toggle-enabled ∖ bound, each
        /// set scoped to the account
        /// exactly once, then add / keep / backfill / foreign-held / remove
        /// against the registered identifiers — fed this platform's inputs:
        /// the bound refs as the stronger presence and the domain-owner record
        /// (`ownersAt:` is `FileProviderDomainOwners`' test seam). The set
        /// arithmetic lives in Rust, pinned there; this wrapper is only the
        /// input mapping.
        static func plan(
            actorHex: String, sets: [PresenceSet], boundSets: Set<String>,
            prefs: FfiOnDemandPrefsStore, registered: [String],
            ownersAt url: URL? = FileProviderDomainOwners.defaultURL()
        ) throws -> PresencePlan {
            try onDemandPresencePlan(
                actorIdHex: actorHex, sets: sets, stronger: boundSets.sorted(), prefs: prefs,
                registered: registered, owners: FileProviderDomainOwners.all(at: url))
        }

        /// Write the plan's backfill records — the reconcile's one owner-record
        /// write that needs no OS call, split out so the headless pin drives
        /// the real store (`at:` is `FileProviderDomainOwners`' test seam).
        static func backfillOwners(
            _ plan: PresencePlan, actorHex: String,
            at url: URL? = FileProviderDomainOwners.defaultURL()
        ) {
            for domainId in plan.backfill {
                FileProviderDomainOwners.record(domainId: domainId, actorIdHex: actorHex, at: url)
                logMessage(
                    level: .info, target: "fauna.fileprovider",
                    message:
                        "[fp] domain \(domainId): owner recorded (backfill of a registered domain with no owner on record)"
                )
            }
        }

        /// Converge the registered domains onto every set the account holds
        /// (`sets` — the shared door's list, `FfiFoldersClient.presenceSets`:
        /// own folders this device's place accepts in and the folders shared
        /// with the account, never mapped in Swift), filtered through the
        /// per-device toggle store (default ON)
        /// minus `boundSets` (the refs of the sets whose local presence is a
        /// bound always-resident folder — the caller reads its platform's
        /// binding surface; iOS has none and passes nothing) — the shared plan.
        /// A registered domain whose identifier is not a desired set's identity —
        /// another account's domain, a set that left Sync mode, or an identifier
        /// from before actor scoping (a bare ref or a set name) — is removed (no
        /// adoption: the 2026-09-24 baseline reset). Provisions the shared-Keychain
        /// capability before the first add (and refreshes it — bearer included —
        /// on every reconcile that wants ≥1 domain, so the extension's fresh-read
        /// bearer stays renewable without an extra loop).
        ///
        /// Best-effort by design: domain registration only works from the
        /// appex-embedding app bundle, so failures log and return rather than throw
        /// — the SPM-built bare binary (mac-app, e2e) reaches this code too.
        public static func reconcile(
            sets: [PresenceSet], boundSets: Set<String> = [],
            provisioning: FileProviderProvisioningContext
        ) async {
            // The account every identity this reconcile scopes to — derived
            // from the session secret up front (the same derivation
            // `provision` records into the slot), since the toggle store and
            // the desired identifiers are keyed by it before anything is
            // provisioned.
            let actorHex: String
            do {
                actorHex = data_to_hex(
                    try actorIdFromSecret(secret: hex_to_data(provisioning.secretHex)))
            } catch {
                logMessage(
                    level: .error, target: "fauna.fileprovider",
                    message: "[fp] reconcile skipped — actor id derivation failed: \(error)")
                return
            }
            held.set(actorHex: actorHex, sets: sets)

            let current: [String]
            do {
                current = try await FileProviderDomains.list()
            } catch {
                logMessage(
                    level: .warn, target: "fauna.fileprovider",
                    message: "[fp] domain list failed (no appex in this bundle?): \(error)")
                return
            }

            let plan: PresencePlan
            do {
                plan = try self.plan(
                    actorHex: actorHex, sets: sets, boundSets: boundSets,
                    prefs: FileProviderDomainPrefs.shared, registered: current)
            } catch {
                logMessage(
                    level: .error, target: "fauna.fileprovider",
                    message: "[fp] reconcile skipped — presence plan failed: \(error)")
                return
            }

            // The single-slot capability is provisioned for the active account
            // before its first add. Nothing desired → nothing to provision;
            // removals need no capability.
            if !(plan.add.isEmpty && plan.keep.isEmpty && plan.foreignHeld.isEmpty) {
                do {
                    let actorId = try await provision(provisioning)
                    await FileProviderSignerWatch.shared.watch(actorId: actorId)
                } catch {
                    logMessage(
                        level: .error, target: "fauna.fileprovider",
                        message: "[fp] capability provisioning failed — not adding domains: \(error)")
                    return
                }
            }

            for presence in plan.add {
                let domainId = presence.scopedId
                do {
                    try await FileProviderDomains.add(
                        domainId: domainId, displayName: presence.set.name)
                    FileProviderDomainOwners.record(domainId: domainId, actorIdHex: actorHex)
                    logMessage(
                        level: .info, target: "fauna.fileprovider",
                        message: "[fp] added domain \(domainId) for set \(presence.set.name)")
                } catch {
                    logMessage(
                        level: .warn, target: "fauna.fileprovider",
                        message: "[fp] add domain \(domainId) (\(presence.set.name)) failed: \(error)")
                }
            }
            backfillOwners(plan, actorHex: actorHex)
            for hold in plan.foreignHeld {
                logMessage(
                    level: .warn, target: "fauna.fileprovider",
                    message:
                        "[fp] domain \(hold.presence.scopedId) (wanted for set \(hold.presence.set.name)) is recorded to another account (\(hold.ownerHex.prefix(12))…) whose edits are still draining — not adopted; this set's domain is added once that account's drain completes and its domain is removed"
                )
            }
            for removal in plan.remove {
                let domainId = removal.identifier
                do {
                    try await FileProviderDomains.remove(removal)
                    logMessage(
                        level: .info, target: "fauna.fileprovider",
                        message: "[fp] removed domain \(domainId)")
                } catch {
                    logMessage(
                        level: .warn, target: "fauna.fileprovider",
                        message: "[fp] remove domain \(domainId) failed: \(error)")
                }
            }
        }

        /// Bound-folder pickup: remove one set's FP domain (by its bare
        /// `FolderRef` wire string — the binding surface's key) **before** its
        /// resident engine starts, closing the two-writer overlap window at the
        /// source (rather than waiting for the next full reconcile). The domain
        /// is the provisioned account's — the single slot is the active
        /// account's, the only one whose domains this device serves; another
        /// account's same-numbered ref is a different set on a different nest
        /// and is never touched. Best-effort — a no-domain / no-appex /
        /// unprovisioned state is already the goal.
        public static func yieldDomain(folderId: String) async {
            guard let creds = FileProviderCredentialStore.load(),
                let identity = FileProviderDomainIdentity(
                    actorIdHex: data_to_hex(creds.actorId), folderId: folderId)
            else { return }
            try? await FileProviderDomains.remove(domainId: identity.domainId)
        }

        /// Sign-out teardown: remove every registered domain, then revoke the
        /// shared-Keychain capability so a still-running extension fails closed.
        ///
        /// iOS drain-hold: a removal the upload-drain gate refused
        /// (`FileProviderRemovalRefused`) keeps its domain registered so the
        /// OS's pending-change retry can still drain — and then the capability
        /// must survive too, or the extension fails closed and the drain can
        /// never complete. The account in the identifier (checked in
        /// `makeFileProviderHost`) is what keeps that lingering capability from
        /// ever serving a *later* account's slot through the leftover domain;
        /// the next reconcile or the owner's next sign-in retries the removal.
        /// Any other removal error keeps the old best-effort shape (revoke
        /// regardless).
        public static func signOut() async {
            var drainPending = false
            if let current = try? await FileProviderDomains.list() {
                for domainId in current {
                    do {
                        try await FileProviderDomains.remove(domainId: domainId)
                    } catch let refusal as FileProviderRemovalRefused {
                        drainPending = true
                        logMessage(
                            level: .warn, target: "fauna.fileprovider",
                            message:
                                "[fp] sign-out: domain \(refusal.domainId) kept (\(refusal.pendingCount) un-recorded edit(s) pending) — capability retained until the drain completes"
                        )
                    } catch {
                        logMessage(
                            level: .info, target: "fauna.fileprovider",
                            message: "[fp] sign-out: remove domain \(domainId) failed: \(error)")
                    }
                }
            }
            held.clear()
            if !drainPending {
                await FileProviderSignerWatch.shared.stop()
                FileProviderCredentialStore.revoke()
            }
        }

        /// Derive + write the app-dead capability: owner `BackupKey` from the seed,
        /// a freshly-minted renewable bearer, the public ids, and — once this
        /// machine is enrolled — its principal's change signer. Idempotent
        /// (delete-then-add per account in the store). Returns the actor id the
        /// slot now holds — the account every identity this reconcile scoped to.
        private static func provision(_ ctx: FileProviderProvisioningContext) async throws -> Data {
            let secret = hex_to_data(ctx.secretHex)
            let backupKey = try backupKeyDerive(secret: secret)
            let actorId = try actorIdFromSecret(secret: secret)
            let bearer = try await mintBearer(nestUrl: ctx.nestUrl, secret: secret)
            FileProviderCredentialStore.provision(
                FileProviderCredentials(
                    nestURL: ctx.nestUrl,
                    actorId: actorId,
                    deviceId: hex_to_data(ctx.deviceIdHex),
                    deviceLabel: ctx.deviceLabel,
                    backupKey: backupKey
                ),
                bearer: bearer.token,
                signer: FileProviderSignerWatch.machineSigner(actorId: actorId)
            )
            // Intra-process read-back guard, the same one the FP test CLI runs:
            // the writer swallows `SecItemAdd`'s status, and the one failure that
            // matters here — the data-protection keychain refusing the app-group
            // access group (`errSecMissingEntitlement` −34018 under ad-hoc
            // signing; anything else on a signed build) — otherwise surfaces
            // only as the extension failing closed with no line naming the cause.
            // A supervised install round reads this line instead of guessing.
            if FileProviderCredentialStore.load() == nil {
                let probe = FileProviderCredentialStore.diagnoseAccessGroupRoundTrip()
                logMessage(
                    level: .error, target: "fauna.fileprovider",
                    message:
                        "[fp] capability written but not readable back — the extension will fail closed "
                        + "(access-group probe: SecItemAdd=\(probe.add), SecItemCopyMatching=\(probe.read))"
                )
            } else {
                logMessage(
                    level: .info, target: "fauna.fileprovider",
                    message: "[fp] capability provisioned into the shared data-protection keychain")
            }
            return actorId
        }
    }

    /// Keeps the extension's provisioned **change signer** current
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records* (1),
    /// *The capability host*). The machine principal changes on its own
    /// schedule — the first enrollment usually completes after the sign-in's
    /// reconcile has already provisioned the capability, and a `SyncWrite`
    /// re-certification or a re-minted writer key can land at any time — so
    /// the app re-reads its principal slot whenever this process writes it
    /// (`principalSlotWritesAfter`) and re-provisions the signer beside the
    /// bearer. Only while the capability is the watched account's: a switch or
    /// a sign-out stops it.
    actor FileProviderSignerWatch {
        static let shared = FileProviderSignerWatch()

        private var task: Task<Void, Never>?
        private var actorId: Data?

        /// This machine's signer for `actorId`, or `nil` when it is not
        /// enrolled with `SyncWrite` yet (or the slot cannot be read — logged).
        static func machineSigner(actorId: Data) -> FfiChangeSignerCarriage? {
            do {
                return try machineChangeSignerCarriage(actorId: actorId)
            } catch {
                logMessage(
                    level: .warn, target: "fauna.fileprovider",
                    message: "[fp] change signer unreadable from the principal slot: \(error)")
                return nil
            }
        }

        /// Start re-provisioning `actorId`'s signer on every slot write
        /// (idempotent for the account already watched).
        func watch(actorId: Data) {
            if self.actorId == actorId, task != nil { return }
            task?.cancel()
            self.actorId = actorId
            task = Task.detached {
                var seen: UInt64 = 0
                while !Task.isCancelled {
                    seen = await principalSlotWritesAfter(seen: seen)
                    if Task.isCancelled { return }
                    // Never onto another account's capability.
                    guard FileProviderCredentialStore.load()?.actorId == actorId else { return }
                    let signer = Self.machineSigner(actorId: actorId)
                    FileProviderCredentialStore.provisionSigner(signer)
                    logMessage(
                        level: .info, target: "fauna.fileprovider",
                        message: signer == nil
                            ? "[fp] change signer cleared (the principal is not enrolled with SyncWrite)"
                            : "[fp] change signer re-provisioned after a principal-slot write")
                }
            }
        }

        func stop() {
            task?.cancel()
            task = nil
            actorId = nil
        }
    }

    /// Per-set, per-account, per-device "Show in Finder / Files" preference
    /// backing `folder-on-demand-toggle`: the shared-Rust store
    /// (`FfiOnDemandPrefsStore` over `fauna_folders_machine::on_demand_presence`
    /// — default ON, keyed by the set's actor-scoped identity, refusing a bare
    /// ref or a set name), so apple and android keep one store. Device-local
    /// by design — the same class of sanctioned device-local config as the
    /// folder map (`on-demand-files.md` § Hosting multiple on-demand folders),
    /// never nest state.
    ///
    /// Only the app reads or writes it (the toggle and the reconcile; the
    /// extension never consults it), so the file lives in the **app's own**
    /// consent domain (`SyncStateDir.appDomain`): the user domain on macOS,
    /// the app-group container on iOS, where it is the app's only one
    /// (`on-demand-files.md` § Apple File Provider binding, *state
    /// unification*). An iOS e2e launch keeps the unscoped in-sandbox dir,
    /// never the machine-global container (testing.md § conventions point 10).
    public enum FileProviderDomainPrefs {
        static let filename = "on-demand-prefs.json"

        /// The app's one store — one instance, so the toggle's writes and the
        /// reconcile's reads share the FFI's in-process lock.
        public static let shared: FfiOnDemandPrefsStore = open(at: defaultURL())

        static func defaultURL() -> URL {
            let base =
                SyncStateDir.e2eKeepsFlatLayout
                ? SyncStateDir.appSupportSyncDir
                : (SyncStateDir.base(of: SyncStateDir.appDomain) ?? SyncStateDir.appSupportSyncDir)
            return base.appendingPathComponent(filename)
        }

        /// Open the store at `url`.
        static func open(at url: URL) -> FfiOnDemandPrefsStore {
            FfiOnDemandPrefsStore(path: url.path)
        }

        public static func isEnabled(domainId: String, store: FfiOnDemandPrefsStore = shared) -> Bool {
            store.isEnabled(scopedId: domainId)
        }

        /// Persist one set's choice. A failed write is logged, not thrown: the
        /// toggle has no error surface, and the store still reads the old value.
        public static func setEnabled(
            _ enabled: Bool, domainId: String, store: FfiOnDemandPrefsStore = shared
        ) {
            do {
                try store.setEnabled(scopedId: domainId, enabled: enabled)
            } catch {
                logMessage(
                    level: .warn, target: "fauna.fileprovider",
                    message: "[fp] on-demand pref \(domainId) not saved: \(error)")
            }
        }
    }
#endif
