import Foundation
import FaunaKit

/// One folder ↔ folder binding row as the Folders page renders it — the
/// projection of the shared `FfiLocationBindingsModel`'s rows, whose truth is the
/// external `fauna-sync-agent`'s own config (`sync-agent.md` § Control plane split).
/// `docs/goal/behavior/on-demand-files.md` § Hosting multiple on-demand folders
/// (one folder ↔ one named folder).
///
/// `folderId` is the set's `FolderRef` wire string — the binding's key; `folder`
/// is its label (a name is unique only per owner). The pre-cutover
/// `location-map.json` this type used to decode, and the migration seed read from
/// it, were retired 2026-09-24 with the name-keyed binding (the compat-remnant
/// sweep).
struct LocationBinding: Identifiable, Hashable {
    /// Local filesystem path this device keeps in sync.
    var path: String
    /// Nest folder name this folder maps to — a label.
    var folder: String
    /// The set's `FolderRef` wire string (`FolderSummary.folderRef`).
    var folderId: String

    var id: String { path }
}

/// The subset of the sync-agent control channel `LocationsModel` drives — the
/// device-local folder↔folder binding verbs (`sync-agent.md` § Control plane
/// split). `FfiSyncAgentProvisioner` conforms directly (its `bind_location` /
/// `unbind_location` / `list_locations` UniFFI exports); unit tests substitute an
/// in-memory fake — the macOS peer of windows' `InMemorySyncPipeClient` — since
/// the e2e `sync_inject_locations` seam bypasses the reconcile path entirely.
protocol LocationControlChannel: AnyObject, Sendable {
    func bindLocation(path: String, folder: String, folderId: String) async throws
    func unbindLocation(path: String) async throws
    func listLocations() async throws -> [FfiAgentLocation]
    func applyHeldDeletes(folder: String) async throws -> FfiHeldDeletesApplied
}

extension FfiSyncAgentProvisioner: LocationControlChannel {}

/// Bridges the shared convergence loop's **agent-reachable rising edge**
/// (`FfiAgentReachabilityObserver`, fired from the loop's tokio task) to
/// `LocationsModel`. Hops to the main actor to re-drive the model's reconcile
/// the moment the just-spawned agent first answers — closing the
/// launch race where the single at-attach reconcile beats the
/// agent's socket coming up (`sync-agent.md` § Control plane split; review
/// 2026-07-19).
final class SyncAgentReachabilityObserver: FfiAgentReachabilityObserver {
    private let model: LocationsModel
    init(model: LocationsModel) { self.model = model }
    func onAgentReachable() {
        Task { @MainActor in model.agentBecameReachable() }
    }
}

/// Observable store for the device-local folder map, owned by `FaunaMacApp` and
/// injected into the environment. `MacFolderBindingSection` renders `mappings` as
/// the cross-app `folder-location-*` rows under each folder.
///
/// **Binding a folder binds it on the agent.** Since the A4 cutover the writes go
/// through the sync-agent control channel (`FfiSyncAgentProvisioner.bindLocation` /
/// `unbindLocation`), which records the binding in the agent's own config *and*
/// reconciles that set's resident engine live agent-side — the in-app one-shot-only
/// host has no resident engines to start. Mutations are optimistic (the row updates
/// immediately) with the agent exchange in a background task; a failed exchange is
/// logged and healed by the next `reconcile()`. The e2e `sync_inject_locations`
/// command still swaps an in-memory list (no agent, no disk) — the macOS peer of
/// Linux's `inject_locations_for_test` and Windows' `InMemorySyncPipeClient`.
///
/// **The optimistic reconcile itself is shared Rust, not a Swift reimplementation**
/// (`sync-agent.md` § Implementation status today — the C# read-half paragraph
/// names this convergence). `model` holds the pure, IO-free
/// `fauna_client_sync::agent::LocationBindingsModel` (exported as
/// `FfiLocationBindingsModel`) — the same state machine linux and fauna-tui link
/// directly and windows now consumes over the same FFI face. This class is the
/// DRIVER: it owns the `LocationControlChannel` IO, calls `model.reconcile(agentRows:)`
/// to learn what to push, pushes it, and reports outcomes back via
/// `confirmBind`/`confirmUnbind` — mirroring windows' `LocationBindingsController`
/// (`FaunaApp.Core/Services/LocationBindingsController.cs`), the reference this was
/// lifted from.
@MainActor
@Observable
final class LocationsModel {
    /// The shared optimistic state machine. Never touches IO; this class drives it.
    private var model: FfiLocationBindingsModel

