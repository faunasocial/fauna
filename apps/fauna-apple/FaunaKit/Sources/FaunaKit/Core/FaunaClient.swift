import Foundation
import SwiftData

@MainActor @Observable
public class FaunaClient {
    public let api: APIClient
    public let keychain: KeychainStore
    public let networkMonitor: NetworkMonitor
    public var modelContext: ModelContext

    /// The in-process **file-sync engine host** — the shared `fauna-sync-engine` run
    /// over UniFFI (`file-sync.md` § Apple apps — convergence design). Built once
    /// per authenticated session (it needs the live WS-RPC connection), `nil` until
    /// then. It runs construct-run-drop engine work only (photo ingress, restore,
    /// per-file state reads) and hosts **no resident engine** on either platform:
    /// macOS's resident watcher lives in the standalone `fauna-sync-agent`
    /// (`sync-agent.md` § Scope per platform), and iOS binds no folder in-process
    /// at all (`sync-engine-deployments.md` § Apple apps — convergence design).
    /// Held for the session's life, dropped at `shutdown()`.
    ///
    /// This replaced the bespoke Swift `SyncEngine` + FSEvents `DirectoryWatcher` +
    /// launchctl `SyncDaemonManager`: Swift keeps only the platform shells (folder
    /// binding UI, PhotoKit ingress, lifecycle glue, rendering) and holds no chunk
    /// pipeline and no per-file state store of its own.
    public private(set) var syncHost: FfiSyncEngineHost?

    /// Per-file display state behind the `sync-state-badge`, re-read from the
    /// host's `fileStates`.
    public let syncStates = SyncStatesStore()

    /// The unattested-member review roster cache
    /// (`succession-aftermath.md` § Propagation) — read by the thread-header
    /// chip marks AND the contacts badge, so both surfaces answer off the
    /// SAME cached roster rather than each keeping their own. Refreshed
    /// behind the aftermath's raise (`SuccessionAftermath`'s
    /// `configStageSettled` hook) and after every Keep/Remove; resets to
    /// empty for every new session by construction — a fresh `FaunaClient`
    /// per sign-in (mirrors web's per-actor `member-reviews.ts` store).
    public private(set) var memberReviewRoster: [FfiMemberReview] = []

    /// Re-read the roster. Best-effort: a failed read keeps the PREVIOUS
    /// roster rather than blanking live marks (a page repaint must not
    /// un-flag a real review — mirrors `refreshMailEpochSchedule`'s contract).
    public func reloadMemberReviewRoster() async {
        if let fresh = try? await api.memberReviewList() {
            memberReviewRoster = fresh
        }
    }

    /// The post-succession aftermath's per-leg progress
    /// (`succession-aftermath.md` § Re-key scope's "surfaced with progress"),
    /// read by `RecoveryKitSection` — `recordAftermathProgress` is the only
    /// writer, called from `SuccessionAftermath.run`'s `onProgress`. Resets to
    /// empty for every new session by construction, same as
    /// `memberReviewRoster` above.
    public private(set) var aftermathProgress = AftermathProgress()

    /// The open inherited-rule marks (`succession-aftermath.md` § Adjudicating
    /// what the aftermath carries across, the email-filter plane) — the ids of
    /// every filter rule the aftermath carried across that the owner has not yet
    /// answered. ONE cache feeding both surfaces: the Privacy list paints
    /// `filter-unattested-mark`/`filter-review-keep-button` off it, and its
    /// count is the Account line below — so a Keep or a delete takes the line
    /// down with the mark (linux's `store_marks` shape). Re-read behind the
    /// aftermath's raise (`configStageSettled`), on every Privacy visit and
    /// after every verdict; resets to empty for every new session by
    /// construction, like ``memberReviewRoster``.
    public private(set) var inheritedFilterMarks: [Int64] = []

    /// The count behind `recovery-kit-inherited-filters-status` — derived,
    /// never stored apart from the marks it counts.
    public var recoveryInheritedFiltersOpen: Int { inheritedFilterMarks.count }

    /// Re-read the open inherited-rule marks. Best-effort: a failed read keeps
    /// the PREVIOUS marks rather than blanking them — a transport blip must
    /// never silently hide a mark the owner has not answered (linux
    /// `read_filter_marks`' "leave the cache alone" contract).
    public func reloadInheritedFilterMarks() async {
        do {
            inheritedFilterMarks = try await api.filterMarksList()
        } catch {
            logMessage(level: .warn, target: "fauna.app",
                       message: "re-reading the inherited-filter marks failed: \(error)")
        }
    }

    /// Record one leg's line, as `LoggingAftermathSink.progress` hands it
    /// over. `line: nil` is recorded like any other value — a leg that
    /// settles into "nothing to report" must clear a stale line from an
    /// earlier pass, not leave it painted.
    public func recordAftermathProgress(leg: FfiAftermathLeg, line: LocalizedText?) {
        switch leg {
        case .backupRegrant: aftermathProgress.backupRegrant = line
        case .mlsReseal: aftermathProgress.mlsReseal = line
        case .grantRemint: aftermathProgress.grantRemint = line
        case .draftsReseal: aftermathProgress.draftsReseal = line
        case .mailBurn: aftermathProgress.mailBurn = line
        }
    }

    #if os(iOS)
    public let photoBackup: PhotoBackupEngine
    public let backgroundScheduler: BackgroundScheduler
    public let custodianBackup: CustodianBackupEngine
    #endif

    // No segment-backup upload driver on either platform: the **source nest** is
    // the writer (`message-segment-store.md` § Cross-location backup protocol),
    // and apple's in-app driver — macOS's always-on `BackupUploadDriver` +
    // `PowerMonitor`, iOS's `MailBackupPushKick` and the mail leg of the
    // `social.fauna.sync.upload` BGProcessingTask — was deleted at the slice-5
    // flip 2026-08-15 (`backup-restore.md` § Background Tasks → *Flip status
    // (slice 5)*). The Backups page reads per-destination status from the nest's
    // own `fauna.backup.status` projection, so it stays correct with every app
    // asleep. Do not re-add a driver to make the Task-delegation row name a
    // client runner — the row is meant to show the nest.

    /// The hex device id this client backs up under (`generate_device_id()`
    /// form). Stored so the iOS backup triggers can re-derive the coordinator,
    /// and read by the shared Backups view to plumb the SAME stable sync device
    /// id into the per-destination live-status read (`backups.md`
    /// § Per-destination status read).
    public let deviceId: String

    /// Long-lived loop that broadcasts `.faunaReconnected` on every WS-RPC
    /// reconnect (Track-2 feed/surface re-hydrate). See `startReconnectObserver`.
    private var reconnectObserverTask: Task<Void, Never>?

    /// Long-lived loop that dispatches every inbound push (`fauna.notification`,
    /// `resync_required`, …) to the surface it touches. See `startPushObserver`.
    private var pushObserverTask: Task<Void, Never>?

    /// Long-lived loop that broadcasts `.faunaKnockReceived` on every inbound
    /// `fauna.knock` — the *dedicated* knock seam the generic push loop can't carry.
    /// See `startKnockObserver`.
    private var knockObserverTask: Task<Void, Never>?

    /// The app-level Nests auto-renew loop (macOS) — see `startNestsAutoRenewLoop`.
    private var nestsAutoRenewTask: Task<Void, Never>?
    /// The page-less VM the auto-renew tick dispatches `AutoRenew` through, so
    /// the tick never depends on the Nests page being open.
    private let nestsAutoRenewVM = LinkedNestsVM()

    /// Fires the OS knock toast (`NotificationManager.postKnockNotification`)
    /// from `startKnockObserver`. Owned here rather than injected, mirroring
    /// `MessageBannerObserver`'s own `NotificationManager()` instance —
    /// each platform-firing consumer constructs its own (priority #1).
    private let notificationManager = NotificationManager()

    /// The sync-agent provisioner this session built, if any — the target of the
    /// remote-change nudge in `startPushObserver`'s `.syncChanged` arm.
    ///
    /// **WEAK, and that is the correctness argument, not an optimization.** The
    /// caller owns the lifecycle (`makeSyncAgentProvisioner`'s contract), and a
    /// retired provisioner must never be nudged: macOS clears its reference on
    /// sign-out, factory reset and the e2e retire-before-rebuild path, and a
    /// strong reference here would both keep a dead provisioner alive and let a
    /// nudge reach the *previous* actor's agent — a known failure mode.
    /// Weak means a retired provisioner simply reads back nil and the
    /// nudge no-ops.
    ///
    /// Registered at the one construction point rather than by the app, so no
    /// per-app wiring can forget it (priority #1). Nothing registers on iOS,
    /// where the File Provider extension is the sync mechanism and no agent is
    /// spawned; the arm below is a no-op there by construction.
    private weak var syncAgentProvisioner: FfiSyncAgentProvisioner?

    /// Live nest WS-RPC connection state, driving the global `connection-status`
    /// indicator at the top of the shell. Seeded `.connecting`; updated by
    /// `startConnectionStateObserver` off the shared `FfiNestClient`'s
    /// `connection_state()` watch — the *visible* half of the reconnect machinery
    /// (a transient swap shows as `.connecting`, never an error; `transport.md`
    /// § Connection-status indicator).
    public private(set) var connectionState: FfiConnectionState = .connecting

    /// Long-lived loop that publishes the live `ConnectionState` to
    /// `connectionState`. See `startConnectionStateObserver`.
    private var connectionStateObserverTask: Task<Void, Never>?

