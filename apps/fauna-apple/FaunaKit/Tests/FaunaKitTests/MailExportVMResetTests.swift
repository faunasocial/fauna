import Foundation
import Testing
@testable import FaunaKit

#if !FAUNA_KIDS
// The export's Progress repaint loop must end on `reset()` even while the
// session is Paused — a Paused session keeps the wizard on Progress, so the step
// condition alone never ends the loop. The machine is a concrete UniFFI class, so
// the loop is driven through its `spawnTick` seam with a stand-in "still on
// Progress" predicate that stays true (the Paused case).

@Test @MainActor func resetEndsTheProgressTickWhileTheSessionStaysOnProgress() async throws {
    let vm = MailExportVM()
    var reads = 0
    vm.spawnTick(onProgress: { true }, read: { reads += 1 })
    #expect(vm.tickIsRunning)

    try await Task.sleep(for: .milliseconds(MailExportVM.progressTickMs * 3))
    #expect(reads > 0)

    vm.reset()
    #expect(!vm.tickIsRunning)
    let settled = reads
    try await Task.sleep(for: .milliseconds(MailExportVM.progressTickMs * 3))
    #expect(reads == settled)
}

// Every door that retires the export's drive loops, each
// pinned on its own: `reset()`, `configure` meeting a different `APIClient`, and the
// VM's release at the page's unmount. A loop is armed with a stand-in body that parks
// until cancelled; a REAL machine over a torn-down nest (`tornDownNest`) is seeded where
// one is needed, so nothing dials.

/// An `APIClient` whose export machine build is refused at once, so a `configure` on
/// it never hydrates (hydrating a machine over a torn-down nest waits out its
/// offline refresh). A machine is seeded through `adopt` instead.
private final class RefusingExportAPI: APIClient {
    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }
    override func mailExportMachine(handle: String) async throws -> MailExportMachine {
        throw ScopeTestFailure(account: "the incoming account")
    }
}

@MainActor
private func seededExportVM(from api: APIClient) async throws -> MailExportVM {
    let vm = MailExportVM()
    vm.adopt(buildMailExportMachine(nest: try await tornDownNest()), from: api)
    return vm
}

@Suite("MailExportVM's drive loops retire at every door", .timeLimit(.minutes(1)))
@MainActor
struct MailExportVMDriveLoopTests {
    @Test("reset() cancels every drive loop")
    func resetCancelsEveryLoop() async {
        let vm = MailExportVM()
        let witnesses = MailExportVM.Loop.allCases.map { armCancelWitness(vm.loops, $0) }
        vm.reset()
        for witness in witnesses { #expect(await sawCancel(witness)) }
    }

    @Test("configure on a different APIClient cancels the loops; the same one does not")
    func repointCancelsTheLoops() async throws {
        let first = RefusingExportAPI()
        let vm = try await seededExportVM(from: first)
        let run = armCancelWitness(vm.loops, .run)

        await vm.configure(api: first) { "" }
        #expect(vm.loops.isRunning(.run))

        await vm.configure(api: RefusingExportAPI()) { "" }
        #expect(await sawCancel(run))
    }

    @Test("releasing the VM cancels its loops")
    func releaseCancelsTheLoops() async {
        var vm: MailExportVM? = MailExportVM()
        let run = armCancelWitness(vm!.loops, .run)
        vm = nil
        #expect(await sawCancel(run))
    }

    @Test("the real drive bodies hold the VM weakly")
    func driveBodiesDoNotRetainTheVM() async throws {
        var vm: MailExportVM? = try await seededExportVM(from: RefusingExportAPI())
        vm!.driveExport()
        #expect(vm!.loops.isRunning(.run))
        weak var released = vm
        vm = nil
        #expect(released == nil)
    }
}

// A failed export paints Dismiss instead of Pause / Resume / Cancel; every
// other session state keeps the three controls (mail-export.md § A failed
// export is a record the wizard shows until the user moves on).
@Suite struct MailExportFailedRecordTests {
    @Test func onlyAnErroredSessionIsPaintedAsFailed() {
        #expect(MailExportVM.isFailedExport(.errored))
        for live: ExportSessionState? in [.running, .paused, .completed, .cancelled, nil] {
            #expect(!MailExportVM.isFailedExport(live))
        }
    }

    @Test func theExportPushIsItsOwnNotification() {
        #expect(Notification.Name.faunaMailExportChanged != .faunaMailSpamChanged)
    }
}
#endif