    private(set) var mappings: [LocationBinding] = []

    /// Folder names whose binding the agent has PARKED — the owner withdrew
    /// this actor's write grant mid-life (`FfiAgentLocation.accessRevoked`,
    /// mirroring `fauna_ipc::sync::LocationInfo.access_revoked`). A writer's
    /// shared-set row renders `folder-access-revoked-warning` while its name
    /// is in this set (`file-sync.md` § Multi-writer shared sets — D4). Derived
    /// alongside `mappings` on every `render()`, from the model's own rows — no
    /// extra round trip.
    private(set) var revokedFolders: Set<String> = []

    /// The mass-delete floor's per-set hold, keyed by folder (set) name — only
    /// sets with a live non-zero hold appear, mirroring `revokedFolders`'s
    /// membership shape. Renders `folder-location-deletes-held` +
    /// `folder-location-apply-deletes-button` while a set's entry is present;
    /// `0` is not stored (`delete-propagation.md` § A wholesale-vanished folder
    /// is infrastructure failure — the hold is derived per reconcile pass,
    /// never stored, and `0` is the reading that retracts the affordance).
    private(set) var deletesHeld: [String: UInt64] = [:]

    /// The delete rail's unreadable-path count per set
    /// (`FfiBindingRow.deletesSkippedUnreadable`, folded beside the hold by the
    /// same shared `fold_engine_holds`) — only sets with a non-zero count appear,
    /// like `deletesHeld`. Renders `folder-location-unreadable`, a status line
    /// with NO action: it is deliberately independent of `deletesHeld`, the
    /// reading the apply button is gated on, so an unreadable-only set offers
    /// nothing to confirm (`delete-propagation.md` § Unreadable is not absent).
    private(set) var deletesSkippedUnreadable: [String: UInt64] = [:]

    /// True once `injectForTest` has seeded an in-memory map — suppresses both the
    /// agent calls and any disk read, so a test run never touches the real device
    /// config or a real agent.
    private(set) var isInjected = false

    /// The session's agent control channel, handed over once `FaunaMacApp` starts
    /// the provisioner. `nil` before login: the rows still render (loaded from
    /// disk), but binding is deferred until the channel arrives. Held as the
    /// `LocationControlChannel` seam so unit tests can substitute a fake.
    private var provisioner: (any LocationControlChannel)?

    /// Arbitration hook (`FaunaMacApp` sets it to its FP-domain reconcile): the
    /// bound-folder list is an input to the one-local-presence rule, so every
    /// bind/unbind re-converges the File Provider domains — an unbound set's
    /// domain reappears (toggle still ON), a freshly-bound set's domain is
    /// already gone (`yieldDomain` below runs before the agent engine starts).
    var onBindingsChanged: (@Sendable () async -> Void)?

    init() {
        model = FfiLocationBindingsModel()
        render()
    }

    /// A model holding `rows` as pending binds (the model's `add`) — the
    /// test/injection seed. Every row starts `PendingBind`, so a reconcile against
    /// an agent that already holds it just confirms.
    private static func seededModel(_ rows: [LocationBinding]) -> FfiLocationBindingsModel {
        let model = FfiLocationBindingsModel()
        for row in rows {
            _ = model.add(path: row.path, folder: row.folder, folderId: row.folderId)
        }
        return model
    }

    /// Take the session's agent control channel and reconcile: bind any mapping the
    /// agent doesn't know yet (any binding the user made while the channel was
    /// still nil), then adopt the agent's bound-folder list as the truth.
    ///
    /// A single reconcile here is not enough on a **launch that just spawned the agent**:
    /// `FfiSyncAgentProvisioner.start()` only *requests* the agent's spawn and
    /// returns immediately, so this attach can beat the agent's socket coming up
    /// and bail (the agent isn't reachable yet). `FaunaMacApp` therefore also wires
    /// a `SyncAgentReachabilityObserver`, which calls `agentBecameReachable()` the
    /// moment the convergence loop first reaches the agent — that re-fires this
    /// reconcile so the pending bind reliably lands (`sync-agent.md` § Control plane
    /// split; review 2026-07-19).
    func attach(provisioner: any LocationControlChannel) {
        guard !isInjected else { return }
        self.provisioner = provisioner
        Task {
            await reconcile()
            // The agent may know bindings this model didn't (bound on another
            // launch) — re-converge the FP domains on the adopted truth.
            await onBindingsChanged?()
        }
    }

