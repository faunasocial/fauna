// Excised from the Fauna Kids build — the kids FFI flavor carries no
// `archive-import` feature (family-safety.md § The account age band, the
// kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// Shared view-model for the user-facing `archive-import` wizard page (macOS +
/// iOS, one FaunaKit VM — `docs/goal/behavior/archive-import.md` § The wizard
/// and its machine). A thin proxy over the shared `ArchiveImportMachine`
/// (UniFFI, built by `buildArchiveImportMachine`): the machine owns the
/// six-screen FSM (Source → Archive → Scope → Confirm → Progress → Done), the
/// parser, the folder writes and the authoring; this VM holds the latest
/// `ArchiveImportSnapshot` as `@Observable` state and re-reads it after each
/// `hydrate` / `dispatch`. The `MailImportVM` shape; lifts the tui lead page
/// (`apps/fauna-tui/src/settings/archive_import.rs`).
///
/// # This VM spawns `run_import` itself
///
/// The machine never self-spawns its drive loop (`MailImportMachine`'s
/// contract, which this machine shares): after a `Start`/`Resume` whose
/// post-dispatch snapshot really reads `.running` — or a hydrate that finds a
/// run still running — ``driveImport`` runs the loop and repaints on a
/// ``progressTickMs``-millisecond tick while the wizard is on Progress.
@MainActor @Observable
public final class ArchiveImportVM {
    /// The Progress screen's repaint cadence — the mail-import page's tick.
    static let progressTickMs: UInt64 = 400

    /// Latest snapshot; `nil` until `configure`.
    public private(set) var snapshot: ArchiveImportSnapshot?
    /// Page-level error surface (`error-message`).
    public var errorMessage: String?
    public private(set) var isLoading = false
    /// Set when the glue refused to build the machine (a bearer-only session
    /// holds no identity key to sign the imported posts with): the page states
    /// it and offers no wizard that could not commit (the tui UNAVAILABLE shape).
    public private(set) var unavailable = false

    private var machine: ArchiveImportMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a
    /// session re-point (`MailImportVM`).
    private var configuredApi: APIClient?
    /// Guards against a second drive loop when Resume is pressed while one is
    /// already running.
    private var driving = false
    enum Loop: CaseIterable, Sendable { case tick, run }
    @ObservationIgnored let loops = DriveLoops<Loop>()

    public init() {}

    /// Retire the drive loops and drop the machine — on a different `APIClient`
    /// and when the page disappears (`MailImportVM.reset`).
    public func reset() {
        loops.cancelAll()
        driving = false
        machine = nil
        configuredApi = nil
        snapshot = nil
        errorMessage = nil
        isLoading = false
        unavailable = false
        #if DEBUG
        if Self.currentForTest === self { Self.currentForTest = nil }
        #endif
    }

    /// Vend the machine for the signed-in actor whose sync device id is
    /// `deviceId`, then hydrate. Idempotent.
    public func configure(api: APIClient, deviceId: String) async {
        if machine != nil, configuredApi !== api { reset() }
        guard machine == nil || configuredApi !== api else { return }
        do {
            adopt(try await api.archiveImportMachine(deviceIdHex: deviceId), from: api)
        } catch {
            unavailable = true
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
    }

    func adopt(_ m: ArchiveImportMachine, from api: APIClient) {
        configuredApi = api
        machine = m
        snapshot = m.snapshot()
        #if DEBUG
        Self.currentForTest = self
        #endif
    }

    /// Find an unfinished import in an archive folder and land on its Progress
    /// step; a run the hydrate finds still running is driven again.
    public func hydrate() async {
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        do { try await machine.hydrate() }
        catch { errorMessage = DisplayError.message(error) }
        readBack(machine)
        if snapshot?.runState == .running { driveImport() }
    }

    public func dispatch(_ action: ArchiveImportAction) async {
        await dispatchSequence([action])
    }

    /// Dispatch `actions` in order, awaiting each before the one re-read, and
    /// stopping at the first refusal — the path and date fields are drafts
    /// committed here as one ordered op (the tui `open_actions` /
    /// `scope_next_actions` shape), never a `Set*` per keystroke.
    public func dispatchSequence(_ actions: [ArchiveImportAction]) async {
        guard let machine, !actions.isEmpty else { return }
        isLoading = true
        defer { isLoading = false }
        let wantsRun = actions.contains {
            switch $0 {
            case .start, .resume: return true
            default: return false
            }
        }
        for action in actions {
            do { try await machine.dispatch(action: action) }
            // The refusal is on the snapshot's `error` too.
            catch { break }
        }
        readBack(machine)
        // Only an accepted transition arms the loop: a refused Start or Resume
        // leaves the run where it was.
        if wantsRun, snapshot?.runState == .running { driveImport() }
    }

    /// The drive loop plus the Progress repaint tick, as two cooperating tasks
    /// (`MailImportVM.driveImport`): `runImport` suspends for the whole run.
    /// Every body holds the VM weakly, so a live loop never outlives the page.
    func driveImport() {
        guard let machine, !driving else { return }
        driving = true
        // Phase two (`archive-import.md` § Linking and merging): hand the
        // login's archive links to the machine before its run is spawned, so a
        // run that completes sets the Done step's link counts and runs the
        // announce round itself — nothing is glued onto the run here (tui's
        // `LinkRound`). No links (no conversations session yet) → a plain run.
        if let links = configuredApi?.archiveLinks() {
            machine.attachLinks(links: links)
        }
        loops.spawnTick(.tick, everyMs: Self.progressTickMs,
                        while: { [weak self] in self?.driving == true },
                        read: { [weak self] in self?.readBack(machine) })
        loops.spawn(.run) { [weak self] in
            defer { if !Task.isCancelled { self?.driving = false } }
            do { try await machine.runImport() }
            catch {
                if Task.isCancelled { return }
                self?.errorMessage = DisplayError.message(error)
            }
            if Task.isCancelled { return }
            self?.readBack(machine)
        }
    }

    private func readBack(_ machine: ArchiveImportMachine) {
        let snap = machine.snapshot()
        snapshot = snap
        errorMessage = snap.error
    }

    #if DEBUG
    /// The mounted page's VM, for the `archive_import_pause_after` /
    /// `archive_import_hold_after` agent commands (`ArchiveImportTestCommand`);
    /// `nil` while no page holds a machine.
    static weak var currentForTest: ArchiveImportVM?

    /// Arm the machine's one-shot pause (or hold) after `records` settled
    /// records — the restart-resume and pause/cancel journeys' causal anchors
    /// (convention 14). `false` with no machine. The setters are `test-helpers`
    /// UniFFI exports, hence `#if DEBUG` (`test_apple_seam_gating.py`).
    func armTestAnchor(hold: Bool, records: UInt64) -> Bool {
        guard let machine else { return false }
        if hold {
            machine.setHoldAfterRecordsForTest(records: records)
        } else {
            machine.setPauseAfterRecordsForTest(records: records)
        }
        return true
    }
    #endif
}
#endif // !FAUNA_KIDS
