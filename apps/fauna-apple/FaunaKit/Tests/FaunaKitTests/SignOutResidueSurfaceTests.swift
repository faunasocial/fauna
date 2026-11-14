import FaunaFFISwift
import Foundation
import Testing
@testable import FaunaKit

/// `SignOutResidueSurface` — the apple seat of **the sign-out record** and the
/// residue surface (`account-scoping.md` § Erasure follows scope → *The
/// sign-out record is on all 7 apps*), over the `fauna-ffi` face. The
/// routine's own rules are pinned in shared Rust (`sign_out_finish::tests`,
/// `fauna-ffi`'s `sign_out_residue` tests); these pin the seat's plumbing —
/// that the record lands under the base the seat names before anything is
/// erased, that a finish over it (the gesture's tail, or a later launch's)
/// takes the account's credentials, its scope and both records, that the line
/// is the localized `Rendered` copy, and that a residue still on the disk is
/// painted again by the next run and finished silently once it can go.
///
/// Hermetic: temp bases and a private in-memory secret store under the
/// registry — never the developer's real Application Support root or keychain.
/// No agent is dismissed (`caller: .gesture`): the dismissal is shared
/// Rust's, pinned there.
@Suite struct SignOutResidueSurfaceTests {
    private final class MemorySecretStore: FfiSecretStore, @unchecked Sendable {
        private var values: [String: String] = [:]
        private let lock = NSLock()
        func get(key: String) -> String? { lock.withLock { values[key] } }
        func set(key: String, value: String) { lock.withLock { values[key] = value } }
        func delete(key: String) { lock.withLock { _ = values.removeValue(forKey: key) } }
        var keys: [String] { lock.withLock { Array(values.keys) } }
    }

    /// One device: an install base, a store container, a registry naming one
    /// signed-in account whose scope holds the user's data, and the seat over
    /// them.
    private struct Device {
        let root: URL
        let base: URL
        let container: URL
        let store: MemorySecretStore
        let registry: FfiAccountRegistry
        let actor: String
        let seat: ResidueSeat

        init() throws {
            root = FileManager.default.temporaryDirectory
                .appendingPathComponent("fauna-sign-out-\(UUID().uuidString)", isDirectory: true)
            base = root.appendingPathComponent("base", isDirectory: true)
            container = root.appendingPathComponent("container", isDirectory: true)
            try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
            try FileManager.default.createDirectory(at: container, withIntermediateDirectories: true)
            let store = MemorySecretStore()
            self.store = store
            let registry = FfiAccountRegistry.newWithLockDir(store: store, lockDir: base.path)
            self.registry = registry
            actor = try registry.addAccount(
                secretHex: String(repeating: "33", count: 32), nestUrl: "https://a.example",
                deviceId: nil)
            seat = ResidueSeat(
                baseDir: base.path, storeContainerDir: container.path, registry: { registry })
            let scope = base.appendingPathComponent(actor, isDirectory: true)
            try FileManager.default.createDirectory(at: scope, withIntermediateDirectories: true)
            try Data("the user's data".utf8).write(to: scope.appendingPathComponent("mls.db"))
        }

        var scope: URL { base.appendingPathComponent(actor, isDirectory: true) }
        var recordFile: URL { base.appendingPathComponent("sign-out-record.json") }
        var residueFile: URL { base.appendingPathComponent("sign-out-residue.json") }
        var secretStored: Bool { store.keys.contains("fauna/\(actor)/secret") }
        var registered: Bool { registry.list().contains { $0.actorId == actor } }

        func exists(_ url: URL) -> Bool { FileManager.default.fileExists(atPath: url.path) }

        /// The gesture's own finish — what `StatusVM.signOut` runs after the
        /// runtime's stop.
        func gestureFinish(written: [String]) -> SignOutFinishOutcome {
            SignOutResidueSurface.finish(seat: seat, caller: .gesture, written: written)
        }

        /// A later run — the launch's or Remove Again's — over whatever record
        /// is on the disk.
        func laterFinish() -> SignOutFinishOutcome {
            SignOutResidueSurface.finish(seat: seat, caller: .gesture, written: nil)
        }

        func remove() {
            try? FileManager.default.setAttributes(
                [.posixPermissions: 0o755], ofItemAtPath: scope.path)
            try? FileManager.default.removeItem(at: root)
        }
    }