    /// The active session's actor id (lowercase hex). Written once per session
    /// construction — the account model is a serialized switcher (one active
    /// identity per running client; switch = full teardown + relaunch,
    /// `long-term-store.md` § Multi-account evolution), so there is exactly one
    /// meaningful value at a time — and consumed by `syncStateDir` so every
    /// session-scoped engine resolves the account's **own** scoped state dir
    /// (`file-sync.md` § Multi-account × File Provider, consequence 3).
    /// `nonisolated(unsafe)`: the serialized switch path is the only writer.
    ///
    /// ⚠ **Never resolve a per-instance credential off this** — the switch
    /// path can move it out from under an older `FaunaClient` still tearing
    /// down or finishing a build that started before the switch. Use [ownActorIdHex] instead for anything scoped to THIS
    /// instance's own session.
    public private(set) nonisolated(unsafe) static var activeActorIdHex: String?

    /// THIS instance's own actor id (lowercase hex), fixed at construction
    /// from its own `secretHex` and never touched again — unlike
    /// [activeActorIdHex], which the switch path rewrites out from under
    /// every other still-live instance.
    public let ownActorIdHex: String?

    /// THIS instance's own secret — the one `ownActorIdHex` is derived from,
    /// and the only secret `start()`/`resume()` may authenticate with.
    ///
    /// ⚠ **Never a store read of the ACTIVE account.** On a bound seat the
    /// active account can be one this instance does not serve
    /// (`account-scoping.md` § Concurrent instances). `start()` once read the
    /// retired single slot (the active account's mirror), and `authenticate`
    /// re-points the client (`APIClient.adoptActor`): after a bound seat's own
    /// succession the freshly built successor client was re-pointed at the
    /// RETIRED actor whenever the mirror still held it, and every call was
    /// refused `fauna.auth.superseded`.
    private let ownSecretHex: String

    /// THIS instance's own account's session material — keyed on
    /// [ownActorIdHex], never the active pointer, for the reason [ownSecretHex]
    /// records. The views' read of the account's server-data cache (handle /
    /// domain / tier) and home nest (`account-scoping.md` § Concurrent instances
    /// → *Session identity resolves through the session's account*).
    public var sessionMaterial: FfiSessionMaterial? {
        ownActorIdHex.flatMap {
            FaunaAccounts.registry(keychain: keychain).sessionMaterial(actorId: $0)
        }
    }

    /// This instance's own retired owner `BackupKey`s off its succession
    /// chain, empty for an identity that never succeeded (`sync-agent.md`
    /// § Credential model → *Retired owner keys after an identity
    /// succession*). Resolve ONCE post-auth and share across every consumer —
    /// [makeSyncAgentProvisioner] and `conversationsSession`'s `__mls` re-seal
    /// — mirroring tui `session.rs::succession_predecessor_backup_keys`.
    ///
    /// ⚠ Resolves off [ownActorIdHex] — this instance's own actor — never
    /// [activeActorIdHex]: an append-mode sign-in or a switch racing
    /// `ConversationsVM.rebuild` can leave the two disagreeing, and a
    /// wrong-actor list makes the `__mls` re-seal silently do nothing.
    public func resolvedPredecessorBackupKeys() -> [Data] {
        guard let actor = ownActorIdHex else {
            // Only unreachable if actorIdFromSecret failed at construction —
            // which means the secret itself was malformed, so auth would
            // already have failed. Logged rather than silently swallowed so
            // a resolve failure is never indistinguishable from "no
            // predecessors".
            logMessage(level: .warn, target: "fauna.client", message:
                "resolvedPredecessorBackupKeys: no ownActorIdHex — empty fallback")
            return []
        }
        return FaunaAccounts.registry().predecessorBackupKeys(actorId: actor)
    }

    /// This instance's own **attested** predecessor actor ids off its
    /// succession chain — [resolvedPredecessorBackupKeys]'s sibling walk,
    /// empty for an identity that never succeeded
    /// (`account-data-taxonomy.md` § The generation machinery → *The source
    /// of `prior`*, ruled 2026-09-13). Feeds [makeSyncAgentProvisioner],
    /// mirroring tui `session.rs::attested_predecessors`;
    /// [startAccountRuntime] hands shared Rust the registry itself, which
    /// runs this same walk there.
    ///
    /// ⚠ Resolves off [ownActorIdHex], never [activeActorIdHex], for the
    /// same reason [resolvedPredecessorBackupKeys] does.
    public func resolvedAttestedPredecessorActorIds() -> [Data] {
        guard let actor = ownActorIdHex else {
            logMessage(level: .warn, target: "fauna.client", message:
                "resolvedAttestedPredecessorActorIds: no ownActorIdHex — empty fallback")
            return []
        }
        return FaunaAccounts.registry().attestedPredecessorActorIds(actorId: actor)
    }

    /// Everything the Media machine needs from this instance's succession
    /// chain, resolved off [ownActorIdHex] (never [activeActorIdHex], for the
    /// reason [resolvedPredecessorBackupKeys] gives): the bare retired keys, the
    /// attested ids, and the registry's one PAIRED walk
    /// (`FfiAccountRegistry.predecessorChain`) — never a zip of the two
    /// single-list exports, whose lengths can differ. Empty for an identity that
    /// never succeeded.
    public func resolvedMediaPredecessors() -> MediaPredecessors {
        guard let actor = ownActorIdHex else {
            logMessage(level: .warn, target: "fauna.client", message:
                "resolvedMediaPredecessors: no ownActorIdHex — empty fallback")
            return MediaPredecessors()
        }
        let chain = FaunaAccounts.registry().predecessorChain(actorId: actor)
        return MediaPredecessors(
            backupKeys: resolvedPredecessorBackupKeys(),
            attestedActorIds: resolvedAttestedPredecessorActorIds(),
            chainActorIds: chain.actorIds,
            chainKeys: chain.keys)
    }

    public init(
        nodeUrl: URL, secretHex: String, deviceId: String, modelContext: ModelContext,
        keychain: KeychainStore = KeychainStore()
    ) {
        self.keychain = keychain
        self.api = APIClient(nodeUrl: nodeUrl)
        self.ownSecretHex = secretHex
        // This instance's own, immutable actor id (see the property doc) —
        // and the serialized switcher's active-identity slot, both derived
        // from the same secret at the same moment they can never disagree.
        let derivedActorIdHex =
            (try? actorIdFromSecret(secret: hex_to_data(secretHex))).map { data_to_hex($0) }
        self.ownActorIdHex = derivedActorIdHex
        Self.activeActorIdHex = derivedActorIdHex
        // Prime the API secret synchronously at construction so the first
        // `ensureNestConnected()` — e.g. the `am-i-admin` nav gate firing the
        // instant this client is exposed (set_state) or restored (launch) — can
        // open the WS-RPC connection without waiting for the async
        // `authenticate()` (set_state) / silent-challenge launch path (which
        // never sets it). See `APIClient.primeSecret`.
        self.api.primeSecret(secretHex)
        self.networkMonitor = NetworkMonitor()
        self.modelContext = modelContext
        self.deviceId = deviceId

        #if os(iOS)
        self.photoBackup = PhotoBackupEngine()
        self.backgroundScheduler = BackgroundScheduler()
        self.custodianBackup = CustodianBackupEngine()
        photoBackup.configure(
            api: api,
            networkMonitor: networkMonitor,
            modelContext: modelContext,
            deviceId: deviceId
        )
        custodianBackup.configure(api: api, deviceId: deviceId)
        // The `social.fauna.sync.upload` BGProcessingTask now carries the photo
        // leg alone — its mail-segment leg went at the slice-5 flip. The task id
        // itself stays (it is shared with photo backup).
        backgroundScheduler.configure(photoBackupEngine: photoBackup)
        backgroundScheduler.configure(custodianEngine: custodianBackup)
        #endif
    }

    /// The sync engine host's state dir — the per-folder `SyncDb`s and the shared
    /// `device.db`.
    ///
    /// Production: the active account's scoped dir in the **app's own consent
    /// domain** (`SyncStateDir.resolve(actorIdHex:in:)` with
    /// `SyncStateDir.appDomain` — `on-demand-files.md` § Apple File Provider
    /// binding, *state unification* + Multi-account × File Provider
    /// consequence 3). On iOS that is the app-group container, shared with the
    /// File Provider extension. On macOS it is the **user domain**
    /// (`~/Library/Application Support/Fauna/sync/<actor>/`), the same root the
    /// external `fauna-sync-agent` scopes from its provisioned capability — so
    /// the agent-hosted sets' `file_states` are readable cross-process right
    /// here, while the File-Provider-bound sets stay in the container, where
    /// `SyncStatesStore` reads them as the container's steward.
    ///
    /// An iOS e2e launch keeps the unscoped `SyncStateDir.appSupportSyncDir`
    /// (`SyncStateDir.e2eKeepsFlatLayout` — the container is machine-global
    /// state a test launch must never touch, testing.md § point 10); a macOS
    /// e2e launch runs the production derivation inside its relocated `HOME`.
    /// Same unscoped fallback when the domain root is unreachable (a packaging
    /// bug) or no session has published its actor yet.
    /// `nonisolated` — it touches only `FileManager`, so off-main-actor callers
    /// can resolve it.
    public nonisolated static var syncStateDir: String {
        if !SyncStateDir.e2eKeepsFlatLayout,
            let actor = Self.activeActorIdHex,
            let own = SyncStateDir.resolve(actorIdHex: actor, in: SyncStateDir.appDomain)
        {
            return own.path
        }
        let dir = SyncStateDir.appSupportSyncDir
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
    }

