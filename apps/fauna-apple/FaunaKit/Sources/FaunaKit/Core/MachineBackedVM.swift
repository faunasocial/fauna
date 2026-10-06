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
/// conformer.
protocol MachineBacked: AnyObject {
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
}

extension MachineBackedVM {
    func didHydrateSuccessfully() {}

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
        guard let machine else { return }
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
extension MailSettingsMachine: MachineBacked {}
extension BridgeApprovalMachine: MachineBacked {}
