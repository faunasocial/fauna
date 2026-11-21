import Foundation

/// A UniFFI-generated snapshot struct that carries a page-level failure reason
/// (`error-message`) — every `MachineBacked` machine's snapshot type already has
/// this field; conforming it retroactively (below) just names the shape once.
///
/// Internal (not `public`): stays a same-module implementation detail of
/// `MachineBackedVM` below, so its conformers can keep `machine`/`snapshot`'s
/// setter/`isLoading`'s setter genuinely non-public — a `public` protocol would
/// force every witness public too (Swift's protocol-witness access rule),
/// exposing the raw machine and settable snapshot/isLoading to any module.
protocol MachineSnapshotWithError {
    var error: String? { get }
}

/// The UniFFI-generated machine shape shared by every simple hydrate/dispatch/
/// pull-snapshot mail or nest-trust page: an async `hydrate()`, an async
/// `dispatch(action:)`, and a pull `snapshot()`. Each generated `XxxMachineProtocol`
/// already declares exactly this shape; conforming the generated `open class`
/// retroactively (below) just names it once instead of once per `MachineBackedVM`
/// conformer. `Sendable` (every generated machine is `@unchecked Sendable`)
/// keeps each conformance nonisolated: `MachineBackedVM` is `@MainActor`, and
/// Swift 6.4 otherwise reads its `Machine: MachineBacked` as possibly isolated,
/// which the nonisolated async `hydrate()`/`dispatch(action:)` calls cannot take.
protocol MachineBacked: AnyObject, Sendable {
    associatedtype Act
    associatedtype Snap: MachineSnapshotWithError
    func hydrate() async throws
    func dispatch(action: Act) async throws
    func snapshot() -> Snap
}

/// Shared hydrate/dispatch/re-read lifecycle for a machine-backed `@Observable`
/// ViewModel. A conformer supplies its own `configure(api:)` (machine vending
/// differs per page — direct, list-scoped, dual-machine, …), its own extra
/// derived properties, and a one-line **public** `hydrate()`/`dispatch(_:)`
/// forward to `hydrateFromMachine()`/`dispatchToMachine(_:)` below (the
/// forward — not a direct public protocol requirement — is what keeps
/// `machine`/`snapshot`/`isLoading` non-public: see `MachineSnapshotWithError`'s
/// doc). This protocol owns only the identical hydrate/dispatch/re-read
/// plumbing found byte-identical across `MailListsVM`/`MailAliasesVM`/
/// `MailListMembersVM`/`MailExportVM`/`MailSpamVM`/`LinkedNestsVM`.
/// Pages whose `dispatch` genuinely diverges — `AdminMailVM`'s extra
/// `snapshotVersion` re-seed counter, `MailImportVM`'s `dispatchSequence` —
/// stay full non-conformers; forcing them onto this shape would paper over
/// real behavior differences (pass 66/67's own stated risk). `MailSettingsVM`
/// (thrown-vs-snapshot error precedence) and `BridgeApprovalVM` (the
/// no-clobber comment) diverge on `dispatch` only — their `hydrate()` was
/// byte-identical to `hydrateFromMachine()` all along, so they conform for
/// that one requirement and keep a hand-rolled `dispatch()` calling
/// `machine.dispatch(action:)` directly, never `dispatchToMachine(_:)`
/// . `AdminCalendarVM`/`AdminContactsVM`/
/// `AdminFilesVM` looked divergent at that same pass (their own local
/// `applySnapshot()` indirection) but turned out, on closer read (pass 69),
/// to fit this protocol exactly — and their `applySnapshot()` carried the
/// identical clobber bug this protocol already fixed once for `LinkedNestsVM`
/// (a caught hydrate-throw's `errorMessage` immediately overwritten back to
/// `nil` by the unconditional re-read that follows it in the same function).
///
/// **Account scope.** A conformer's `configure(api:)` guard (`machine == nil`,
/// `configuredApi !== api`) is not what keeps one account's data off the next
/// account's page — the page's unmount is. Where that guarantee comes from, per
/// target, and which pages owe a seam of their own is stated once, in
/// ``ActorScope``'s *State a view owns* section.
@MainActor
protocol MachineBackedVM: AnyObject {
    associatedtype Machine: MachineBacked
    var machine: Machine? { get set }
    var snapshot: Machine.Snap? { get set }
    var errorMessage: String? { get set }
    var isLoading: Bool { get set }