    #if os(macOS)
        /// The container domain's scoped dir for the active account — the root
        /// `SyncStatesStore` reads the File-Provider-bound sets from as the
        /// container's steward (`SyncStateDir`'s two-domain doc). `nil` for an
        /// e2e launch (the container is machine-global, and the FP surface is
        /// e2e-gated anyway), when the container is unreachable, or before a
        /// session has published its actor.
        nonisolated static var containerStewardDir: URL? {
            guard !FaunaE2E.isActive, let actor = Self.activeActorIdHex else { return nil }
            return SyncStateDir.resolve(actorIdHex: actor, in: .container)
        }
    #endif

    /// The label this device registers under in the nest's device list — the
    /// platform constant, matching linux's `fauna-linux`
    /// (`apps/fauna-linux/src/sync.rs`), never a hostname.
    static var deviceLabel: String {
        #if os(macOS)
        "fauna-macos"
        #else
        "fauna-ios"
        #endif
    }

    /// Build the in-process sync engine host for this session and hand it to its
    /// consumers. Idempotent — the host is built once and held for the session.
    ///
    /// It starts no engine: resident engines are the external `fauna-sync-agent`'s
    /// (`sync-agent.md` § Control plane split), and each host call builds, uses
    /// and drops the engine it needs. A failure here is non-fatal: the app runs,
    /// photo ingress and restore don't, and the next `start()`/`resume()` retries.
    ///
    /// Public for the same reason as `startPushObserver`: the macOS e2e login
    /// skips `start()`, and without a host the Media page's per-file badges
    /// (`SyncStatesStore`) have nothing to read — every item would render
    /// `RemoteOnly` whatever the agent's `fsid-<ref>.db` holds.
    public func startSyncHost() async {
        guard syncHost == nil else { return }

        guard let host = await logTryAsync(.warn, "fauna.sync", "syncEngineHost", {
            try await api.syncEngineHost(
                deviceId: deviceId,
                stateDir: Self.syncStateDir,
                deviceLabel: Self.deviceLabel
            )
        }) else { return }

        syncHost = host
        #if os(macOS)
        // The two-root fold: the host reads the user domain (its own root, the
        // agent's too); the FP-bound sets are read from the container as steward.
        syncStates.attach(host: host, containerScopedDir: Self.containerStewardDir)
        #else
        syncStates.attach(host: host)
        #if os(iOS)
        photoBackup.host = host
        // Re-register the PhotoKit change observer, and catch up, when this
        // launch starts with backup ALREADY enabled.
        //
        // ⚠ Without this the foreground trigger survives exactly one app session.
        // `startObserving()` used to be reached only from the enable toggle's own
        // grant branch, so a user who turned backup on and then restarted the app
        // had no library observer at all: photos taken with the app open sat there
        // until the OS happened to schedule a background slice. That is the same
        // sentence `ui/folders.md` § Photo backup promises ("new photos go up by
        // themselves — you never have to open the app or press anything") failing
        // in the same silent way the weakly-held observer did, one layer up.
        //
        // macOS has always done this from its own shell (`FaunaMacApp`, right
        // after it hands the engine its host) — so this was per-app divergence
        // over one shared engine, resolved toward the platform that had it right
        // (priorities #1/#4). The catch-up pass mirrors that block too: it is what
        // uploads whatever arrived while the app was closed.
        if UserDefaults.standard.bool(forKey: PhotoBackupControlsView.enabledKey) {
            photoBackup.startObserving()
            _ = try? await photoBackup.syncNewPhotos()
        }
        #endif
        #endif
    }

    /// Build the sync-agent provisioner for this session (`sync-agent.md`
    /// § Control plane split, milestone A4), supplying everything the session
    /// already owns — `deviceId`, the sync device label, the retired owner keys
    /// off the account registry (empty for an identity that never succeeded —
    /// `sync-agent.md` § Credential model → *Retired owner keys after an
    /// identity succession*), and the live-bearer source off `api` — so the
    /// macOS app passes only the genuinely platform-specific piece: how to
    /// spawn/install the LaunchAgent. The caller owns the returned object's
    /// lifecycle (`start()` post-auth, `unprovision()` on every teardown path).
    public func makeSyncAgentProvisioner(
        spawner: FfiAgentSpawner,
        reachabilityObserver: FfiAgentReachabilityObserver? = nil
    ) async throws -> FfiSyncAgentProvisioner {
        let provisioner = try await api.syncAgentProvisioner(
            deviceId: deviceId,
            deviceLabel: Self.deviceLabel,
            predecessorBackupKeys: self.resolvedPredecessorBackupKeys(),
            predecessorActorIds: self.resolvedAttestedPredecessorActorIds(),
            spawner: spawner,
            bearerSource: APIClientProvisioningBearerSource(api: api),
            reachabilityObserver: reachabilityObserver
        )
        // Register it as the remote-change nudge's target. Here rather than in the
        // app so the two cannot drift: a provisioner that exists but was never
        // handed over would leave the agent on its rescan cadence alone, which is
        // invisible until a second device's save takes minutes (see the weak
        // property's note, and `startPushObserver`'s `.syncChanged` arm).
        syncAgentProvisioner = provisioner
        return provisioner
    }

    // MARK: - This device's sealed custodian store (CustodianStoreAccess)

    /// See `CustodianStoreAccess.custodianStoreFootprint` — the platform choice
    /// lives here and nowhere else, so the Backups page is one file for both
    /// shells.
    ///
    /// **A runtime choice, deliberately not `#if os(macOS)`.** Both arms have to
    /// compile on both targets: every apple compile this box runs targets the
    /// macOS *host* — `swift test`, `just mac-debug`, and even
    /// `apple-swift-build-check`'s `--target FaunaiOS` line — so an
    /// iOS-only `#else` here would be compiled by nothing until a real device
    /// build, which is exactly how `Fauna-NSE` stayed broken for four and a half
    /// months (the premise a dedicated dev-fleet iOS-typecheck gate now covers).
    ///
    /// The condition is also the *true* one, not a stand-in for the platform: a
    /// desktop hosts its replica in the external agent, so ask the agent — it
    /// alone can resolve the store's location (`sync-agent.md` § Control plane
    /// split) and promise a reclaim does not delete bytes out from under its own
    /// live pull pass. With no provisioner the app-local read is the honest
    /// answer for **both** shells: it is where iOS really keeps its store
    /// (`CustodianBackupEngine`, and nothing registers a provisioner there), and
    /// on macOS, which never writes an app-local store, it measures an absent
    /// one and answers empty — the conservative direction for a read that arms a
    /// destructive gesture.
    public func custodianStoreFootprint() async throws -> FfiCustodianStoreInfo {
        if let provisioner = syncAgentProvisioner {
            return try await provisioner.custodianStore()
        }
        return try await api.custodianStoreFootprint(dataDir: AccountStateDir.base.path)
    }

    /// See `CustodianStoreAccess.reclaimCustodianStore`. Same runtime choice, for
    /// the same two reasons — and it stays paired with the read above: whichever
    /// side answered that this device holds bytes is the side asked to free them.
    public func reclaimCustodianStore() async throws -> FfiCustodianReclaimOutcome {
        if let provisioner = syncAgentProvisioner {
            return try await provisioner.reclaimCustodianStore()
        }
        return try await api.reclaimCustodianStore(dataDir: AccountStateDir.base.path)
    }

    /// See `CustodianStoreAccess.reseedCustodianStore`. Same runtime choice, and
    /// paired with the two above for the same reason: the side that holds the
    /// bytes is the side that delivers them — the agent on a desktop, this
    /// process on a phone (`backup-destinations.md` § Re-seed → *Where the
    /// ceremony runs*).
    public func reseedCustodianStore() async throws -> FfiReseedResult {
        if let provisioner = syncAgentProvisioner {
            return try await api.reseedCustodianStore(via: provisioner, deviceId: deviceId)
        }
        return try await api.reseedCustodianStore(deviceId: deviceId, dataDir: AccountStateDir.base.path)
    }

    #if DEBUG
    /// Run **one** custodian pull pass, on whichever side this platform hosts its
    /// replica — the e2e `custodian_pull_run_now` poke (`CustodianPullTestCommand`;
    /// convention 14's causal barrier: the first production pass is 15 minutes
    /// away). Compiled out of release artifacts (convention 15): the agent arm is
    /// a `test-helpers` UniFFI call and the whole point of the method is to force
    /// a pass no user gesture can.
    ///
    /// **The same runtime pick as the two store verbs above, for the same reason**
    /// — a provisioner means the external agent hosts (macOS), none means this
    /// process does (iOS) — so the poke drives the side the *product* uses and a
    /// test cannot go green by exercising the other one. A macOS launch that never
    /// spawned its agent (no `real_sync_agent` marker) therefore falls to the
    /// in-app arm rather than failing; that store lands where the agent's never
    /// does, and the caller's on-disk assertion fails loudly on it, with
    /// `via: "in-app"` in the reply naming the cause.
    ///
    /// The in-app arm refuses a non-zero `nowOffsetSecs` by name rather than
    /// dropping it: `FfiCustodianHost.runAllKinds` reads the wall clock itself, so
    /// honouring the offset silently would let a caller believe it had crossed a
    /// 24-hour audit debounce it never did (convention 11).
    public func custodianPassForTest(nowOffsetSecs: Int64) async throws -> CustodianPassOutcome {
        if let provisioner = syncAgentProvisioner {
            return .agent(try await provisioner.custodianRunPassNow(nowOffsetSecs: nowOffsetSecs))
        }
        guard nowOffsetSecs == 0 else {
            throw CustodianPassPokeError.clockOffsetNotHonoured(nowOffsetSecs)
        }
        // The exact host `CustodianBackupEngine.runPass` builds — unscoped per-user
        // base, since `build_custodian_host` derives the actor scope itself.
        guard
            let host = try await api.custodianHost(
                deviceId: deviceId,
                dataDir: AccountStateDir.base.path,
                excluder: CloudBackupExcluder()
            )
        else { return .inApp(nil) }
        return .inApp(await host.runAllKinds())
    }
    #endif

