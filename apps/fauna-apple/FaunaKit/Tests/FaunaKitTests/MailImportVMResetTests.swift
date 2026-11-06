import Foundation
import Testing
@testable import FaunaKit

#if !FAUNA_KIDS
// The import's Progress repaint loop must end on `reset()` even while the
// session is Paused — a Paused session keeps the wizard on Progress, so the step
// condition alone never ends the loop. Driven through the `spawnTick` seam with a
// stand-in "still on Progress" predicate that stays true (the Paused case).

@Test @MainActor func importResetEndsTheProgressTickWhileTheSessionStaysOnProgress() async throws {
    let vm = MailImportVM()
    var reads = 0
    vm.spawnTick(onProgress: { true }, read: { reads += 1 })
    #expect(vm.tickIsRunning)

    try await Task.sleep(for: .milliseconds(MailImportVM.progressTickMs * 3))
    #expect(reads > 0)

    vm.reset()
    #expect(!vm.tickIsRunning)
    let settled = reads
    try await Task.sleep(for: .milliseconds(MailImportVM.progressTickMs * 3))
    #expect(reads == settled)
}

// Every door that retires the import's four drive loops, each
// pinned on its own (`MailExportVMDriveLoopTests`' twin): `reset()`, `configure`
// meeting a different `APIClient`, and the VM's release at the page's unmount.

/// An `APIClient` whose import machine build is refused at once, so a `configure` on
/// it never hydrates (hydrating a machine over a torn-down nest waits out its
/// offline refresh). A machine is seeded through `adopt` instead.
private final class RefusingImportAPI: APIClient {
    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }
    override func mailImportMachine() async throws -> MailImportMachine {
        throw ScopeTestFailure(account: "the incoming account")
    }
}

@MainActor
private func seededImportVM(from api: APIClient) async throws -> MailImportVM {
    let vm = MailImportVM()
    vm.adopt(buildMailImportMachine(nest: try await tornDownNest()), from: api)
    return vm
}

@Suite("MailImportVM's drive loops retire at every door", .timeLimit(.minutes(1)))
@MainActor
struct MailImportVMDriveLoopTests {
    @Test("reset() cancels every drive loop")
    func resetCancelsEveryLoop() async {
        let vm = MailImportVM()
        let witnesses = MailImportVM.Loop.allCases.map { armCancelWitness(vm.loops, $0) }
        vm.reset()
        for witness in witnesses { #expect(await sawCancel(witness)) }
    }

    @Test("configure on a different APIClient cancels the loops; the same one does not")
    func repointCancelsTheLoops() async throws {
        let first = RefusingImportAPI()
        let vm = try await seededImportVM(from: first)
        let run = armCancelWitness(vm.loops, .run)

        await vm.configure(api: first)
        #expect(vm.loops.isRunning(.run))

        await vm.configure(api: RefusingImportAPI())
        #expect(await sawCancel(run))
    }

    @Test("releasing the VM cancels its loops")
    func releaseCancelsTheLoops() async {
        var vm: MailImportVM? = MailImportVM()
        let run = armCancelWitness(vm!.loops, .run)
        vm = nil
        #expect(await sawCancel(run))
    }

    @Test("the real drive bodies hold the VM weakly")
    func driveBodiesDoNotRetainTheVM() async throws {
        var vm: MailImportVM? = try await seededImportVM(from: RefusingImportAPI())
        vm!.driveImport()
        vm!.driveMicrosoftSignIn()
        #expect(vm!.loops.isRunning(.run))
        #expect(vm!.loops.isRunning(.signInRun))
        weak var released = vm
        vm = nil
        #expect(released == nil)
    }
}

// § Wizard steps step 6: a finished import's machine outlives the page on the session's
// client, and nothing else does.

@Suite("MailImportVM hands a finished machine to the session", .timeLimit(.minutes(1)))
@MainActor
struct MailImportVMLeavePageTests {
    @Test("leaving a finished import holds its machine, and the next visit adopts it")
    func finishedMachineSurvivesTheVisit() async throws {
        let api = RefusingImportAPI()
        let leaving = try await seededImportVM(from: api)
        leaving.leavePage(finished: true)
        #expect(api.finishedMailImportMachine != nil)
        #expect(leaving.snapshot == nil)

        let returning = MailImportVM()
        returning.adopt(api.finishedMailImportMachine!, from: api)
        #expect(returning.snapshot != nil)
    }

    @Test("leaving anywhere but Done holds nothing and clears an earlier hold")
    func unfinishedLeaveClearsTheHold() async throws {
        let api = RefusingImportAPI()
        let vm = try await seededImportVM(from: api)
        api.finishedMailImportMachine = buildMailImportMachine(nest: try await tornDownNest())
        vm.leavePage(finished: false)
        #expect(api.finishedMailImportMachine == nil)
    }

    @Test("leaving retires the drive loops whatever the step")
    func leavingCancelsTheLoops() async throws {
        let vm = try await seededImportVM(from: RefusingImportAPI())
        let run = armCancelWitness(vm.loops, .run)
        vm.leavePage(finished: true)
        #expect(await sawCancel(run))
    }
}

// `FfiStaleSurfaces.mail_import` reaches the open Import page as its own
// notification, not the export twin's or the spam page's.
@Test func theImportPushIsItsOwnNotification() {
    #expect(Notification.Name.faunaMailImportChanged != .faunaMailExportChanged)
    #expect(Notification.Name.faunaMailImportChanged != .faunaMailSpamChanged)
}
#endif
