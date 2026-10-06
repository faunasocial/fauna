import SwiftUI

/// Thin SwiftUI-friendly proxy over the page-level `BackupsMachine` (UniFFI,
/// `libs/fauna-backups-machine` via `libs/fauna-ffi`). The Backups page's
/// **snapshot half** — the folder selector, the snapshot list, `last-backed-up`,
/// create / delete / immediate-delete / prune / check, and the
/// `snapshot-detail-files` read — all live in the shared-Rust machine; this class:
///
///   1. builds + owns the machine instance (over the session `FfiNestClient`),
///   2. implements `BackupsObserver` to translate machine notifications into
///      `@Observable` invalidations on the main actor,
///   3. exposes the latest `BackupsSnapshot` + small gesture wrappers so SwiftUI
///      views read `vm.snapshot` / drive `vm.createSnapshot()` instead of
///      reaching into `vm.machine` everywhere.
///
/// Shared by the macOS and iOS apps — one FaunaKit VM, identical behaviour on
/// both. Mirrors `DevicesMachineVM`'s observer-box pattern. Target state:
/// `docs/goal/ui/backups.md` § Snapshot-list shape (RATIFIED 2026-08-05) and its
/// *Reconciliation ledger* `apple` row, which this class closes.
///
/// **What deliberately is NOT machine state.** Four surfaces on this page are
/// per-app glue the ratified section leaves to the shell, and they keep living
/// here beside the machine (the `DevicesMachineVM` precedent, which likewise
/// holds the folder-sharing reads next to its machine):
///   * the immediate-delete friction bar's typed inputs (the modal is client
///     glue; only its *enable predicate* is shared — `immediateDeleteEnabled`),
///   * the macOS-only device filter (`devices` / `selectedDeviceId`) — explicitly
///     allowed to stay by the ledger row, and it no longer feeds `last-backed-up`
///     now that the machine derives it,
///   * the repo-stats popover and the snapshot diff (macOS), neither of which the
///     snapshot half owns,
///   * the per-file download (`snapshot-file-download-button`, ratified
///     2026-07-12), which is an `APIClient` byte fetch rather than a page gesture.
@MainActor @Observable
public final class BackupsMachineVM {
    /// The page-level machine. `nil` until `configure` succeeds. Views forward
    /// page gestures through the wrappers below.
    public private(set) var machine: BackupsMachine?

    /// One-time connect/build failure (`api.backupsMachine` threw). Page read /
    /// write failures live on the machine snapshot's `error` instead; both are
    /// surfaced through `errorMessage`.
    public private(set) var connectError: String?

    /// Handed to each machine at construction, and replaced by ``reset()`` so the
    /// machine it drops can no longer reach this VM.
    private var observerBox = BackupsObserverBox()

    /// The `APIClient` this VM is scoped to — the identity key — and the one the four
    /// non-machine reads documented above go through. A fresh login mints a new
    /// `APIClient` (`FaunaClient.api` is a `let`), so a different instance is what
    /// signals an account switch. Held strongly, so the comparison is on a live object
    /// rather than a recyclable `ObjectIdentifier`.
    ///
    /// Invariant: `machine` is non-nil only if it was built over this `api` — `api`
    /// moves only after ``reset()`` has dropped the machine.
    private var api: APIClient?

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary: at the identity change itself, keyed on the identity, with no
    /// hand-listed field set at each caller). ``configure(api:deviceIdHex:)`` calls it
    /// when the api changes, and the iOS pages call it on the nil-client phase of a
    /// switch or sign-out, so a field added to this VM is dropped at every site by
    /// adding it here and nowhere else.
    ///
    /// That is the machine AND the state beside it: the device chips, repo stats,
    /// snapshot diff and sharing error the non-machine reads landed, and the
    /// immediate-delete bar's typed inputs — a half-typed acknowledgement for account
    /// A's snapshot must not survive into account B's page.
    ///
    /// Clears `api` too, so a read still suspended for the outgoing account finds its
    /// identity gone and drops its own result instead of landing it (the in-flight
    /// clause). The outgoing machine's notifications are cut loose from this VM with
    /// its observer box. `probeSnapshot` — the headless layout seam — is not account
    /// state and is left alone.
    public func reset() {
        machine = nil
        connectError = nil
        sharingError = nil
        devices = []
        selectedDeviceId = nil
        stats = nil
        diffResult = nil
        immediateDeleteSnapshotId = nil
        immediateDeleteConfirmId = ""
        immediateDeleteAcknowledge = ""
        immediateDeleteAck = ""
        api = nil
        observerBox.target = nil
        observerBox = BackupsObserverBox()
        _observerTick &+= 1
    }