    /// Restore a **folder** snapshot's files into `outputDir` (a user-chosen
    /// folder) via the shared client-side walk — the Backups full restore
    /// (`backup-restore.md` § 4 + § Restoring Files). It runs in-process on the
    /// sync engine host, so unlike the legacy server-side ZIP route it works on
    /// **sealed** snapshots: each file's manifest + chunks are fetched, decrypted
    /// under the owner `BackupKey`, verified, and written locally — the plaintext
    /// only ever materializes here, where the keys live.
    public func restoreSnapshot(snapshotId: Int, outputDir: String) async throws -> FfiRestoreSummary {
        guard let host = syncHost else {
            throw APIError.ffiError("Sync engine host not ready — restore is unavailable")
        }
        return try await host.restoreSnapshotToDir(snapshotId: Int64(snapshotId), outputDir: outputDir)
    }

    /// Mint this session's bearer as THIS instance's own identity — the one
    /// auth step `start()` and `resume()` share. `ownSecretHex`, never the
    /// legacy slot: see that property for the bound-seat succession it broke.
    func authenticateOwnSeat() async throws {
        try await api.authenticate(secret: ownSecretHex)
    }

    /// Full startup: authenticate, then start the long-lived observer loops
    /// (reconnect re-hydrate, connection-status, push dispatch) and the in-process
    /// byte-sync engine host. The real WS-RPC socket is opened lazily by the shared
    /// `FfiNestClient` (the observers `ensureNestConnected()` at subscribe time).
    public func start() async {
        do {
            try await authenticateOwnSeat()

            #if os(iOS)
            backgroundScheduler.registerTasks()
            backgroundScheduler.scheduleCustodianPull()
            backgroundScheduler.scheduleWidgetRefresh()
            #endif

            // Drive the reconnect re-hydrate loop (re-pull visible surfaces on
            // every WS-RPC reconnect — the feed has no poll backstop).
            startReconnectObserver()

            // Publish the live connection state to the global `connection-status`
            // indicator (top of the shell). One observer for the app lifetime,
            // like the reconnect observer (the single FfiNestClient supervisor
            // drives both watches across reconnects).
            startConnectionStateObserver()

            // Central push dispatch: re-fetch the surface each inbound push
            // touches (`fauna.notification` → notifications, `resync_required` →
            // the reconnect sweep). One observer for the app lifetime, off the
            // same long-lived FfiNestClient broker.
            startPushObserver()

            // Dedicated knock seam: `fauna.knock` is its own broker kind, not a
            // push-event variant, so the generic push observer above can't carry
            // it — a knock reaches a mounted contacts screen only through this loop
            // (mirrors windows `StartKnockPump` / android `startKnockPump`).
            startKnockObserver()

            // Nests auto-renew: desktop ticks at start and on the shared cadence;
            // iOS ticks once here (cold launch is a foreground) and again on each
            // `resume()` — the phones rule, no timer.
            #if os(iOS)
            Task { await nestsAutoRenewTick() }
            #else
            startNestsAutoRenewLoop()
            #endif

            // Mail content-sealing epoch schedule: refresh once per successful
            // connect (best-effort, log-only) so the publish paths use the
            // current epoch. A no-op when mail isn't enabled — see
            // `refreshMailEpochSchedule`. The reconnect leg lives in
            // `startReconnectObserver` (the "(re)" half of once-per-(re)connect).
            Self.refreshMailEpochSchedule(api: api)

            // The post-succession aftermath (`succession-aftermath.md` § Re-key
            // scope's `BackupKey` corpus row: "started at first successor
            // sign-in, surfaced with progress, resumed until complete"). Every
            // authenticated start, not only one that just ran a ceremony: the
            // pass is resumable and no-ops for an identity that never
            // succeeded, and a re-seal cut short by a lost connection is
            // finished by the NEXT sign-in — which has no ceremony to notice.
            // Fire-and-forget, best-effort, like the two passes below it.
            // The ceremony's raise context is parked in the account registry by
            // shared Rust and drained by the pass itself — nothing is carried
            // here.
            SuccessionAftermath.run(
                api: api,
                onConfigStageSettled: { [weak self] in
                    Task {
                        await self?.reloadMemberReviewRoster()
                        await self?.reloadInheritedFilterMarks()
                    }
                },
                onProgress: { [weak self] leg, line in
                    Task { await self?.recordAftermathProgress(leg: leg, line: line) }
                })

            // S8 D1 + D3 — the client-driven seal backfill: stamp any owned set
            // whose sealed siblings are still missing, then the snapshot
            // tag-seal stamps per owned set (D3 — snapshots are immutable, so
            // the stamp kind is that plane's only catch-up path). Once per
            // session start, best-effort (the shared `seal_backfill`
            // module every UniFFI app calls).
            Self.runSealBackfill(api: api)

            // Byte sync: build the in-process engine host, which starts a resident
            // engine per persisted folder binding and converges each one (the old
            // hardcoded `pullChanges("documents"/"photos")` pair is gone with the
            // Swift engine — set names come from the user's bindings now).
            await startSyncHost()

            // Custodian foreground push-kick, started unconditionally here for the
            // same reason `startSyncHost()` is above: `scenePhase`'s `.onChange`
            // does not fire for the phase already active at cold launch, so a
            // launch straight into the foreground would otherwise never start it
            // until the FIRST background/foreground round trip. `suspend()`
            // correctly tears it down if this launch turns out to be a
            // background-only BGProcessingTask wake.
            #if os(iOS)
            await custodianBackup.startForegroundPushKick()
            #endif
        } catch {
            logMessage(level: .error, target: "fauna.client", message: "start error: \(error)")
        }

        // Host the W3 (account-data-plane.md § Workstreams) account-store runtime (see the method). **Outside the
        // do/catch on purpose**, and it is the same independence the plane is
        // built on: the account runtime shares nothing with the observers, the
        // sync host or the mail-epoch refresh above — `memberships: None` is a
        // supported wiring, and it needs no conversations rail at all — so a
        // throw from any of them (the `authenticate` at the top most of all)
        // must not silently take the account plane down with it. Inside the
        // block, one failed bearer mint left every preference surface on the
        // blob rail with nothing on screen saying so.
        await startAccountRuntime()
    }