    /// The decision lands on the disk, under the install base, naming the
    /// account — and nothing is erased yet: the gesture writes it before its
    /// first await, and the runtime's stop comes after.
    @Test func theRecordLandsUnderTheBaseBeforeAnythingIsErased() throws {
        let device = try Device()
        defer { device.remove() }

        let recorded = SignOutResidueSurface.recordSignOut(
            seat: device.seat, servedActor: device.actor)

        #expect(recorded == [device.actor])
        #expect(device.exists(device.recordFile))
        let body = try String(contentsOf: device.recordFile, encoding: .utf8)
        #expect(body.contains(device.actor), "the record names the account: \(body)")
        #expect(device.registered && device.secretStored && device.exists(device.scope))
    }

    /// The served account is recorded even when the registry does not list
    /// it — an unreadable index would not name it.
    @Test func theServedAccountIsRecordedEvenWhenTheRegistryOmitsIt() throws {
        let device = try Device()
        defer { device.remove() }
        let served = String(repeating: "cd", count: 32)

        let recorded = SignOutResidueSurface.recordSignOut(seat: device.seat, servedActor: served)

        #expect(Set(recorded) == Set([device.actor, served]))
    }

    /// The gesture's tail: the credentials, the scope and the record go, and a
    /// clean run paints nothing.
    @Test func theGesturesFinishTakesEverythingAndPaintsNothing() throws {
        let device = try Device()
        defer { device.remove() }
        let recorded = SignOutResidueSurface.recordSignOut(
            seat: device.seat, servedActor: device.actor)

        let outcome = device.gestureFinish(written: recorded)

        #expect(outcome.residue == nil)
        #expect(!outcome.failsClosed)
        #expect(!device.registered && !device.secretStored)
        #expect(!device.exists(device.scope))
        #expect(!device.exists(device.recordFile) && !device.exists(device.residueFile))
    }

    /// Account outcome 17: the process died after the record and before the
    /// wipe. The next launch's run finishes it.
    @Test func aLaunchAfterTheAppDiedMidSignOutFinishesIt() throws {
        let device = try Device()
        defer { device.remove() }
        _ = SignOutResidueSurface.recordSignOut(seat: device.seat, servedActor: device.actor)
        // … killed here, inside the runtime's stop.

        let outcome = device.laterFinish()

        #expect(outcome.residue == nil && !outcome.failsClosed)
        #expect(!device.registered && !device.secretStored)
        #expect(!device.exists(device.scope))
        #expect(!device.exists(device.recordFile))
    }

    /// No record: a signed-in launch is left exactly as it was.
    @Test func aLaunchWithNoRecordTouchesNothing() throws {
        let device = try Device()
        defer { device.remove() }

        let outcome = device.laterFinish()

        #expect(outcome.residue == nil && !outcome.failsClosed)
        #expect(device.registered && device.secretStored && device.exists(device.scope))
    }

    /// Beside another window serving the recorded account, a launch's run is
    /// refused and fails closed — nothing is erased, and the line is the
    /// refusal.
    @Test func aLaunchBesideASiblingServingTheAccountFailsClosed() throws {
        let device = try Device()
        defer { device.remove() }
        _ = SignOutResidueSurface.recordSignOut(seat: device.seat, servedActor: device.actor)
        let sibling = try #require(acquireAccountInstanceLockShared(
            stateBase: device.base.path, actorIdHex: device.actor,
            storeContainerDir: device.container.path))
        #expect(sibling.isHeld(), "a genuinely held lock, not the degrade")

        let outcome = device.laterFinish()

        #expect(outcome.failsClosed)
        let residue = try #require(outcome.residue, "a refused run paints its refusal")
        #expect(residue.line == L.settings.signOutResidueRetryBlockedOtherWindow)
        #expect(device.registered && device.secretStored && device.exists(device.scope))
        #expect(device.exists(device.recordFile), "the record still owes its wipe")
        _ = sibling
    }

    /// The POSIX fault the e2e journeys inject: a read-only scope directory.
    /// The gesture's run paints the Remove Again line; while the fault holds a
    /// later run paints it again from the residue record; once lifted, the
    /// next run finishes the erase without a word.
    @Test func aResidueIsPaintedAgainAndFinishedSilentlyOnceItCanGo() throws {
        let device = try Device()
        defer { device.remove() }
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o555], ofItemAtPath: device.scope.path)
        // A process that writes through a read-only dir (root) would make every
        // assertion below pass for the wrong reason.
        let probe = device.scope.appendingPathComponent(".probe")
        if FileManager.default.createFile(atPath: probe.path, contents: Data()) {
            try? FileManager.default.removeItem(at: probe)
            return
        }
        let recorded = SignOutResidueSurface.recordSignOut(
            seat: device.seat, servedActor: device.actor)

        let first = try #require(device.gestureFinish(written: recorded).residue)
        #expect(first.line == L.settings.signOutResidue(count: "1"))
        #expect(!device.secretStored, "the wipe ran first")
        #expect(device.exists(device.residueFile))

        let again = try #require(device.laterFinish().residue, "still on the disk")
        #expect(again.line == first.line)

        try FileManager.default.setAttributes(
            [.posixPermissions: 0o755], ofItemAtPath: device.scope.path)
        #expect(device.laterFinish().residue == nil)
        #expect(!device.exists(device.scope))
        #expect(!device.exists(device.residueFile) && !device.exists(device.recordFile))
    }
}