    // ── Headless layout-probe seam ───────────────────────────────────────────

    /// A pre-populated page used *only* when no machine has been built —
    /// `BackupSplitViewLayoutTests`' `NSHostingView` pane measurement, which must
    /// lay the three panes out in their POPULATED state (an empty folder list
    /// and a closed detail both under-measure, which is exactly why the pane
    /// overflow that test pins stayed invisible to every non-e2e check).
    ///
    /// Production is unaffected: `configure` never sets it, and once a machine
    /// exists the machine's own snapshot always wins.
    public var probeSnapshot: BackupsSnapshot?

    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// Vend the machine from `APIClient` and load the first snapshot. Idempotent
    /// for the *same* `APIClient` — the machine is built once; later calls only
    /// re-`refresh()`. A DIFFERENT `APIClient` (a re-login mints a fresh one) is
    /// another account: the previous account's state is dropped via ``reset()``
    /// **before** building, so a build that then throws leaves the page empty and
    /// retryable — never the previous account's machine paired with the new api.
    ///
    /// `deviceIdHex` is the shell's stable sync device id (`FaunaClient.deviceId`),
    /// stamped onto the machine so manual snapshots carry row provenance
    /// (§ Snapshot-list shape, *Create* ruling). An absent or malformed id clears
    /// it rather than half-setting one — an unattributed snapshot is what the
    /// wire's `Option` means, and is strictly better than a wrong provenance.
    public func configure(api: APIClient, deviceIdHex: String?) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        guard machine == nil else {
            logMessage(level: .info, target: "fauna.backups.vm",
                       message: "configure: machine already built — re-refreshing")
            await refresh()
            return
        }
        connectError = nil
        observerBox.target = self
        logMessage(level: .info, target: "fauna.backups.vm",
                   message: "configure: building the backups machine")
        let built: BackupsMachine
        do {
            built = try await api.backupsMachine(observer: observerBox)
        } catch {
            logMessage(level: .error, target: "fauna.backups.vm",
                       message: "configure: machine build FAILED: \(error)")
            // The build suspended: a `reset()` (the switch's nil-client phase) or a
            // `configure` for another api may have run meanwhile, and a failure for
            // the outgoing account must not paint on that account's successor.
            guard self.api === api else { return }
            connectError = DisplayError.message(error)
            return
        }
        // Same re-check for the success path. A concurrent build for this same api
        // that finished first also wins — one machine, one observer.
        guard self.api === api, machine == nil else { return }
        if let deviceIdHex {
            backupsMachineSetDeviceId(machine: built, deviceIdHex: deviceIdHex)
        }
        machine = built
        logMessage(level: .info, target: "fauna.backups.vm",
                   message: "configure: machine built — first refresh")
        await refresh()
    }

    // ── Observer ─────────────────────────────────────────────────────────────

    fileprivate func onMachineChanged() {
        // @Observable picks up via the property accesses below; provoke a
        // tracked-property read on the main actor so SwiftUI re-renders.
        _observerTick &+= 1
    }
    private var _observerTick: UInt64 = 0

    // ── Read surface (read freshly on every access) ──────────────────────────

    /// The whole renderable snapshot half in one record. `nil` until `configure`
    /// (or until the probe seam above is populated).
    public var snapshot: BackupsSnapshot? {
        _ = _observerTick
        if let machine { return machine.snapshot() }
        return probeSnapshot
    }

    /// The selected set's rows, after the macOS-only device filter. The machine
    /// hands them back newest-first and apps do **not** re-sort
    /// (§ Snapshot-list shape, *Row content contract*).
    public var rows: [SnapshotRow] {
        let all = snapshot?.snapshots ?? []
        guard let deviceId = selectedDeviceId else { return all }
        return all.filter { $0.deviceId == deviceId }
    }

    public var selectedFolder: String? { snapshot?.selectedFolder }

    /// `last-backed-up` — ONE non-indexed element: the SELECTED set's newest
    /// snapshot `created_at`, "never" when it has none (§ Snapshot-list shape,
    /// *`last-backed-up`* ruling). Derived in the machine; never from the device
    /// filter above, and never from a selector row's `cachedLastSnapshotAt`.
    public var lastBackedUpText: String {
        guard let at = snapshot?.lastBackedUp else { return L.backups.lastBackedUpNever }
        return L.backups.lastBackedUpAt(when: Date(epochSeconds: at).relativeFormatted)
    }

    /// Single-flight: every mutating control is disabled while an op runs
    /// (§ Snapshot-list shape, *Create* ruling — this retires the page's ad-hoc
    /// partial busy flags).
    public var busy: Bool { snapshot?.inProgressOp != nil }

    /// Single-flight (§ Snapshot-list shape, *Create* ruling): while the machine
    /// reports an op in flight every mutating control is disabled. A `Refresh` is
    /// included — it is the re-read every mutation ends with.
    public var mutationsDisabled: Bool { selectedFolder == nil || busy }

    /// The `in_progress_op` line, off the shared `fauna_backups_machine::busy_text`
    /// (`docs/goal/ui/backups.md` § Where logic lives):
    /// no app hand-rolls which key an operation maps to. Named `busyOpText`, not
    /// `busyText`, so this property can't shadow the global FFI function of that
    /// name in Swift's scope-prioritized lookup.
    public var busyOpText: String? {
        snapshot?.inProgressOp.map { renderLocalizedText(busyText(op: $0)) }
    }

    /// The open snapshot's id, if any — the machine's `detail` is the single
    /// source of "which row is open" on both targets, so neither shell carries a
    /// `selectedSnapshot` of its own. A detail whose row a re-read no longer
    /// lists is dropped by the machine, which closes the pane here too.
    public var openSnapshotId: Int64? { snapshot?.detail?.snapshotId }

    public var detailFiles: [SnapshotFileRow] { snapshot?.detail?.files ?? [] }

    /// The page-level `error-message`: the connect failure first, else the
    /// machine snapshot's localized `error`. A completed check with problems is
    /// NOT routed here — it is a result (§ Architectural rules, rule 6).
    public var errorMessage: String? {
        _ = _observerTick
        return firstNonNil(connectError, sharingError, snapshot?.error.map(renderLocalizedText))
    }

    /// Transient failure from one of the four non-machine reads above.
    public private(set) var sharingError: String?

    // ── Page gestures ────────────────────────────────────────────────────────

    public func refresh() async {
        await machine?.refresh()
        let snap = machine?.snapshot()
        logMessage(level: .info, target: "fauna.backups.vm",
                   message: "refresh: vm sees folders=\(snap?.folders.count ?? -1) "
                          + "snapshots=\(snap?.snapshots.count ?? -1) "
                          + "machine=\(machine == nil ? "nil" : "built")")
    }

    /// `backup-folder-selector`.
    ///
    /// ⚠ **Re-entry guard** (the shape linux's leg learned and recorded on the
    /// ledger row so this one would not rediscover it): both shells bind a picker
    /// to `snapshot.selectedFolder`, and re-pointing that picker at the machine's
    /// own selection fires the binding's setter again. Without this equality
    /// check that setter dispatches a `select_folder` for the selection the
    /// machine just handed back — an endless refresh loop that also clears the
    /// check result and prune preview each time round.
    public func selectFolder(_ name: String) async {
        guard !name.isEmpty, name != snapshot?.selectedFolder else { return }
        let scope = api
        await machine?.selectFolder(name: name)
        // A switch landed while the machine call was in flight: `name` is a folder of
        // the OUTGOING account, and reading its devices through the incoming account's
        // api would pair the two.
        guard api === scope else { return }
        await loadDevices(folder: name)
    }

    public func createSnapshot() async { await machine?.createSnapshot() }

    public func deleteSnapshot(id: Int64) async {
        await machine?.deleteSnapshot(snapshotId: id)
    }

    /// `snapshot-undelete-button[i]` — recover a soft-deleted row before its
    /// `purge_after` (§ Snapshot-list shape → *Soft-deleted rows*). Single-flight
    /// on the machine's own `in_progress_op`, same as every other mutating
    /// gesture here.
    public func undeleteSnapshot(id: Int64) async {
        await machine?.undeleteSnapshot(snapshotId: id)
    }

    /// `snapshot-prune-button` — the preview half. Prune applies THIS SET'S OWN
    /// resting retention policy and never carries a client-chosen one
    /// (§ Architectural rules, rule 5), which is what retired the page's
    /// hand-encoded `keep_*` writer along with `RetentionEditorSheet`.
    public func prunePreview() async { await machine?.prunePreview() }
    public func pruneExecute() async { await machine?.pruneExecute() }
    public func cancelPrunePreview() { machine?.cancelPrunePreview() }

    /// `snapshot-check-button` — the tagged button fires the check DIRECTLY
    /// (§ Snapshot-list shape, *Check* ruling; the sheet that required a second,
    /// untagged "Start check" click violated the actuation contract).
    public func check() async { await machine?.check() }

    public func openSnapshot(id: Int64) async {
        await machine?.openSnapshot(snapshotId: id)
    }
    public func closeSnapshotDetail() { machine?.closeSnapshotDetail() }

    // ── Immediate-delete friction bar (client glue; shared predicate) ─────────

    /// `immediateDeleteSnapshotId == nil` ⇒ modal closed.
    public var immediateDeleteSnapshotId: Int64?
    /// `immediate-delete-confirm-input` — the user re-types the snapshot id.
    public var immediateDeleteConfirmId = ""
    /// `immediate-delete-acknowledge-input` — the user types the exact ack phrase.
    public var immediateDeleteAcknowledge = ""
    /// The exact phrase the nest checks byte-for-byte (the protocol
    /// `IMMEDIATE_DELETE_ACK_TEXT` via the FFI `immediateDeleteAckText()` free
    /// fn). Captured when the modal opens and displayed for the user to retype —
    /// a client literal could drift from the nest's check.
    public var immediateDeleteAck = ""

    /// The `immediate-delete-confirm-button` enabled flag. **Called, never
    /// re-derived**: the machine threads its REAL in-flight op into the shared
    /// predicate, which is the half every app used to get wrong (this page's own
    /// copy tracked a private `immediateDeleteInProgress` bool that no other
    /// mutation on the page could set).
    public var immediateDeleteEnabled: Bool {
        _ = _observerTick
        guard let machine, let target = immediateDeleteSnapshotId else { return false }
        return machine.immediateDeleteEnabled(
            confirmId: immediateDeleteConfirmId,
            targetId: String(target),
            acknowledge: immediateDeleteAcknowledge)
    }

    /// Open the modal for `snapshotId` — the per-row button OPENS it, never a
    /// one-click delete (backups.md § Architectural rules, rule 4).
    public func openImmediateDelete(snapshotId: Int64) {
        immediateDeleteSnapshotId = snapshotId
        immediateDeleteConfirmId = ""
        immediateDeleteAcknowledge = ""
        immediateDeleteAck = immediateDeleteAckText()
        machine?.clearError()
    }

    /// Close the modal with no side effect (`immediate-delete-cancel-button`).
    public func cancelImmediateDelete() { immediateDeleteSnapshotId = nil }

    /// `immediate-delete-confirm-button`.
    ///
    /// ⚠ The modal closes on **the row leaving the machine's list**, never on the
    /// call returning (the second shape linux's leg recorded): the nest's
    /// `hard_floor_breach` refusal must leave the modal and both typed inputs
    /// standing so the user can see why and retry.
    public func confirmImmediateDelete() async {
        guard let machine, let id = immediateDeleteSnapshotId else { return }
        await machine.deleteSnapshotImmediate(
            snapshotId: id,
            confirmId: immediateDeleteConfirmId,
            acknowledge: immediateDeleteAcknowledge)
        if !machine.snapshot().snapshots.contains(where: { $0.id == id }) {
            immediateDeleteSnapshotId = nil
        }
    }

    // ── Non-machine reads (see the class doc for why each stays here) ─────────

    /// The macOS device-filter chips. Read per selected set.
    public private(set) var devices: [DeviceInfo] = []
    public var selectedDeviceId: String?

    public func loadDevices(folder: String) async {
        guard let api else { return }
        let loaded: [DeviceInfo]
        do {
            loaded = try await api.listFolderDevices(name: folder)
        } catch {
            loaded = []
        }
        // The read suspended: a `reset()` or a switch that ran meanwhile means this
        // is the OUTGOING account's answer, which must not land on the next account.
        guard self.api === api else { return }
        devices = loaded
        if let selected = selectedDeviceId,
           !devices.contains(where: { $0.deviceId == selected }) {
            // The outgoing set's filter must not silently hide the incoming
            // set's rows (it would read as "this set has no snapshots").
            selectedDeviceId = nil
        }
    }

    public private(set) var stats: RepoStatsResponse?

    public func loadStats(folder: String) async {
        guard let api else { return }
        do {
            let loaded = try await api.repoStats(folder: folder)
            guard self.api === api else { return }  // outgoing account's answer — see `loadDevices`
            stats = loaded
        } catch {
            guard self.api === api else { return }
            sharingError = DisplayError.http(error)
        }
    }

    public private(set) var diffResult: SnapshotDiffResponse?

    public func loadDiff(a: Int, b: Int) async {
        guard let api else { return }
        do {
            let loaded = try await api.snapshotDiff(a: a, b: b)
            guard self.api === api else { return }  // outgoing account's answer — see `loadDevices`
            diffResult = loaded
        } catch {
            guard self.api === api else { return }
            sharingError = DisplayError.http(error)
        }
    }

    /// Fetch one file's bytes from a snapshot for local save
    /// (`snapshot-file-download-button`). Caller saves the returned bytes —
    /// under e2e via `SnapshotFileSaver`, otherwise via the platform-native save
    /// mechanism.
    ///
    /// `deviceId` is optional because it comes from the open snapshot's own row,
    /// which the wire may leave unattributed. Refusing HERE — loudly, onto
    /// `error-message` — rather than in each view is e2e convention 11 (an
    /// automation command is honoured or it fails visibly, never a bare
    /// `return`) plus priority #2: one refusal in shared FaunaKit plays on both
    /// targets, where two view-level guards were two silent drops.
    public func downloadSnapshotFile(deviceId: String?, snapshotId: Int64,
                                     path: String) async -> Data? {
        // Input check BEFORE the `api` guard: the device id is wrong
        // independently of whether a nest is connected, and checking it first
        // leaves both refusals assertable without a live API.
        guard let deviceId else {
            sharingError = L.errors.snapshotDeviceUnknown
            return nil
        }
        guard let api else {
            sharingError = L.errors.notConnectedToNest
            return nil
        }
        do {
            return try await api.downloadSnapshotFile(
                deviceId: deviceId, snapshotId: Int(snapshotId), path: path)
        } catch {
            guard self.api === api else { return nil }  // outgoing account's failure — see `loadDevices`
            sharingError = DisplayError.http(error)
            return nil
        }
    }

    /// The device id of the row a detail is open on — what the download walk
    /// resolves against. Read from the machine's own row rather than threaded
    /// through each shell (macOS used to pass it past a SwiftData bridge model
    /// that does not carry it).
    public var openSnapshotDeviceId: String? {
        guard let id = openSnapshotId else { return nil }
        return snapshot?.snapshots.first { $0.id == id }?.deviceId
    }
}

/// Trampoline conforming to UniFFI's `BackupsObserver`. The machine takes the
/// observer at construction time (`build_backups_machine`), so late-binding via
/// `target` lets the VM register itself after `configure`. Mirrors
/// `DevicesObserverBox`.
final class BackupsObserverBox: BackupsObserver, @unchecked Sendable {
    weak var target: BackupsMachineVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onMachineChanged() }
    }
}