    /// Host the **W3 account-store runtime** in this process
    /// (`account-data-plane.md` § The account store → *The client-side
    /// lifecycle*). Best-effort like every other post-auth hook: a failure
    /// leaves every preference surface on the blob rail exactly as before this
    /// existed, and never fails a sign-in.
    ///
    /// **iOS is the target that most needs it.** `apps/sync-agent.md` § Scope
    /// per platform keeps iOS app-side "entirely (no daemons)", so an in-process
    /// host is not merely better there — it is the only host the account plane
    /// gets: no app-dead backstop, and no W5.1 peer to lose the election to. On
    /// macOS the co-located `fauna-sync-agent` hosts the same store and may hold
    /// the pump lock, so a healthy macOS app legitimately reads
    /// `runtime: true, holder: false`; hosting is still required of it, because
    /// the runtime is what the account's own preference surfaces read through.
    ///
    /// **Placed here rather than inside `conversationsSession(...)`, and that is
    /// a rule rather than taste.** `memberships: None` is a *supported* wiring —
    /// an app with no conversations rail must still host a runtime over its
    /// own-actor scopes — so the two calls need **no ordering contract**: the
    /// membership source re-reads the client's stashed session on every pump
    /// pass, so a runtime started first answers *cannot tell* until the session
    /// lands and then starts answering.
    ///
    /// **Public and idempotent** for the same reason `startReconnectObserver` is:
    /// macOS's e2e session-patch path deliberately skips the full `start()`, and
    /// a hook only `start()` fired would deny the account plane to precisely the
    /// runs that assert it. Calling it twice supersedes — the second assembly
    /// installs and the first runtime is shut down.
    ///
    /// **The three inputs, and why each is that value:**
    ///
    /// * `appDataDir` — this app's own per-user data dir, **unread** by the
    ///   assembly now (it rooted the retired device-local `__config` replica; see
    ///   `docs/goal/architecture/config-dissolution.md`). The store root is a
    ///   *sibling* of this dir under the per-user root, never a child.
    /// * `storeContainer` — `AccountStateDir.storeContainerDir`, the ONE
    ///   accessor the erases read too (its doc carries the stranding bug that
    ///   makes sharing it load-bearing), **paired with how the shell keeps it
    ///   out of iCloud device backup** (`CloudBackupExcluder`, the same
    ///   `isExcludedFromBackup` flip the custodian store uses). The pairing is
    ///   the point: the account store's writer key is a `ThisDeviceOnly`
    ///   keychain row that a restore does not carry, so a store dir that
    ///   restored without it would strand the account plane on the new device
    ///   (`common.md` § Credential storage → *The shared Rust credential slots
    ///   on the phones*). `nil` on macOS — no container, and shared Rust states
    ///   the desktop posture itself (no iCloud device backup exists there; Time
    ///   Machine is deliberately left alone).
    /// * `ownDeviceId` — this install's stable 32-byte sync device id.
    ///   ⚠ **Deliberately NOT `APIClient.indexLeaseDevice`**, which is `nil` on
    ///   iOS: that `nil` is ratified because a phone builds no content index
    ///   (`content-index.md` § Where the index is built), and that reason does
    ///   not transfer to the account plane — iOS enrolls a device row like any
    ///   other host. A wrong-length id FAILS the call by design rather than
    ///   degrading to "no seat" (the same ruling as `index_lease_device`), and
    ///   the `catch` below logs it instead of hiding it. A `nil` id fails the
    ///   start too: the id is the machine's named row, the enrollment's one
    ///   target (`sync-agent-credentials.md` § Credential model → the RULED
    ///   2026-09-28 block, decision 3).
    /// * `accounts` — the account registry itself. Shared Rust resolves this
    ///   session's own actor's **attested** succeeded-from identities off it
    ///   (never the active account): their ids are the fleet view's `prior`
    ///   (`account-data-taxonomy.md` § The generation machinery → *The
    ///   source of `prior`*, ruled 2026-09-13), and their key schedules are
    ///   what a predecessor's preference rows are carried under. Nothing to
    ///   resolve for an identity that never succeeded, which is fail-safe.
    public func startAccountRuntime() async {
        do {
            // Screen to 32 bytes rather than handing Rust whatever the session
            // happens to hold. `start_account_runtime` *fails the call* on a
            // wrong-length id (`nest_client.rs`, deliberately — so a real
            // wiring bug cannot hide as a silently-placeholder enrollment),
            // and `deviceId` here is the ACCOUNT REGISTRY's device slot, which
            // is a free-form string by contract: linux defaults it to
            // "test-device", `fauna_client_accounts`' own tests use "dev-b" /
            // "dev-legacy", and a session patch may carry anything. The
            // 32-byte-hex sync device id is a DIFFERENT store — `device.db`,
            // which linux (`sync::device_id`) and tui (`media::device_id_hex`)
            // both read for this very argument, and which apple has no reader
            // for yet.
            //
            // Unscreened, one wrong-store value takes the WHOLE W3 account
            // plane down — every preference surface back on the blob rail,
            // this account's scopes unwalked. The screen turns that into a
            // `nil`, which fails the start just as loudly — the id is the
            // machine's named row, the enrollment's one target. Same screen
            // and same reasoning as `APIClient.indexLeaseDevice`, which
            // already does this for the conversations rail (priority #4 — the
            // richest existing pattern). Logged, not silent, so the missing
            // real id stays observable rather than hiding behind the nil.
            let registryDeviceId = deviceId.isEmpty ? nil : hex_to_data(deviceId)
            let ownDeviceId = registryDeviceId?.count == 32 ? registryDeviceId : nil
            if ownDeviceId == nil && !deviceId.isEmpty {
                logMessage(
                    level: .debug, target: "fauna.accounts",
                    message: "[account-runtime] the account registry's device id is not "
                        + "32-byte hex, so this account has no named row to enroll on")
            }
            let storeContainer = AccountStateDir.storeContainerDir.map { dir in
                FfiStoreContainer(
                    dir: dir,
                    exclusion: .excludedByShell(excluder: CloudBackupExcluder()))
            }
            try await api.startAccountRuntime(
                appDataDir: AccountStateDir.base.path,
                storeContainer: storeContainer,
                ownDeviceId: ownDeviceId,
                // The registry itself: shared Rust resolves this session's
                // attested predecessors off it — the ids and the schedules
                // their preference rows are carried under.
                accounts: FaunaAccounts.registry())
        } catch {
            logMessage(
                level: .warn, target: "fauna.accounts",
                message: "[account-runtime] start failed; preference surfaces stay on the "
                    + "blob rail and this account's own scopes go unwalked: \(error)")
        }
    }

    // EXCISED BY `FAUNA_EXCISE_P2P_SHARE`: the store-safe FFI exports no share plane
    // (the reason is written once, at the top of `SharePlaneModel.swift`).
    #if !FAUNA_EXCISE_P2P_SHARE

    /// Start the **cross-user share plane** for this session (`p2p.md` § Cross-user
    /// shared-set transfer → *Implementation status today*, the FFI share-plane
    /// host). Best-effort like [startAccountRuntime]: a failure leaves the
    /// peer-transfer surface absent and never fails a sign-in.
    ///
    /// **Called from where the sync-agent provisioner starts, not from [start].** The
    /// plane's third input is the app's own provisioner (its two agent verbs ride
    /// it), which the app shell builds outside `start()` — and macOS's e2e
    /// session-patch path skips `start()` entirely, so a hook only `start()` fired
    /// would deny the plane to precisely the runs that assert it. Ordering against
    /// the account runtime and the conversations session is NOT the caller's problem:
    /// both land on their own tasks after the calls that start them return, and shared
    /// Rust waits for them (`libs/fauna-ffi/src/share_plane.rs`).
    ///
    /// - The owner secret is THIS instance's own ([ownSecretHex]) — never the active
    ///   account's, for the reason that property records.
    /// - `spoolDir`: app-private scratch for in-flight peer downloads, beside the
    ///   account's own state (tui and linux use `<config>/share-spool`).
    public func startSharePlane(provisioner: FfiSyncAgentProvisioner) async {
        await SharePlaneModel.shared.start(
            api: api, ownerSecretHex: ownSecretHex, provisioner: provisioner,
            spoolDir: AccountStateDir.base.appendingPathComponent("share-spool", isDirectory: true))
    }

    #endif

    /// Open one long-lived subscription on the shared `FfiNestClient` and post
    /// `.faunaReconnected` on each reconnect-after-first, so live surfaces
    /// (`onReconnect`) re-pull their snapshot. The single `FfiNestClient`'s own
    /// supervisor bumps the watch on every reconnect (including iOS
    /// foreground-after-background), so one observer suffices for the app's
    /// lifetime — no per-`resume()` re-spawn. Mirrors linux `WsEvent::Reconnected`
    /// (`apps/fauna-linux/src/app.rs`). Captures `api` directly (not `self`) so the
    /// task never retains `FaunaClient`.
    ///
    /// Public (and idempotent — cancels any prior task) for the same reason as
    /// `startConnectionStateObserver`/`startPushObserver`/`startKnockObserver`:
    /// the macOS in-process e2e agent skips the full `start()`, so
    /// `test_nest_flip_feed_rehydrate`'s reconnect-triggered feed re-hydrate
    /// needs this wired up explicitly here (the iOS agent calls
    /// `start()`, which already invokes it — this method was the one sibling
    /// observer left un-promoted, so only macOS's e2e path was missing the
    /// mechanism; production login always goes through `start()`).
    public func startReconnectObserver() {
        reconnectObserverTask?.cancel()
        let api = self.api
        reconnectObserverTask = Task {
            guard let sub = await logTryAsync(.warn, "fauna.client", "subscribeReconnects", { try await api.subscribeReconnects() }) else { return }
            while !Task.isCancelled {
                if await sub.next() == nil { break }  // nil ⇒ client torn down
                NotificationCenter.default.post(name: .faunaReconnected, object: nil)
                // Refresh the mail epoch schedule on each reconnect too
                // (best-effort, log-only) — the "(re)" half of
                // once-per-(re)connect; the initial-connect leg fires in `start()`.
                Self.refreshMailEpochSchedule(api: api)
            }
        }
    }

    /// One Nests auto-renew tick (`nests.md` § Expiry / renewal → *Duration and
    /// blessing*): renews each blessed standing grant inside the renew-ahead
    /// threshold, silently and best-effort. Runs over a page-less machine, so it
    /// fires whichever page the user sits on.
    public func nestsAutoRenewTick() async {
        await nestsAutoRenewVM.autoRenewTick(api: api)
    }

    /// The desktop auto-renew loop: a tick at start, then one every shared
    /// `autoRenewCheckSecs()` (the one cadence constant — never re-spelled).
    /// Idempotent — a second call supersedes the first. iOS runs no timer: the
    /// phones rule is app foreground only (`start()` and `resume()` tick once).
    public func startNestsAutoRenewLoop() {
        nestsAutoRenewTask?.cancel()
        nestsAutoRenewTask = Task { [weak self] in
            let interval = Duration.seconds(Int64(autoRenewCheckSecs()))
            while !Task.isCancelled {
                await self?.nestsAutoRenewTick()
                try? await Task.sleep(for: interval)
            }
        }
    }

    /// Best-effort, log-only refresh of the mail content-sealing **epoch
    /// schedule**, fired once per successful (re)connect — the macOS/iOS leg of
    /// `encryption-at-rest.md` § Capability tiering → Content-sealing epochs
    /// (spec `2026-07-18-mail-content-sealing-epochs-design.md` § 3). The shared,
    /// idempotent `MailSettingsMachine.refreshEpochSchedule()` is a no-op when
    /// mail isn't enabled, so it is always safe to call unconditionally —
    /// fire-and-forget (linux
    /// `FaunaClient::refresh_mail_epoch_schedule`, wired in the `AuthSuccess` arm;
    /// android
    /// `MailEnableGlueVM.refreshMailEpochSchedule`; web's one-shot-per-session
    /// `$effect`). Static + `api`-only so the reconnect observer (which captures
    /// `api`, never `self`) can call it without retaining `FaunaClient`.
    static func refreshMailEpochSchedule(api: APIClient) {
        Task {
            guard let machine = try? await api.mailSettingsMachine() else { return }
            _ = await logTryAsync(.warn, "fauna.client", "refreshEpochSchedule") {
                try await machine.refreshEpochSchedule()
            }
        }
    }

