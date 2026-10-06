import Foundation
import Testing
@testable import FaunaKit

// The apple leg of "one `DevicesMachine` per signed-in session" (`docs/goal/ui/
// folders.md` § Implementation status today, the network-fault bullet): the
// followed-folders source remembers the last rows it read, so a machine built
// per page visit answers a revisit during a dropped connection with no rows,
// and the refresh barrier (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`) resets
// under any baseline the previous visit took. apple held one
// `DevicesMachineVM` per view; it is now ONE app-scene VM shared by
// `DevicesView`, `MacFoldersView` and the iOS `FoldersView` — web's
// `devices-session.ts` twin — dropped by `ActorScope.dropAppOwnedState`.

private struct StubBuildError: Error {}

/// Counts machine builds and holds each one open until released, so two
/// configures can genuinely overlap. Fails the build offline (no nest), which
/// is enough: what is pinned is how many builds START.
private final class CountingDevicesAPI: APIClient {
    var builds = 0
    var notice: String?
    private var gate: CheckedContinuation<Void, Never>?

    init(notice: String? = nil) {
        self.notice = notice
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func devicesMachine(observer: DevicesObserver) async throws -> DevicesMachine {
        builds += 1
        await withCheckedContinuation { gate = $0 }
        throw StubBuildError()
    }

    override func accountEnrollmentNotice() async -> String? { notice }

    /// The boundary sentence a follow fails with, as the shared Rust hands it.
    var followRefusal = "No public folder by that name for that person. Check the handle and the folder name."

    override func followPublicFolder(owner: String, folderName: String) async throws
        -> [FfiFollowedFolder]
    {
        throw FfiError.General(msg: followRefusal)
    }

    func releaseBuild() {
        gate?.resume()
        gate = nil
    }

    var buildIsWaiting: Bool { gate != nil }
}

@MainActor private func waitUntil(_ condition: () -> Bool) async {
    for _ in 0..<1000 where !condition() { await Task.yield() }
}

/// Two views appearing at once (iOS can hold Devices and Folders in one stack)
/// share ONE build — never two machines, the second of which would own the
/// followed-folders memory the first never sees.
@Test @MainActor func concurrentConfiguresShareOneBuild() async {
    let api = CountingDevicesAPI()
    let vm = DevicesMachineVM()

    async let first: Void = vm.configure(api: api)
    await waitUntil { api.buildIsWaiting }
    async let second: Void = vm.configure(api: api)
    for _ in 0..<50 { await Task.yield() }
    api.releaseBuild()
    _ = await (first, second)

    #expect(api.builds == 1, "a configure that overlaps an in-flight build must join it, not start a second")
}

/// A configure against a DIFFERENT session's `APIClient` (a re-claim the
/// canonical drop somehow missed) never keeps the outgoing session's state.
@Test @MainActor func configureWithANewSessionsApiDropsTheOldSessionsState() async {
    let old = CountingDevicesAPI(notice: "the outgoing session's notice")
    let vm = DevicesMachineVM()
    async let configured: Void = vm.configure(api: old)
    await waitUntil { old.buildIsWaiting }
    old.releaseBuild()
    await configured
    // The stub build fails, so the first load never ran — load the notice
    // directly (the `api` is set either way).
    await vm.loadEnrollmentNotice()
    #expect(vm.enrollmentNotice == "the outgoing session's notice")
    vm.offlineSharePeerCodeInput = "the outgoing session's draft"

    let new = CountingDevicesAPI(notice: nil)
    async let reconfigured: Void = vm.configure(api: new)
    await waitUntil { new.buildIsWaiting }
    #expect(vm.enrollmentNotice == nil)
    #expect(vm.offlineSharePeerCodeInput.isEmpty)
    new.releaseBuild()
    await reconfigured
    #expect(new.builds == 1, "the new session builds its own machine")
}

/// A gesture's error is the answer to THAT visit's gesture. With one VM per
/// session it no longer dies with the view, so a visit clears it — else the
/// last visit's refusal greets the next one, and an answer read after a new
/// follow may be a leftover (the macOS follow witness caught exactly that).
/// Also pins that the refusal is the boundary's own sentence, never the
/// `General(msg: …)` debug shape.
@Test @MainActor func aRevisitClearsTheLastVisitsGestureError() async {
    let api = CountingDevicesAPI()
    let vm = DevicesMachineVM()
    async let configured: Void = vm.configure(api: api)
    await waitUntil { api.buildIsWaiting }
    api.releaseBuild()
    await configured

    let followed = await vm.followFolder(owner: "someone", folderName: "misspelt")
    #expect(followed == false)
    #expect(vm.sharingError == api.followRefusal)

    async let revisit: Void = vm.configure(api: api)
    await waitUntil { api.buildIsWaiting }
    #expect(vm.sharingError == nil, "a revisit must not show the last visit's refusal")
    api.releaseBuild()
    await revisit
}

/// `reset()` returns the VM to its never-configured state.
@Test @MainActor func resetDropsEverySessionField() async {
    let api = CountingDevicesAPI(notice: "a notice")
    let vm = DevicesMachineVM()
    async let configured: Void = vm.configure(api: api)
    await waitUntil { api.buildIsWaiting }
    api.releaseBuild()
    await configured
    vm.offlineSharePeerCodeInput = "draft"
    #expect(vm.errorMessage != nil, "sanity: the failed build reads as a connect error")

    vm.reset()

    #expect(vm.machine == nil)
    #expect(vm.errorMessage == nil)
    #expect(vm.enrollmentNotice == nil)
    #expect(vm.offlineSharePeerCodeInput.isEmpty)
    #expect(vm.offlineShareView == nil, "no api survives the reset")
}

/// The refresh barrier's key is published from the first read on: the zero
/// triple before a machine exists (the legitimate "none yet"), never absent —
/// an absent key tells the witness this app has no leg at all.
@Test @MainActor func devicesRefreshesReadsZerosBeforeTheMachineExists() {
    let state = AppStateObservables.commonState(
        session: SessionState(),
        isAdmin: false,
        liveClient: nil,
        conversationsSession: nil,
        feedManager: nil,
        devicesMachine: nil,
        inboxMode: nil
    )
    let refreshes = state["devices_refreshes"] as? [String: Any]
    #expect(refreshes?["started"] as? Int == 0)
    #expect(refreshes?["completed"] as? Int == 0)
    #expect(refreshes?["committed_gen"] as? Int == 0)
}