    /// The agent just became reachable (the convergence loop's rising-edge hook,
    /// via `SyncAgentReachabilityObserver`) — re-drive the reconcile so any
    /// binding made while the agent was still spawning reliably reaches the agent
    /// instead of waiting for the next launch or a user mutation.
    func agentBecameReachable() {
        Task {
            await reconcile()
            // Same as attach: the adopted agent truth is an arbitration input —
            // re-converge the FP domains (one-local-presence).
            await onBindingsChanged?()
        }
    }

    /// Bind a local folder to a named folder (one folder ↔ one named folder);
    /// the agent starts syncing it on its side of the exchange. Marks the row
    /// `PendingBind` on the shared model (rendering it immediately, before any
    /// push) and lets `reconcile()` — not this method — do the actual push, so a
    /// bind made against a downed agent is recorded rather than dropped.
    ///
    /// `folderId` is the row's `FolderRef` wire string (`FolderSummary.folderRef`)
    /// — the binding's key, required: a call site whose row yields no ref refuses
    /// the bind instead (the name-keyed bind was retired 2026-09-24).
    /// Returns the background push task (`nil` when nothing is pushed) so a
    /// unit test can await its end rather than guess when it ran; production
    /// call sites fire and forget it.
    @discardableResult
    func add(path: String, folder: String, folderId: String) -> Task<Void, Never>? {
        _ = model.add(path: path, folder: folder, folderId: folderId)
        render()
        guard !isInjected, provisioner != nil else { return nil }
        return Task {
            // One-local-presence: the binding's resident engine is about to start
            // agent-side, so the set's FP domain (if the on-demand toggle had it
            // up) yields FIRST — never two engines writing one set.
            await FileProviderCoordinator.yieldDomain(folderId: folderId)
            await reconcile()
            await onBindingsChanged?()
        }
    }

    /// Unbind a folder: the agent stops its engine and forgets the binding. The
    /// nest folder is untouched — the binding "only records which
    /// already-existing folder a local folder serves" (`file-sync.md` § Hosting
    /// multiple on-demand folders), and the engine's state DB is left on disk, so
    /// re-binding the same folder resumes instead of re-uploading. Keyed by
    /// **path** — the macOS row key, like windows' `RemoveByPathAsync` (two
    /// folders bound to the same set are two distinct rows here, not one).
    func remove(path: String) {
        let removed = model.removeByPath(path: path)
        render()
        guard !isInjected, provisioner != nil, !removed.isEmpty else { return }
        Task {
            await reconcile()
            // One-local-presence: with the binding gone the toggle decides again,
            // so the set's FP domain reappears here (default ON).
            await onBindingsChanged?()
        }
    }

    /// Fold the agent's mass-delete-floor holds
    /// (`FfiSyncAgentProvisioner.listEngineHolds`) onto the model — called from
    /// `SyncAgentHealthModel.onEngineHoldsTick`, the app's existing 10 s
    /// agent-status tick, and NOT from `reconcile()`: the hold is derived
    /// inside the agent on its own rescan cadence, so no user gesture or
    /// reachability edge ever produces it (`delete-propagation.md` § A
    /// wholesale-vanished folder is infrastructure failure). A no-op while
    /// injected — the e2e `sync_inject_locations` seam owns its own roster.
    func foldEngineHolds(_ holds: [FfiEngineHold]) {
        guard !isInjected else { return }
        model.foldEngineHolds(holds: holds)
        render()
    }

    /// Fold the agent's binding PARK (`FfiAgentLocation.accessRevoked`, off
    /// `listLocations`) onto the model — called from
    /// `SyncAgentHealthModel.onLocationsTick`, the same 10 s tick as
    /// `foldEngineHolds` and for the same reason: the park is derived inside
    /// the agent (the owning nest refused a write), and a demotion produces no
    /// gesture or reachability edge of this member's, so `reconcile()` alone
    /// never learned it and `folder-access-revoked-warning` stayed dark
    /// (`file-sync.md` § Multi-writer shared sets → *Revocation*). A mirror of
    /// that one flag in both directions, never a reconcile — it pushes nothing.
    /// A no-op while injected.
    func foldParks(_ agentRows: [FfiAgentLocation]) {
        guard !isInjected else { return }
        model.foldParks(agentRows: agentRows)
        render()
    }