/// `OnboardingVM`'s half of the sign-out record — the launch's gate
/// (`launchMayProceed`), its fail-closed landing, and Remove Again — driven by
/// a fake routine. Twin of android's `AppLaunchVMTest` and windows'
/// `OnboardingViewModelSignOutResidueTests`.
@MainActor
@Suite struct OnboardingVMSignOutResidueTests {
    private final class FakeResidue: SignOutResidueSurfacing, @unchecked Sendable {
        let line: String
        init(_ line: String) { self.line = line }
    }

    /// A scripted routine: each run answers the next outcome (the last one
    /// repeats) and counts itself.
    private final class Routine: @unchecked Sendable {
        private let lock = NSLock()
        private var outcomes: [SignOutFinishOutcome]
        private var _runs = 0
        var runs: Int { lock.withLock { _runs } }

        init(_ outcomes: SignOutFinishOutcome...) { self.outcomes = outcomes }

        func run() -> SignOutFinishOutcome {
            lock.withLock {
                _runs += 1
                return outcomes.count > 1 ? outcomes.removeFirst() : outcomes[0]
            }
        }
    }

    /// Counts the root's re-entries into its launch.
    private final class Relaunches {
        var count = 0
    }

    private static let line = "Signed out, but 1 item(s) of your data could not be removed."
    private static let refusal = "Not removed — another Fauna window is using some of this data."

    private static func vm(_ routine: Routine) -> OnboardingVM {
        let vm = OnboardingVM()
        vm.signOutFinish = { _ in routine.run() }
        return vm
    }

    /// Wait for the launch's finish to land: the root's re-entry is its signal.
    private static func settle(_ relaunches: Relaunches, to count: Int) async {
        for _ in 0..<2000 where relaunches.count < count { await Task.yield() }
    }

    @Test func theHandoverMovesTheResidueOffTheSession() {
        let vm = OnboardingVM()
        let session = SessionState()
        session.signOutResidue = FakeResidue(Self.line)

        vm.takeSignOutResidue(from: session)

        #expect(vm.signOutResidue?.line == Self.line)
        #expect(session.signOutResidue == nil)
        #expect(vm.errorMessage == nil, "the residue has its own view, never error-message")
    }

    /// Never painted over an append-mode ("Add account") wizard: that user is
    /// signed in (`account-scoping.md` § Erasure follows scope, decision 3 —
    /// the record waits). The record itself is kept, so the next signed-out
    /// `identity_choice` paints it again.
    @Test func anAppendModeWizardPaintsNoResidueButKeepsIt() {
        let vm = OnboardingVM()
        vm.signOutResidue = FakeResidue(Self.line)

        #expect(vm.paintedSignOutResidue(appending: true) == nil)
        #expect(vm.signOutResidue?.line == Self.line, "the record waits")
        #expect(vm.paintedSignOutResidue(appending: false)?.line == Self.line)
    }

    /// The handover re-roots the wizard at `identity_choice` (`settings.md`,
    /// the `sign-out-button` row). A wizard left where the last sign-in finished —
    /// `.done` — re-ran its exit when the signed-out root remounted it: the
    /// handoff re-persisted the identity just erased and relaunched into it,
    /// so a Settings sign-out after a UI sign-in landed back on the main window.
    @Test func theHandoverReRootsTheWizardAtIdentityChoice() {
        let vm = OnboardingVM()
        vm.machine.beginImportIdentity()
        #expect(vm.step == .identityImport)
        let session = SessionState()
        session.signOutResidue = FakeResidue(Self.line)

        vm.takeSignOutResidue(from: session)

        #expect(vm.step == .identityChoice)
        #expect(vm.signOutResidue?.line == Self.line, "the re-root keeps the residue")
    }

    /// A clean erase's handover closes a view a previous one left up.
    @Test func aCleanHandoverClosesTheView() {
        let vm = OnboardingVM()
        vm.signOutResidue = FakeResidue(Self.line)
        vm.takeSignOutResidue(from: SessionState())
        #expect(vm.signOutResidue == nil)
    }