    /// The S8 D1 + D3 client-driven seal backfill (file-sync.md § Sealed names
    /// & paths → Implementation status today). Fire-and-forget, best-effort —
    /// mirrors `refreshMailEpochSchedule`'s shape. The D1-then-D3-skip-member
    /// sequencing lives once in the shared `seal_backfill` sweep
    /// (`APIClient.runSealBackfillSweep`), which every UniFFI app now calls
    /// instead of hand-rolling the loop; this glue only logs — never throws,
    /// the sweep is best-effort by contract (android's
    /// `MailEnableGlueVM.runSealBackfill` is the reference noteworthy-only shape).
    static func runSealBackfill(api: APIClient) {
        Task {
            guard let report = await api.runSealBackfillSweep() else { return }
            let noteworthy = report.fieldsError != nil
                || report.rosterError != nil
                || report.setFailures > 0
                || report.tags.stampFailures > 0
                || (report.fields.map { $0.names > 0 || $0.updateFailures > 0 || $0.identityMismatch > 0 } ?? false)
            if noteworthy {
                logMessage(level: .warn, target: "fauna.client", message:
                    "sealBackfillSweep: fieldsError=\(report.fieldsError ?? "nil") "
                    + "rosterError=\(report.rosterError ?? "nil") setFailures=\(report.setFailures) "
                    + "tagStampFailures=\(report.tags.stampFailures)")
            }
        }
    }

    /// Refresh the `am-i-admin` nav gate (admin.md § Navigation model; mac/iOS,
    /// priority #2 — was duplicated verbatim in each platform's own
    /// `refreshAdminStatus`, which then assigned the result to its own
    /// `isAdmin` — `MacAppState` and FaunaKit's shared `AppState` are distinct
    /// types, so the assignment itself stays per-caller). Fail-closed: any
    /// error ⇒ not admin, so a failed probe hides the admin entry rather than
    /// leaking it (the web bug the gating tests guard against).
    public static func refreshAdminStatus(client: FaunaClient?) async -> Bool {
        guard let client else {
            logMessage(level: .info, target: "fauna.app", message: "[admin-gate] no client → isAdmin=false")
            return false
        }
        let isAdmin = (try? await client.api.amIAdmin()) ?? false
        logMessage(level: .info, target: "fauna.app", message: "[admin-gate] am_i_admin=\(isAdmin)")
        if isAdmin {
            // The admin auto-default (long-term-store.md § Multi-account evolution):
            // an admin identity gets require-confirm-to-activate ON unless the user
            // ever touched its toggle.
            FaunaAccounts.autoEnableRequireConfirmForActiveAdmin()
        }
        return isAdmin
    }

    /// Read the app-global unread notification count (`notifications.md`
    /// § Architectural rules, rule 4: the count lives on a shell-lifetime holder
    /// that the push arm refreshes, never only on the Notifications page's
    /// view-model, which exists solely while that page is mounted). Both shells
    /// call this from their root on session start, on reconnect and on every
    /// `fauna.notification` push, and assign the result to their own
    /// `notificationsUnreadCount` (`MacAppState` and `AppState` are distinct
    /// types — the `refreshAdminStatus` split). No client ⇒ 0; a failed read ⇒
    /// nil, so the caller keeps the last known count rather than zeroing it.
    public static func fetchUnreadNotificationCount(client: FaunaClient?) async -> Int? {
        guard let client else { return 0 }
        return await logTryAsync(.warn, "fauna.notifications", "unreadNotificationCount") {
            try await client.api.unreadNotificationCount()
        }
    }

    /// Open one long-lived subscription on the shared `FfiNestClient` for **all**
    /// inbound pushes and dispatch each to the surface it touches — the apple twin
    /// of linux's `app.rs` `WsEvent::Push(e) => match e { … }` and android's
    /// `ApiClient.startPushPump` (`transport.md` § Push events and `seq`
    /// numbering). One observer for the app's lifetime: the single `FfiNestClient`
    /// supervisor drives the push broker across reconnects (including iOS
    /// foreground-after-background), so no per-`resume()` re-spawn — exactly like
    /// `startReconnectObserver`. Captures `api` directly (not `self`) so the task
    /// never retains `FaunaClient`. Single-consumer, so this one task serializes
    /// `next()`; every arm stays cheap and non-blocking (each just posts a
    /// NotificationCenter signal a live surface re-fetches on).
    ///
    /// Which surfaces are stale is derived from `staleSurfacesForPushEvent`
    /// (`fauna_protocol::StaleSurfaces::for_kind`, `transport.md` § Which surfaces
    /// a push invalidates) rather than a hand-matched table per event — this file
    /// no longer carries its own copy of the kind→surface mapping, so a kind the
    /// shared seam later grows a new surface for reaches apple with no edit here
    /// (mirrors android's `ApiClient.startPushPump` adoption). `SyncChanged` is
    /// matched separately alongside the classifier because its folder-specific
    /// consumers (the File Provider relay, the sync-agent pull, the per-row
    /// device-activity section) need the folder name the flattened booleans don't
    /// carry — the classifier still decides whether the cross-set Media signal
    /// fires.
    ///
    /// Public (and idempotent — cancels any prior task) for the same reason as
    /// `startConnectionStateObserver`: the macOS in-process e2e agent skips the full
    /// `start()`, so it wires this up explicitly (the iOS agent calls `start()`,
    /// which invokes it). `test_push_live_refresh.py` depends on it.
    public func startPushObserver() {
        pushObserverTask?.cancel()
        let api = self.api
        pushObserverTask = Task {
            guard let sub = await logTryAsync(.warn, "fauna.client", "subscribePushes", { try await api.subscribePushes() }) else { return }
            while !Task.isCancelled {
                guard let event = await sub.next() else { break }  // nil ⇒ client torn down
                let stale = staleSurfacesForPushEvent(event: event)
                if stale.notifications {
                    // Mirrors linux `fetch_notifications()` / android `notificationTick`:
                    // re-fetch the notifications surface so a mounted page grows the row
                    // live (what the cross-app e2e asserts). The OS toast is a
                    // follow-on leg (NotificationManager).
                    NotificationCenter.default.post(name: .faunaNotificationReceived, object: nil)
                }
                if stale.events {
                    // A durable write landed in one of this actor's calendars (own
                    // other device, or an external MUA via the MDA). The Events
                    // surface's quick-appearance poll (`EventsVM.pollWhileVisible`,
                    // 2026-07-15) already covers this as a backstop; consuming the
                    // push cuts that latency down to push latency (transport.md
                    // § Push events; mirrors android's `calendarChangedTick`).
                    NotificationCenter.default.post(name: .faunaCalendarChanged, object: nil)
                }
                if stale.addressBook {
                    // A durable write landed in one of this actor's CardDAV address
                    // books (own other device, or an external MUA via the MDA). The
                    // Address Book segment re-reads the book list and the open
                    // book's cards while it is on screen (transport.md § Push
                    // events; mirrors android's `addressBookChangedTick`).
                    NotificationCenter.default.post(name: .faunaAddressBookChanged, object: nil)
                }
                if case .syncChanged(let folder, let folderHash) = event {
                    // A record landed in a folder this actor participates in
                    // (file-sync.md § Remote-change nudge). It has TWO possible
                    // consumers on apple and they are not alternatives — whichever
                    // is absent simply no-ops, so both are signalled unconditionally
                    // rather than branched on.
                    //
                    // 1. The File Provider extension, for a domain-bound set
                    // (`FileProviderCoordinator.signalChanged` → `enumerateChanges`
                    // in `Fauna-FileProvider/FileProviderEnumerator.swift`, the
                    // only externally-reachable hook into that separate process).
                    #if canImport(FileProvider)
                        Task {
                            // The push names the set by its name or name hash;
                            // the coordinator matches it to a held set through
                            // the shared rule and signals that set's domain by
                            // its ref — never by display name.
                            try? await FileProviderCoordinator.signalChanged(
                                folder: folder, folderHash: folderHash)
                        }
                    #endif
                    // 2. The resident `fauna-sync-agent`, for a folder-bound set on
                    // macOS. **The agent holds no WS connection of its own**, so
                    // this relay is its ONLY out-of-cadence delivery path — exactly
                    // the gap that made the windows seat pair red (
                    // the push fell out of the switch and the only route left was
                    // the rescan tick, against a far shorter convergence window).
                    // Apple had the same hole one platform over: the arm existed
                    // but signalled only consumer 1, so a folder-bound macOS app
                    // uploaded correctly and never applied a peer's change.
                    //
                    // "Best-effort" here means LATENCY, not optional (the FFI
                    // export's own words): a set with no resident engine, or a pull
                    // already pending, is a silent no-op agent-side — but without
                    // the call a second device's save waits out the whole rescan
                    // cadence. Mirrors linux `sync_agent::pull_set_now`, tui
                    // `SyncAgentState::pull_set_now` and windows
                    // `TryPullFolderNowAsync` (priorities #1/#3).
                    if let provisioner = syncAgentProvisioner {
                        Task {
                            // `folderHash` relayed as received: the agent matches
                            // its bindings by it (a sealed set's nudge names no
                            // plaintext — path-sealing.md § the set-name plane).
                            try? await provisioner.pullFolderNow(folder: folder, folderHash: folderHash)
                        }
                    }
                    // 3. The Folders page's per-set device-activity section, for
                    // whichever set is CURRENTLY EXPANDED (`FolderDeviceActivitySection`
                    // in `FoldersContent.swift`) — carries the folder name so the
                    // observer can gate on it naming the expanded row, never a
                    // blanket refetch (mirrors tui/linux/windows' analogous guard).
                    NotificationCenter.default.post(name: .faunaFolderDeviceActivityChanged, object: folder)
                }
                if stale.media {
                    // The Media page's cross-set all-media aggregate, while Media
                    // is the page on screen — `fauna.media.list` spans every set, so
                    // unlike the per-folder signal above there is no name to gate
                    // on; the page-visibility gate lives entirely in `onMediaChanged`
                    // (SwiftUI only delivers `.onReceive` to a mounted view), never
                    // here (media.md § Staying live while the page is open).
                    NotificationCenter.default.post(name: .faunaMediaChanged, object: nil)
                }
                if stale.knocks || stale.contacts || stale.account || stale.atproto {
                    // Surfaces with no dedicated push signal on apple today: knocks
                    // rides its own `subscribeKnocks()` broker (`startKnockObserver`),
                    // never acted on here even when a `fauna.knock` arrives as
                    // `.other` on this generic stream too; contacts/account have no
                    // push-specific signal and fall back to the reconnect sweep
                    // (`ContactSplitView`'s own `.onReconnect`; AccountUpdated re-pulls
                    // via the Settings screen's own load); bluesky has no reconnect
                    // observer wired at all yet — `AtprotoSettingsView` loads once on
                    // mount, a pre-existing absence this adoption surfaces but does
                    // not fix (mirrors android's documented `AtprotoVM` gap). Reusing
                    // `.faunaReconnected` here — rather than a fifth dedicated signal —
                    // mirrors android's `reconnectTick` reuse for the same bucket.
                    NotificationCenter.default.post(name: .faunaReconnected, object: nil)
                }
            }
        }
    }