    /// The user confirmed the mass-delete floor's hold on `folder`
    /// (`folder-location-apply-deletes-button`). Deliberately non-optimistic —
    /// unlike `add`/`remove` above, nothing here updates state before the
    /// agent replies: the agent re-derives what is actually missing at click
    /// time, so the only honest post-apply count is the reply's
    /// `remainingHeld`, never the number the button was labelled with. A
    /// failed call leaves the hold standing; nothing was recorded and there is
    /// no client-side state to unwind. `async` rather than a self-spawned Task
    /// (unlike `add`/`remove`, which update optimistically before their push)
    /// — the call site wraps it, matching `AccountSettingsView`'s
    /// `Button { Task { await vm.… } }` convention.
    func applyHeldDeletes(folder: String) async {
        guard !isInjected, let provisioner else { return }
        do {
            let result = try await provisioner.applyHeldDeletes(folder: folder)
            model.setEngineHold(folder: folder, held: result.remainingHeld)
            render()
        } catch {
            logMessage(level: .warn, target: "fauna.sync",
                       message: "applyHeldDeletes \(folder) failed (the hold stands): \(error)")
        }
    }

    /// e2e seam: replace the live map with an injected list, in memory only.
    func injectForTest(_ folders: [LocationBinding]) {
        isInjected = true
        provisioner = nil
        model = Self.seededModel(folders)
        render()
    }

    #if DEBUG
    /// Unit-test seam: seed the initial map explicitly without marking the model
    /// injected, so the
    /// real agent-reconcile path under test still runs — unlike `injectForTest`,
    /// which suppresses that path for the e2e `sync_inject_locations` command.
    init(testSeed mappings: [LocationBinding]) {
        model = Self.seededModel(mappings)
        render()
    }

    /// Unit-test seam: hand the model a `LocationControlChannel` fake synchronously
    /// (no reconcile `Task`), so a test can drive `reconcile()` deterministically.
    func setChannelForTest(_ channel: any LocationControlChannel) {
        provisioner = channel
    }
    #endif

    /// List the agent's current sync-folder rows, hand them to the shared model's
    /// `reconcile(agentRows:)` (which adopts agent truth and returns what's still
    /// pending), push each pending action, and confirm the ones that land.
    ///
    /// **No IO decision lives here** — the model decides what to push (union
    /// semantics: a failed push stays pending, an agent row bound from another
    /// surface is adopted, `accessRevoked` is adopted per-row); this method only
    /// drives the channel and reports outcomes back, mirroring windows'
    /// `LocationBindingsController.ReconcileAsync`.
    ///
    /// Internal (not private) so the unit tests can drive it against a
    /// `LocationControlChannel` fake; production always reaches it through `attach`,
    /// `add`, `remove`, or the `agentBecameReachable` edge.
    func reconcile() async {
        guard !isInjected, let provisioner else { return }
        let agentRows: [FfiAgentLocation]
        do {
            agentRows = try await provisioner.listLocations()
        } catch {
            logMessage(level: .info, target: "fauna.sync",
                       message: "listLocations unavailable (agent not up yet?): \(error)")
            return // Rows already rendered stay as they are — nothing changed.
        }

        let actions = model.reconcile(agentRows: agentRows)
        for bind in actions.toBind {
            do {
                try await provisioner.bindLocation(
                    path: bind.path, folder: bind.folder, folderId: bind.folderId)
                model.confirmBind(path: bind.path)
            } catch {
                logMessage(level: .warn, target: "fauna.sync",
                           message: "bindLocation \(bind.folder) (reconcile) failed: \(error)")
            }
        }
        for path in actions.toUnbind {
            do {
                try await provisioner.unbindLocation(path: path)
                model.confirmUnbind(path: path)
            } catch {
                logMessage(level: .warn, target: "fauna.sync",
                           message: "unbindLocation \(path) (reconcile) failed: \(error)")
            }
        }
        render()
    }

    /// Project the model's rendered union into `mappings`/`revokedFolders` — the
    /// two observable properties every renderer reads. A pending-bind row renders
    /// (an add made while the agent is down must stay visible); `accessRevoked`
    /// mirrors the agent's own per-row flag, adopted on the same reconcile that
    /// adopts everything else — no extra round trip.
    private func render() {
        let rows = model.rendered()
        mappings = rows.map { LocationBinding(path: $0.path, folder: $0.folder, folderId: $0.folderId) }
        revokedFolders = Set(rows.filter(\.accessRevoked).map(\.folder))
        deletesHeld = Dictionary(
            rows.compactMap { $0.deletesHeld > 0 ? ($0.folder, $0.deletesHeld) : nil },
            uniquingKeysWith: { first, _ in first }
        )
        deletesSkippedUnreadable = Dictionary(
            rows.compactMap {
                $0.deletesSkippedUnreadable > 0 ? ($0.folder, $0.deletesSkippedUnreadable) : nil
            },
            uniquingKeysWith: { first, _ in first }
        )
    }
}
