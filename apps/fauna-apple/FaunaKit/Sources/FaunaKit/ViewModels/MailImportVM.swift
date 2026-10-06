import SwiftUI

/// Shared view-model for the user-facing `mail-import` wizard page (macOS + iOS,
/// one FaunaKit VM). A thin proxy over the shared `MailImportMachine` (UniFFI):
/// the machine owns the five-screen wizard FSM (Source → Scope → Confirm →
/// Progress → Done), the provider presets, scope default-selection, and the
/// start/pause/resume/cancel sequencing; this VM holds the latest
/// `MailImportSnapshot` as `@Observable` state and re-reads it after each
/// `hydrate` / `dispatch`. Pull-based (no observer callback), like
/// `MailExportVM`.
///
/// **Unlike the export twin, the backend is REAL end to end** — nest RPC
/// surface plus the shared-Rust foreign-IMAP client both ship
/// (`mailbox-migration.md` § Implementation status today), so an `error` on the
/// snapshot is a genuine source/nest rejection, not a permanent
/// unbuilt-backend explanation.
///
/// # This VM spawns `run_import` itself
///
/// The shared machine deliberately does **not** self-spawn the fetch-drive
/// loop (`mailbox-migration.md` § Implementation status today: "Those legs must
/// spawn `run_import` themselves after a `Start`/`Resume` that comes back
/// `Running`"). [`dispatchSequence`] therefore checks the *post-dispatch*
/// snapshot — a rejected `Start` must not spawn anything — and only then kicks
/// [`driveImport`], which runs the loop and repaints on a
/// ``progressTickMs``-millisecond tick until the wizard leaves the Progress
/// screen. Skip it and the Progress screen sits at zero forever while the
/// session is genuinely open, which reads exactly like a nest bug (linux uses a
/// 400 ms tick, android a ticker coroutine — this is the same shape).
@MainActor @Observable
public final class MailImportVM {
    /// How often the Progress screen re-reads the machine while `run_import`
    /// runs. A pure, cheap, synchronous snapshot read — no RPC, no dispatch.
    /// Same cadence linux's `PROGRESS_TICK_MS` and android's ticker use.
    static let progressTickMs: UInt64 = 400

    /// Latest snapshot; `nil` until `configure`.
    public private(set) var snapshot: MailImportSnapshot?
    /// Page-level error surface (`error-message`).
    public var errorMessage: String?
    public private(set) var isLoading = false

    private var machine: MailImportMachine?
    /// The `APIClient` the cached ``machine`` was built from — re-vend on a
    /// session re-point, or the machine keeps talking to the old nest. See
    /// `MailSettingsVM`.
    private var configuredApi: APIClient?
    /// Guards against a second fetch-drive loop when `Resume` is pressed while
    /// one is already running (linux's `ticking` cell).
    private var driving = false

    public init() {}

    /// Vend the machine from APIClient and load the first snapshot. Idempotent.
    public func configure(api: APIClient) async {
        guard machine == nil || configuredApi !== api else { return }
        do {
            let m = try await api.mailImportMachine()
            configuredApi = api
            machine = m
            snapshot = m.snapshot()
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
    }

    /// Load any in-flight session from the nest and jump to its screen.
    public func hydrate() async {
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        do { try await machine.hydrate() }
        catch { errorMessage = DisplayError.message(error) }
        readBack(machine)
    }

    /// Dispatch one action. The machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    public func dispatch(_ action: MailImportAction) async {
        await dispatchSequence([action])
    }

    /// Dispatch `actions` **in order**, awaiting each before the single
    /// re-read, so a Scope→Confirm advance can never observe a half-committed
    /// scope — and stopping at the first rejection rather than pressing on with
    /// a half-applied form (windows' `DispatchOneAsync`, android's converged
    /// `dispatch()`).
    ///
    /// The ordering is a correctness contract, not polish: a per-keystroke
    /// dispatch races the Connect click and can log in to the source with a
    /// truncated password. The action list itself comes from the shared
    /// `connectActions` / `scopeNextActions`; this side only locates its own
    /// field values.
    public func dispatchSequence(_ actions: [MailImportAction]) async {
        guard let machine, !actions.isEmpty else { return }
        isLoading = true
        defer { isLoading = false }
        let wantsRun = actions.contains {
            if case .start = $0 { return true }
            if case .resume = $0 { return true }
            return false
        }
        for action in actions {
            do { try await machine.dispatch(action: action) }
            // A dispatch failure arrives on the snapshot's `error` too; stop
            // here rather than applying the rest of a half-committed form.
            catch { break }
        }
        readBack(machine)
        // Only a `Start`/`Resume` whose post-dispatch snapshot REALLY reports a
        // running session drives the loop — never a rejected one.
        if wantsRun, snapshot?.sessionState == .running {
            driveImport()
        }
    }

    /// Fire-and-forget the fetch-drive loop plus the Progress repaint tick, as
    /// two cooperating tasks: `runImport` suspends for the whole import, so the
    /// tick cannot live inside it. `runImport` mutates the machine's own
    /// snapshot as it goes; the tick is what repaints it.
    private func driveImport() {
        guard let machine, !driving else { return }
        driving = true
        // Repaint until the wizard leaves the Progress screen — the machine
        // moves to `Done` itself once the session completes (`apply_session`),
        // and a Cancel unwinds it the same way, so this needs no other stop
        // condition.
        Task { @MainActor in
            while snapshot?.step == .progress {
                try? await Task.sleep(for: .milliseconds(Self.progressTickMs))
                readBack(machine)
            }
        }
        Task { @MainActor in
            defer { driving = false }
            do { try await machine.runImport() }
            catch { errorMessage = DisplayError.message(error) }
            readBack(machine)
        }
    }

    private func readBack(_ machine: MailImportMachine) {
        let snap = machine.snapshot()
        snapshot = snap
        errorMessage = snap.error
    }
}
