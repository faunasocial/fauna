// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
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
    /// The same guard for the Microsoft sign-in's polling loop.
    private var signingIn = false
    /// The four drive loops (`runImport`, `runMicrosoftSignIn` and a repaint tick
    /// beside each), held as one set: `reset()` cancels it, and so does the VM's
    /// release at the page's unmount (`DriveLoops`; account-scoping.md § the
    /// in-memory corollary: the loops that write state are retired with it).
    enum Loop: CaseIterable, Sendable { case tick, run, signInTick, signInRun }
    @ObservationIgnored let loops = DriveLoops<Loop>()

    public init() {}

    /// Retire the drive loops and drop the machine. A page-owned VM is on no
    /// `ActorScope` list (`ActorScope.swift` § State a *view* owns): this runs when
    /// `configure` meets a different `APIClient` and when the page disappears, and
    /// the page's unmount releases the VM, whose `loops` cancel themselves then
    /// too. A Paused session stays on Progress, so the step alone never ends the
    /// repaint loop.
    public func reset() {
        loops.cancelAll()
        driving = false
        signingIn = false
        machine = nil
        configuredApi = nil
        snapshot = nil
        errorMessage = nil
        isLoading = false
    }

    /// The page disappeared: hand a finished import's machine to the session so
    /// the next visit opens on the same Done, then `reset()` as any leave does.
    /// Only Done is kept — a machine anywhere else may hold a live loop or a
    /// Microsoft token, which end with the visit (§ Lifetime and refresh).
    public func leavePage() {
        leavePage(finished: machine?.snapshot().step == .done)
    }

    /// ``leavePage()`` with the step already read — the seam a test uses, since
    /// a seeded machine never reaches Done.
    func leavePage(finished: Bool) {
        configuredApi?.finishedMailImportMachine = finished ? machine : nil
        reset()
    }

    /// Vend the machine from APIClient and load the first snapshot. Idempotent.
    /// A hydrate follows a held machine too: the shared `refresh` keeps its
    /// finished session up unless a live import has since begun.
    public func configure(api: APIClient) async {
        if machine != nil, configuredApi !== api { reset() }
        guard machine == nil || configuredApi !== api else { return }
        do {
            if let held = api.finishedMailImportMachine {
                adopt(held, from: api)
            } else {
                adopt(try await api.mailImportMachine(), from: api)
            }
        } catch {
            errorMessage = DisplayError.message(error)
            return
        }
        await hydrate()
    }

    /// Hold `m`, built from `api`, as this VM's machine — `configure`'s landing,
    /// and the seam a test uses to seed a machine without hydrating it.
    func adopt(_ m: MailImportMachine, from api: APIClient) {
        configuredApi = api
        machine = m
        snapshot = m.snapshot()
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
        // The one list every app asks (`import_action_starts_run`), so a new
        // run-starting action (SkipRest, RestartRebuiltMailboxes) reaches us too.
        let wantsRun = actions.contains { importActionStartsRun(action: $0) }
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
        // The sign-in's polling half, once Microsoft answered with a code —
        // never on a refused `BeginMicrosoftSignIn` (unavailable, no address).
        let beganSignIn = actions.contains {
            if case .beginMicrosoftSignIn = $0 { return true }
            return false
        }
        if beganSignIn, snapshot?.microsoftSignin?.state == .waiting {
            driveMicrosoftSignIn()
        }
    }

    /// **Sign in with Microsoft** (`mail-import-source-oauth-button`): commit
    /// the address the field holds, then ask Microsoft for a code — the shared
    /// `microsoftSignInActions`, so no app restates the ordering.
    public func signInWithMicrosoft(username: String) async {
        await dispatchSequence(microsoftSignInActions(username: username))
    }

    /// Whether the Outlook sign-in is under way — a code requested, out, or its
    /// grant connecting. The button stays inert meanwhile.
    public var microsoftSignInInFlight: Bool {
        switch snapshot?.microsoftSignin?.state {
        case .requesting, .waiting, .connecting: return true
        default: return false
        }
    }

    /// The sign-in's polling half plus its repaint tick, the same two-task
    /// shape as ``driveImport``: `runMicrosoftSignIn` suspends until the grant
    /// ends (it connects to the source by itself and the wizard moves on to
    /// Scope), so the tick that repaints the code and the step change cannot
    /// live inside it. Both bodies hold the VM weakly, as ``driveImport``'s do.
    func driveMicrosoftSignIn() {
        guard let machine, !signingIn else { return }
        signingIn = true
        loops.spawnTick(.signInTick, everyMs: Self.progressTickMs,
                        while: { [weak self] in self?.signingIn == true },
                        read: { [weak self] in self?.readBack(machine) })
        loops.spawn(.signInRun) { [weak self] in
            defer { if !Task.isCancelled { self?.signingIn = false } }
            do { try await machine.runMicrosoftSignIn() }
            // A declined, expired or refused grant is a sentence on the
            // snapshot's `error` too; the read-back below surfaces it.
            catch {}
            if Task.isCancelled { return }
            self?.readBack(machine)
        }
    }

    /// Fire-and-forget the fetch-drive loop plus the Progress repaint tick, as
    /// two cooperating tasks: `runImport` suspends for the whole import, so the
    /// tick cannot live inside it. `runImport` mutates the machine's own
    /// snapshot as it goes; the tick is what repaints it.
    ///
    /// Every body holds the VM weakly, so a live loop never keeps the VM — and
    /// with it the machine — alive past the page's unmount.
    func driveImport() {
        guard let machine, !driving else { return }
        driving = true
        // Repaint until the wizard leaves the Progress screen — the machine
        // moves to `Done` itself once the session completes (`apply_session`),
        // and a Cancel unwinds it the same way, so this needs no other stop
        // condition.
        spawnTick(onProgress: { [weak self] in self?.snapshot?.step == .progress },
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

    /// The Progress repaint loop; ends on leaving Progress or on cancellation.
    func spawnTick(onProgress: @escaping @MainActor () -> Bool,
                   read: @escaping @MainActor () -> Void) {
        loops.spawnTick(.tick, everyMs: Self.progressTickMs, while: onProgress, read: read)
    }

    /// Test seam: whether the repaint loop's task is live.
    var tickIsRunning: Bool { loops.isRunning(.tick) }

    private func readBack(_ machine: MailImportMachine) {
        let snap = machine.snapshot()
        snapshot = snap
        errorMessage = snap.error
    }
}
#endif // !FAUNA_KIDS