    /// Called once `machine.hydrate()` returns without throwing, before the
    /// snapshot re-read. Default no-op; a list page overrides it to flip its
    /// own `loaded` flag (the "loading is not empty" gate, `ui/README.md`
    /// § List pages) — a wizard/settings-shaped page with no such gate leaves
    /// it unimplemented.
    func didHydrateSuccessfully()

    /// What a gesture says when it reaches this page with no machine
    /// (`ui/README.md` § Offer only what is possible, rule 7: never a silent
    /// return). Default `nil` — the bare return stands for a page that has no
    /// such ruling yet; every mail page answers the shared
    /// `mailUsableUnbuilt()` reason (`mail-settings.md` § Errors & edge cases,
    /// *The in-page predicates*) through ``MailUnbuiltRefusing``.
    var missingMachineRefusal: String? { get }
}

extension MachineBackedVM {
    func didHydrateSuccessfully() {}
    var missingMachineRefusal: String? { nil }

    /// Refresh from the nest (page mount). Never clobbers a thrown-hydrate
    /// error with a since-cleared `snapshot.error` — the bug pass 68 found (and
    /// fixed by moving `LinkedNestsVM` onto this shared, already-safe path) in
    /// one of the six "identical" hydrate bodies, which had silently drifted to
    /// an unconditional `errorMessage = snap.error` instead of this `if let`.
    func hydrateFromMachine() async {
        guard let machine else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            try await machine.hydrate()
            didHydrateSuccessfully()
        } catch { errorMessage = DisplayError.message(error) }
        let snap = machine.snapshot()
        snapshot = snap
        if let e = snap.error { errorMessage = e }
    }

    /// Dispatch a user action; the machine runs it to completion (or records an
    /// error on the snapshot), then we re-read.
    func dispatchToMachine(_ action: Machine.Act) async {
        guard let machine else {
            if let refusal = missingMachineRefusal { errorMessage = refusal }
            return
        }
        isLoading = true
        defer { isLoading = false }
        try? await machine.dispatch(action: action)
        let snap = machine.snapshot()
        snapshot = snap
        errorMessage = snap.error
    }
}

/// A `MachineSnapshotWithError` whose UniFFI-generated `status` enum has the
/// `idle`/`loading`/`working` shape (`CaldavPolicyStatus`/`CarddavPolicyStatus`/
/// `WebdavPolicyStatus` today — three separately-generated types with identical
/// cases). Conforming it retroactively (below) lets `MachineBackedVM`'s
/// `isBusy` extension read it without a per-conformer hand-rolled
/// `snapshot?.status == .working || snapshot?.status == .loading` — found
/// byte-identical across `AdminCalendarVM`/`AdminContactsVM`/`AdminFilesVM`.
protocol MachineSnapshotWithWorkStatus: MachineSnapshotWithError {
    var isWorking: Bool { get }
}

extension MachineBackedVM where Machine.Snap: MachineSnapshotWithWorkStatus {
    /// A conformer's shared spinner-gate rule: still loading, or the snapshot's
    /// own status reads mid-operation.
    var isBusy: Bool {
        isLoading || (snapshot?.isWorking ?? false)
    }
}