    /// Open one long-lived subscription on the shared `FfiNestClient` for inbound
    /// `fauna.knock` pushes and broadcast `.faunaKnockReceived` on each — the mounted
    /// contacts surface (`onKnockReceived`) re-fetches its roster live, so a contact
    /// request arriving while the user is already on the contacts screen appears with
    /// no navigation. `fauna.knock` is a **dedicated** broker kind, not an
    /// `FfiPushEvent` variant, so `startPushObserver` cannot carry it (a knock arrives
    /// there as `Other` and is ignored); this is the apple twin of windows
    /// `NestRpcClient.StartKnockPump`/`KnockReceived` and android `startKnockPump`
    /// (`transport.md` § Push events). One observer for the app's lifetime — the single
    /// `FfiNestClient` supervisor drives the broker across reconnects (including iOS
    /// foreground-after-background), so no per-`resume()` re-spawn, exactly like
    /// `startReconnectObserver`. Captures `api` and `notificationManager` directly
    /// (not `self`) so the task never retains `FaunaClient`. Each `next()` also
    /// raises the OS knock toast (`NotificationManager.postKnockNotification`,
    /// `behavior/notifications.md` § The knock toast) before the roster-refresh
    /// signal — like windows, the pump triggers a full re-fetch, not a counter bump.
    ///
    /// Public (and idempotent — cancels any prior task) for the same reason as
    /// `startPushObserver`: the macOS in-process e2e agent skips the full `start()`, so
    /// it wires this up explicitly (the iOS agent calls `start()`, which invokes it).
    /// `test_knock_live_refresh.py` depends on it.
    public func startKnockObserver() {
        knockObserverTask?.cancel()
        let api = self.api
        let notificationManager = self.notificationManager
        knockObserverTask = Task {
            guard let sub = await logTryAsync(.warn, "fauna.client", "subscribeKnocks", { try await api.subscribeKnocks() }) else { return }
            while !Task.isCancelled {
                guard let knock = await sub.next() else { break }  // nil ⇒ client torn down
                notificationManager.postKnockNotification(knock)
                NotificationCenter.default.post(name: .faunaKnockReceived, object: nil)
            }
        }
    }

    /// Open one long-lived subscription on the shared `FfiNestClient` and publish
    /// the live transport `ConnectionState` to `connectionState`, driving the
    /// global `connection-status` indicator (top of the shell). The FIRST `next()`
    /// resolves immediately with the *current* state (so the indicator seeds on
    /// subscribe, not on the next transition), then each transition; `nil` once
    /// the client tears down. One observer suffices for the app's lifetime — the
    /// single `FfiNestClient`'s supervisor drives the watch across reconnects
    /// (including iOS foreground-after-background), exactly like
    /// `startReconnectObserver`. Captures `api` directly (mirrors the reconnect
    /// observer); the `[weak self]` write avoids retaining `FaunaClient`. Apple
    /// twin of android's `ApiClient.startConnectionStatePump` / web's
    /// `connectionStatus` store.
    ///
    /// Public (and idempotent — cancels any prior task) so the macOS in-process
    /// e2e agent can drive the indicator without running the full `start()`: that
    /// agent skips `start()`'s heavy WebSocket/sync/backup work on the MainActor
    /// and authenticates only, but this observer is lightweight (one subscription
    /// loop over the FfiNestClient the agent's reads open lazily anyway), so the
    /// `connection-status` indicator must be wired up explicitly there. The iOS
    /// agent calls `start()`, which invokes this for it.
    public func startConnectionStateObserver() {
        connectionStateObserverTask?.cancel()
        let api = self.api
        connectionStateObserverTask = Task { [weak self] in
            guard let sub = await logTryAsync(.warn, "fauna.client", "subscribeConnectionState", { try await api.subscribeConnectionState() }) else { return }
            while !Task.isCancelled {
                guard let state = await sub.next() else { break }  // nil ⇒ client torn down
                self?.connectionState = state
                // Every value, repeats included — the `connection_reports`
                // stickiness counter (a no-op outside the test flavor).
                E2eLoudSurfaces.observeConnectionReport(state)
                // A `.disconnected` is also where a supervisor that stopped for
                // good says why (the stop is recorded before the state is
                // announced). A session-ending verdict — a re-mint refused as
                // superseded, suspended, or a changed nest identity — goes to
                // the app root, which routes it to the launch surface
                // (`escalateSessionEnding`); the session ends here. tui's and
                // linux's connection pumps are the model.
                if state == .disconnected, let verdict = api.sessionEndingVerdict() {
                    NotificationCenter.default.post(
                        name: .faunaSessionEnding, object: self,
                        userInfo: [FaunaSessionEnding.verdictKey: verdict])
                    break
                }
            }
        }
    }

    /// Tear this session down for good — the counterpart to ``start()``.
    ///
    /// **This is not ``suspend()``.** `suspend()` is iOS-only and deliberately *keeps*
    /// the sync host (the background one-shot pass still needs it) and the observer
    /// loops; it parks a session that is coming back. `shutdown()` is for a session that
    /// is not: the **account switch** (`long-term-store.md` § Multi-account evolution —
    /// switching is `set_active` + a client teardown/rebuild) and any other point where
    /// this client stops being the app's client.
    ///
    /// Everything here is something that otherwise **outlives the client object**, which
    /// is why dropping the reference is not enough. The three observer tasks are cancelled
    /// only on a re-`start()`, and they capture `api` *strongly* — so an un-shut-down
    /// client keeps live WS-RPC subscriptions posting `.faunaReconnected`,
    /// `.faunaNotificationReceived`, and connection-state for the identity the user just
    /// left, on top of the incoming session's own. The resident sync engines are worker threads inside the Rust host,
    /// not Swift objects. The upload driver's `run_forever` loop is a detached Rust task.
    /// None of those are reachable by ARC.
    ///
    /// Order matters: stop the loops that could observe a half-torn-down session before
    /// tearing it down.
    ///
    /// **`async` for step 4 alone, and that await is load-bearing.** Everything else here
    /// is a cancel or a drop; the account runtime is a *process global* whose teardown
    /// advances an install generation (`libs/fauna-ffi/src/account_runtime.rs`'s
    /// `HOST.take()`, which bumps even on an empty slot). Spawned instead of awaited, it
    /// could land after the *next* login's `begin()` and supersede the FRESH runtime —
    /// which nothing retries, so that account silently runs on the blob rail for the rest
    /// of the process. Every caller of `shutdown()` is a sign-out / switch / factory-reset
    /// already reachable from an async context, so awaiting costs nothing and removes the
    /// race outright. (The same reasoning `FileProviderCoordinator.signOut()` is awaited
    /// before `runLaunch()` under: an async teardown that must COMPLETE before the
    /// incoming session starts.)
    @MainActor
    public func shutdown() async {
        // 1. Observer loops first — they are the ones that would otherwise keep firing
        //    at the app for a session that no longer exists.
        reconnectObserverTask?.cancel()
        reconnectObserverTask = nil
        connectionStateObserverTask?.cancel()
        connectionStateObserverTask = nil
        pushObserverTask?.cancel()
        pushObserverTask = nil
        knockObserverTask?.cancel()
        knockObserverTask = nil
        nestsAutoRenewTask?.cancel()
        nestsAutoRenewTask = nil

        // 2. Sync: drop the host. It holds no resident engine (construct-run-drop
        //    only — `startSyncHost()`), so there is nothing to stop first.
        syncStates.detach()
        syncHost = nil

        // 3. Backup drivers. Only the photo leg and the custodian's foreground
        //    push-kick remain — the segment-backup upload driver went at the
        //    slice-5 flip (the nest is the writer). The push-kick loop is the
        //    same "cancel, don't just drop the reference" case as the observer
        //    loops above: nothing else would notice the session tore down and
        //    stop it, so a leftover loop keeps pushing for the identity that
        //    just left.
        #if os(iOS)
        custodianBackup.stopForegroundPushKick()
        photoBackup.host = nil
        #endif

        // 4. The W3 account runtime — a PROCESS-global this client installed, so
        //    dropping the reference stops nothing (`account-data-plane.md` § The
        //    account store → *The client-side lifecycle*). Awaits the pump's
        //    in-flight pass, so by the time this returns the outgoing account has
        //    stopped writing — which is what keeps a mid-pass sign-out from
        //    leaving the outbox half-drained. Idempotent and safe with no runtime
        //    installed, so a pre-auth teardown costs nothing.
        //
        //    ⚠ A plain **quit** must not reach here, and does not: `shutdown()`
        //    is for a session that is not coming back, while a quit ends the
        //    process (the handle drops, and on macOS the co-located agent takes
        //    the pump role over). iOS's background path is `suspend()`, which
        //    deliberately keeps this running.
        await api.stopAccountRuntime()
    }

