// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Shared view-model for the user-facing `mail-export` wizard page (macOS + iOS,
/// one FaunaKit VM). A thin proxy over the shared
/// `MailExportMachine` (UniFFI): the machine owns the five-step wizard FSM
/// (Format → Scope → Confirm → Progress → Done), the format pick, scope
/// default-selection, the start/pause/resume/cancel sequencing and the whole of
/// § Download flow; this VM holds the latest `MailExportSnapshot` as
/// `@Observable` state and re-reads it after each `hydrate` / `dispatch`.
/// Target behavior: `docs/goal/behavior/mail-export.md`; prior art tui
/// (`apps/fauna-tui/src/mail_glue.rs`) and linux
/// (`apps/fauna-linux/src/settings/mail_export.rs`).
///
/// # This VM drives the export
///
/// The machine is built with key custody (`APIClient.mailExportMachine`), so the
/// VM does the three things custody obliges — tui's and linux's leg, lifted:
///
/// 1. **It spawns `runExport`** after a `Start` or `Resume` whose
///    *post-dispatch* snapshot reads `Running` — never after a rejected one.
///    Custody and the spawn land together: custody alone would open a session
///    nothing drives, a Progress screen stuck at zero holding one of the user's
///    three concurrency slots.
/// 2. **It repaints Progress on a tick** while the loop mutates the machine's
///    snapshot (`MailImportVM.driveImport`'s twin). The tick only re-reads the
///    snapshot and touches no view state, so it cannot disarm anything the user
///    armed.
/// 3. **Download runs § Download flow** (`MailExportAction.download`) into
///    `APIClient.mailExportSaveDir()`, and the snapshot's `savedArchivePath`
///    names where it went — the Done summary says so, and on iOS the view offers
///    the finished file through the share sheet.
///
/// The actor handle names the archive's root directory and the saved file; it
/// may arrive after the machine is built or change with the user, so it is read
/// at the gesture (`setActorHandle` before Start / Resume / Download), never
/// only at construction.
@MainActor @Observable
public final class MailExportVM: MachineBackedVM {
    /// Same cadence as the import twin (linux's `PROGRESS_TICK_MS`).
    static let progressTickMs: UInt64 = MailImportVM.progressTickMs

    /// Latest snapshot; `nil` until `configure`.
    public internal(set) var snapshot: MailExportSnapshot?
    /// Page-level error surface (`error-message`).
    public var errorMessage: String?
    public internal(set) var isLoading = false

    var machine: MailExportMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a session
    /// re-point, or the machine keeps talking to the old nest. See `MailSettingsVM`.
    private var configuredApi: APIClient?
    /// Reads the signed-in account's current handle (empty when not yet known).
    private var currentHandle: () -> String = { "" }
    /// Guards against a second drive loop when `Resume` is pressed while one is
    /// already running (linux's `ticking` cell).
    private var driving = false
    /// The two drive loops (`runExport` and the Progress tick), held as one set:
    /// `reset()` cancels it, and so does the VM's release at the page's unmount
    /// (`DriveLoops`; account-scoping.md § the in-memory corollary: the loops
    /// that write state are retired with it).
    enum Loop: CaseIterable, Sendable { case tick, run }
    @ObservationIgnored let loops = DriveLoops<Loop>()

    public init() {}

    /// A failed export is a record on Progress: no Pause / Resume / Cancel, one
    /// Dismiss (mail-export.md § A failed export is a record the wizard shows
    /// until the user moves on). Whether a session is adopted at all is the
    /// shared machine's rule; this only names how the snapshot paints.
    nonisolated static func isFailedExport(_ state: ExportSessionState?) -> Bool {
        state == .errored
    }

    /// Retire the drive loops and drop the machine. A page-owned VM is on no
    /// `ActorScope` list (`ActorScope.swift` § State a *view* owns): this runs when
    /// `configure` meets a different `APIClient` and when the page disappears, and
    /// the page's unmount releases the VM, whose `loops` cancel themselves then
    /// too. A Paused or Errored session stays on Progress, so the step alone never
    /// ends the repaint loop.
    public func reset() {
        loops.cancelAll()
        driving = false
        machine = nil
        configuredApi = nil
        snapshot = nil
        errorMessage = nil
        isLoading = false
    }

    /// Vend the machine from APIClient and load the first snapshot. Idempotent.
    public func configure(api: APIClient, handle: @escaping () -> String) async {
        currentHandle = handle
        if machine != nil, configuredApi !== api { reset() }
        guard machine == nil || configuredApi !== api else { return }
        do {
            adopt(try await api.mailExportMachine(handle: handle()), from: api)
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
    }

    /// Hold `m`, built from `api`, as this VM's machine — `configure`'s landing,
    /// and the seam a test uses to seed a machine without hydrating it.
    func adopt(_ m: MailExportMachine, from api: APIClient) {
        configuredApi = api
        machine = m
        snapshot = m.snapshot()
    }

    /// Load the source-mailbox list + any in-flight session from the nest.
    public func hydrate() async { await hydrateFromMachine() }

    /// Dispatch a wizard action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read — and a `Start`/`Resume` that
    /// really landed a running session spawns the drive loop.
    public func dispatch(_ action: MailExportAction) async {
        guard let machine else { return }
        var wantsRun = false
        var needsHandle = false
        switch action {
        case .start, .resume: wantsRun = true; needsHandle = true
        case .download: needsHandle = true
        default: break
        }
        if needsHandle {
            let handle = currentHandle()
            if !handle.isEmpty { machine.setActorHandle(handle: handle) }
        }
        await dispatchToMachine(action)
        if wantsRun, snapshot?.sessionState == .running {
            driveExport()
        }
    }

    /// Fire-and-forget the drive loop plus the Progress repaint tick, as two
    /// cooperating tasks: `runExport` suspends for the whole export, so the tick
    /// cannot live inside it. A failure lands on the snapshot's `error` (and the
    /// session is failed nest-side), which the final re-read surfaces.
    ///
    /// Every body holds the VM weakly, so a live loop never keeps the VM — and
    /// with it the custody machine — alive past the page's unmount.
    func driveExport() {
        guard let machine, !driving else { return }
        driving = true
        // Repaint until the wizard leaves the Progress screen — the machine moves
        // to `Done` itself once the session completes, and a Cancel unwinds it
        // the same way, so this needs no other stop condition.
        spawnTick(onProgress: { [weak self] in self?.snapshot?.step == .progress },
                  read: { [weak self] in self?.readBack(machine) })
        loops.spawn(.run) { [weak self] in
            defer { self?.driving = false }
            do { try await machine.runExport() }
            catch {
                if Task.isCancelled { return }
                self?.errorMessage = DisplayError.message(error)
            }
            if Task.isCancelled { return }
            self?.readBack(machine)
        }
    }

    /// The Progress repaint loop; ends on leaving Progress or on cancellation.
    func spawnTick(onProgress: @escaping @MainActor () -> Bool,
                   read: @escaping @MainActor () -> Void) {
        loops.spawnTick(.tick, everyMs: Self.progressTickMs, while: onProgress, read: read)
    }

    /// Test seam: whether the repaint loop's task is live.
    var tickIsRunning: Bool { loops.isRunning(.tick) }

    private func readBack(_ machine: MailExportMachine) {
        let snap = machine.snapshot()
        snapshot = snap
        errorMessage = snap.error
    }
}
#endif // !FAUNA_KIDS