extension MailAliasesSnapshot: MachineSnapshotWithError {}
extension MailListsSnapshot: MachineSnapshotWithError {}
extension MailListMembersSnapshot: MachineSnapshotWithError {}
extension MailExportSnapshot: MachineSnapshotWithError {}
extension MailSpamSnapshot: MachineSnapshotWithError {}
extension LinkedNestsSnapshot: MachineSnapshotWithError {}
extension CaldavPolicySnapshot: MachineSnapshotWithWorkStatus {
    var isWorking: Bool { status == .working || status == .loading }
}
extension CarddavPolicySnapshot: MachineSnapshotWithWorkStatus {
    var isWorking: Bool { status == .working || status == .loading }
}
extension WebdavPolicySnapshot: MachineSnapshotWithWorkStatus {
    var isWorking: Bool { status == .working || status == .loading }
}
// `AdminMailVM` doesn't conform to `MachineBackedVM` (its `dispatch()` genuinely
// diverges, per the doc above) but its `isBusy` had the identical hand-rolled
// check, so `MailPolicySnapshot` conforms here too — `AdminMailVM.isBusy` calls
// `isWorking` directly rather than through the (inapplicable) VM extension.
extension MailPolicySnapshot: MachineSnapshotWithWorkStatus {
    var isWorking: Bool { status == .working || status == .loading }
}
// `MailSettingsVM`/`BridgeApprovalVM` conform ONLY for `hydrate()` — their
// `dispatch()`s genuinely diverge (thrown-vs-snapshot error precedence /
// the no-clobber guard, per the doc above) and stay hand-rolled, calling
// `machine.dispatch(action:)` directly rather than `dispatchToMachine(_:)`.
extension DmarcReportsSnapshot: MachineSnapshotWithWorkStatus {
    var isWorking: Bool { status == .loading }
}
extension MailSettingsSnapshot: MachineSnapshotWithError {}
extension BridgeApprovalSnapshot: MachineSnapshotWithError {}

extension MailAliasesMachine: MachineBacked {}
extension MailListsMachine: MachineBacked {}
extension MailListMembersMachine: MachineBacked {}
extension MailExportMachine: MachineBacked {}
extension MailSpamMachine: MachineBacked {}
extension LinkedNestsMachine: MachineBacked {}
extension CaldavPolicyMachine: MachineBacked {}
extension CarddavPolicyMachine: MachineBacked {}
extension WebdavPolicyMachine: MachineBacked {}
extension DmarcReportsMachine: MachineBacked {}
extension MailSettingsMachine: MachineBacked {}
extension BridgeApprovalMachine: MachineBacked {}

#if !FAUNA_KIDS
/// A shared admin DAV machine that answers whether its enable toggle (and, on
/// CalDAV, the port save) is live — `toggle_possibility` (admin.md § Errors &
/// edge cases, *Controls wait for their read*): blocked until the page's
/// `get_mail_config` read resolves, blocked after a failed one, blocked while
/// a change is on its way.
protocol TogglePossibilityMachine {
    func togglePossibility() -> Possibility
    /// Where the page's `get_mail_config` read stands — an unanswered one is
    /// asked again when the nest connection comes back (``AdminPageReading``).
    func readState() -> ReadState
}

extension CaldavPolicyMachine: TogglePossibilityMachine {}
extension CarddavPolicyMachine: TogglePossibilityMachine {}
extension WebdavPolicyMachine: TogglePossibilityMachine {}

/// The three admin DAV pages' one gate (Calendar, Contacts, Files): paint the
/// toggle from the machine's answer, refuse the press with the same reason.
/// tui's twin is `admin::action_possibility`'s `Toggle*Enabled` arms.
@MainActor
protocol ReadGatedToggleVM: MachineBackedVM where Machine: TogglePossibilityMachine {
    /// The machine could not be built — the page will never read.
    var machineBuildFailed: Bool { get set }
}

extension ReadGatedToggleVM {
    /// The toggle's answer. Reads `snapshot` and `isLoading` so a SwiftUI body
    /// that paints it re-renders on every re-read; the answer itself is the
    /// machine's, asked now.
    var toggleAnswer: Possibility {
        _ = snapshot
        _ = isLoading
        return machinePossibility(machine, buildFailed: machineBuildFailed) { $0.togglePossibility() }
    }

    /// Dispatch a write only when the toggle's answer allows it; otherwise its
    /// reason goes on `error-message` and nothing reaches the nest.
    func dispatchGated(_ action: Machine.Act) async {
        if refusePress(toggleAnswer, errorMessage: &errorMessage) { return }
        await dispatchToMachine(action)
    }
}
#endif

/// A mail page: a gesture reaching it with no machine — its build failed, so
/// the entry gate answers `mail_usable_unbuilt()` — refuses with that same
/// reason on `error-message` (`mail-settings.md` § Errors & edge cases, *The
/// in-page predicates*; tui's `mail_machine_or_refuse`).
@MainActor
protocol MailUnbuiltRefusing {}

extension MachineBackedVM where Self: MailUnbuiltRefusing {
    var missingMachineRefusal: String? { mailUsableUnbuilt().refusal }
}