    /// The launch's first ask holds the launch while the finish runs, then
    /// re-enters it; every later ask lets it through without running the
    /// finish again — once per process.
    @Test func theLaunchFinishRunsOnceThenLetsTheLaunchThrough() async {
        let routine = Routine(.nothing)
        let vm = Self.vm(routine)
        let relaunches = Relaunches()

        #expect(!vm.launchMayProceed { relaunches.count += 1 }, "held while the finish runs")
        await Self.settle(relaunches, to: 1)
        #expect(relaunches.count == 1, "the root re-enters its launch once the finish lands")

        #expect(vm.launchMayProceed { relaunches.count += 1 })
        #expect(vm.launchMayProceed { relaunches.count += 1 })
        #expect(routine.runs == 1, "a re-entry never runs the finish again")
        #expect(!vm.launchFailedClosed)
        #expect(vm.signOutResidue == nil)
    }

    /// What the launch's finish could not remove is painted.
    @Test func theLaunchFinishPaintsWhatIsStillLeft() async {
        let vm = Self.vm(Routine(SignOutFinishOutcome(
            residue: FakeResidue(Self.line), failsClosed: false)))
        let relaunches = Relaunches()

        _ = vm.launchMayProceed { relaunches.count += 1 }
        await Self.settle(relaunches, to: 1)

        #expect(vm.signOutResidue?.line == Self.line)
        #expect(vm.launchMayProceed {}, "a residue alone does not hold the launch")
    }

    /// A sign-out that landed while the launch's finish ran recorded the newer
    /// statement; the finish never paints over it.
    @Test func theLaunchFinishNeverOverwritesANewerSignOut() async {
        let vm = Self.vm(Routine(SignOutFinishOutcome(
            residue: FakeResidue("older"), failsClosed: false)))
        let relaunches = Relaunches()
        vm.signOutResidue = FakeResidue("newer")

        _ = vm.launchMayProceed { relaunches.count += 1 }
        await Self.settle(relaunches, to: 1)

        #expect(vm.signOutResidue?.line == "newer")
    }

    /// Decision 4: a refused finish whose record still owes its wipe fails
    /// closed — the launch stays held on `identity_choice` with the refusal
    /// line, however often the root asks, until Remove Again no longer fails
    /// closed; then the root re-enters its launch.
    @Test func aFailedClosedLaunchIsHeldUntilRemoveAgainClearsIt() async {
        let routine = Routine(
            SignOutFinishOutcome(residue: FakeResidue(Self.refusal), failsClosed: true),
            SignOutFinishOutcome(residue: FakeResidue(Self.refusal), failsClosed: true),
            .nothing)
        let vm = Self.vm(routine)
        let relaunches = Relaunches()

        _ = vm.launchMayProceed { relaunches.count += 1 }
        await Self.settle(relaunches, to: 1)
        #expect(vm.launchFailedClosed)
        #expect(vm.signOutResidue?.line == Self.refusal)
        #expect(!vm.launchMayProceed { relaunches.count += 1 }, "still held")
        #expect(vm.step == .identityChoice)

        await vm.retrySignOutResidue()
        #expect(vm.launchFailedClosed, "the sibling still serves it")
        #expect(relaunches.count == 1)

        await vm.retrySignOutResidue()
        #expect(!vm.launchFailedClosed)
        #expect(vm.signOutResidue == nil)
        #expect(relaunches.count == 2, "Remove Again re-entered the launch")
        #expect(vm.launchMayProceed {})
        #expect(routine.runs == 3)
    }

    @Test func aRetryThatFinishesTheEraseClosesTheView() async {
        let vm = Self.vm(Routine(.nothing))
        vm.signOutResidue = FakeResidue(Self.line)
        await vm.retrySignOutResidue()
        #expect(vm.signOutResidue == nil)
    }

    /// The retry's refusal beside a sibling window replaces the line in place.
    @Test func aRefusedRetryKeepsTheViewWithItsOwnLine() async {
        let vm = Self.vm(Routine(SignOutFinishOutcome(
            residue: FakeResidue(Self.refusal), failsClosed: false)))
        vm.signOutResidue = FakeResidue(Self.line)
        await vm.retrySignOutResidue()
        #expect(vm.signOutResidue?.line == Self.refusal)
    }

    /// Presses are serialized, never dropped: the second runs over what the
    /// first left.
    @Test func twoPressesRunInTurnOverWhatTheFirstLeft() async {
        let routine = Routine(
            SignOutFinishOutcome(residue: FakeResidue(Self.line), failsClosed: false), .nothing)
        let vm = Self.vm(routine)
        vm.signOutResidue = FakeResidue(Self.line)

        async let press1: Void = vm.retrySignOutResidue()
        async let press2: Void = vm.retrySignOutResidue()
        _ = await (press1, press2)

        #expect(routine.runs == 2, "both presses ran")
        #expect(vm.signOutResidue == nil, "the second ran over the first's result")
    }
}