    /// Pause when app goes to background (iOS only — macOS never backgrounds).
    public func suspend() {
        #if os(iOS)
        backgroundScheduler.scheduleUpload()
        backgroundScheduler.scheduleCustodianPull()
        backgroundScheduler.scheduleWidgetRefresh()
        // The custodian's foreground push-debounce loop has no periodic tick —
        // nothing else would ever notice the app backgrounded and wake it to
        // stop, so cancelling here is required, not hygiene (mirrors android's
        // `CustodianPushKick.onStop`). The periodic `BGProcessingTask` above
        // is what carries the custodian pass while backgrounded.
        custodianBackup.stopForegroundPushKick()
        #endif
    }

    /// Resume when app comes to foreground (iOS only).
    public func resume() async {
        #if os(iOS)
        do {
            try await authenticateOwnSeat()
            // The real WS-RPC socket reconnects through the shared `FfiNestClient`
            // supervisor (which the reconnect/push/connection-state observers ride
            // across foreground-after-background); no per-resume socket re-open here.
            // A host that failed to build at `start()` gets its retry here
            // (`startSyncHost()` early-returns once one exists).
            await startSyncHost()
            // Low-latency foreground wake for the custodian, the sibling of the
            // periodic BGProcessingTask (mirrors android's
            // `CustodianPushKick.onStart`). Idempotent — a redundant call while
            // already holding a handle is a no-op.
            await custodianBackup.startForegroundPushKick()
            // The auto-renew tick at foreground (phones: no timer).
            await nestsAutoRenewTick()
        } catch {
            logMessage(level: .error, target: "fauna.client", message: "resume error: \(error)")
        }
        #endif
    }

    // `segmentBackupDataDir` went with the in-app upload driver at the slice-5
    // flip 2026-08-15: nothing apple-side opens a segment-backup state DB any
    // more (the source nest is the writer), and the Backups page's
    // per-destination status comes from the nest's `fauna.backup.status`
    // projection rather than a local coordinator handle.

    /// The active actor's backup-audit state file (`ui/backups.md` § Audit-alert
    /// surface) — the `state_path` `backupAuditRunPass`/`backupAuditObserve` take.
    /// Actor-scoped because apple is a
    /// serialized account switcher (switch = full teardown + relaunch), so this
    /// resolves off the same `activeActorIdHex` written once per construction.
    static var backupAuditStatePath: String {
        AccountStateDir.backupAuditStatePath(actorIdHex: Self.activeActorIdHex)
    }

    // The legacy second WebSocket (`/api/v1/ws/{actor}?token=`) and its JSON
    // `handleIncomingPayload` dispatch were removed 2026-07-17: the nest 401s that
    // query-auth handshake (it now requires the `fauna.v1, bearer.<token>`
    // subprotocol), so the socket never opened and its JSON parse could never match
    // the real CBOR `PushEvent`s — the same dead-second-socket web deleted
    // (`$lib/ws.ts`). Real pushes now ride the one authenticated `FfiNestClient`
    // socket via `startPushObserver`; MLS receive rides the conversations rail's own
    // `subscribe_kind` inside shared Rust (`ConversationsVM`), not this path.
}

public extension Notification.Name {
    /// WS-RPC client reconnected (after the first connect). Live surfaces observe
    /// this via the `onReconnect` modifier and re-pull their snapshot.
    static let faunaReconnected = Notification.Name("faunaReconnected")
    /// The WS-RPC supervisor stopped on a session-ending verdict (posted once by
    /// `FaunaClient`'s connection-state observer, `object` the client, the
    /// verdict under ``FaunaSessionEnding/verdictKey``). The app root observes it
    /// via `onSessionEnding` and routes it through `escalateSessionEnding`.
    static let faunaSessionEnding = Notification.Name("faunaSessionEnding")
    /// A `fauna.notification` push arrived on the authenticated socket (posted by
    /// `FaunaClient`'s push observer). The notifications surface observes this via
    /// `onPushNotification` to re-pull live — the apple twin of android's
    /// `notificationTick` (`transport.md` § Push events).
    static let faunaNotificationReceived = Notification.Name("faunaNotificationReceived")
    /// A `fauna.knock` (contact-request) push arrived on the authenticated socket
    /// (posted by `FaunaClient`'s dedicated knock observer). The contacts surface
    /// observes this via `onKnockReceived` to re-pull its roster live — the apple twin
    /// of windows' `KnockReceived` event / android's `knockTick` (`transport.md`
    /// § Push events). `fauna.knock` is a dedicated broker kind, distinct from the
    /// generic `fauna.notification` above.
    static let faunaKnockReceived = Notification.Name("faunaKnockReceived")
    /// A `fauna.calendar.changed` push arrived (posted by `FaunaClient`'s push
    /// observer) — a durable write landed in one of this actor's calendars (own
    /// other device, or an external MUA via the MDA). The Events surface observes
    /// this via `onCalendarChanged` to re-pull live, cutting the quick-appearance
    /// poll's latency down to push latency — the apple twin of android's
    /// `calendarChangedTick` (`transport.md` § Push events).
    static let faunaCalendarChanged = Notification.Name("faunaCalendarChanged")
    /// A `fauna.addressbook.changed` push arrived (posted by `FaunaClient`'s push
    /// observer) — a durable write landed in one of this actor's CardDAV address
    /// books (own other device, or an external MUA via the MDA). The Address
    /// Book segment observes this via `onAddressBookChanged` to re-read the book
    /// list and the open book's cards live — the apple twin of android's
    /// `addressBookChangedTick` (`transport.md` § Push events).
    static let faunaAddressBookChanged = Notification.Name("faunaAddressBookChanged")
    /// A `fauna.sync.changed` push arrived for a folder this actor participates in
    /// (posted by `FaunaClient`'s push observer, carrying the folder name as
    /// `object`). The Folders page's per-set device-activity section observes this
    /// via `onFolderDeviceActivityChanged`, gating on the pushed name matching its
    /// own (currently-expanded) set — never a blanket refetch for a collapsed row
    /// nobody's looking at (file-sync.md § Implementation status today; the apple
    /// twin of tui's `device_activity_resync_op` / windows' `_expandedFolder ==
    /// folder` guard). Unlike the other push signals here this one carries a
    /// payload — the first case needing to know WHICH entity changed, not just
    /// that something did.
    static let faunaFolderDeviceActivityChanged = Notification.Name("faunaFolderDeviceActivityChanged")
    /// A `fauna.sync.changed` push arrived for a folder this actor participates in
    /// (posted by `FaunaClient`'s push observer, same event as
    /// `faunaFolderDeviceActivityChanged` above but with no payload — Media's read
    /// is a cross-*set* aggregate, so which folder changed doesn't matter). The
    /// Media page observes this via `onMediaChanged`, gated on the page being on
    /// screen (SwiftUI only delivers `.onReceive` to a mounted view) — never a
    /// blanket refetch for the thousands of nudges an initial folder sync fires at
    /// a page nobody is looking at (media.md § Staying live while the page is
    /// open; the apple twin of linux's `media_page_is_visible` guard / tui's
    /// `self.page == Page::Media`).
    static let faunaMediaChanged = Notification.Name("faunaMediaChanged")
    /// The account registry was mutated by something OTHER than the switcher's own
    /// view model — today the admin auto-default
    /// (`FaunaAccounts.autoEnableRequireConfirmForActiveAdmin`), which writes
    /// `require_confirm_to_activate` straight through `FaunaAccounts.registry()` at
    /// an `am-i-admin` observation. `AccountSwitcherVM` caches `accounts` and only
    /// reloads on appear / after its own writes, so without this signal a switcher
    /// already on screen keeps rendering the pre-write row: the toggle shows OFF
    /// while the store says ON, and the next tap computes its new value from that
    /// stale entry and writes the value already there — the user's tap is swallowed.
    /// Observed via the `onAccountRegistryChanged` modifier.
    static let faunaAccountRegistryChanged = Notification.Name("faunaAccountRegistryChanged")
}

/// The session object IS this device's custodian-store access — the conformance
/// is declared apart from the two methods only because they need the private
/// `syncAgentProvisioner` above (`CustodianStoreAccess.swift` carries the why).
extension FaunaClient: CustodianStoreAccess {}
