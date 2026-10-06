import Testing
import FaunaKit
@testable import FaunaMacOSLib

// Unit tests for `SyncAgentHealthModel.poll()` — the local `sync-agent-status`
// tri-state display (`sync-agent.md` § Local agent health). The fake substitutes
// for the agent via the `AgentHealthChannel` seam (the macOS peer of
// `FakeLocationControlChannel`), so the state → display-string mapping is
// deterministic with no live agent, socket, or 10 s poll wait.

/// In-memory `AgentHealthChannel` fake — hands back a canned `FfiAgentStatus`
/// and a canned mass-delete-floor roster (the SAME tick folds both).
final class FakeAgentHealthChannel: AgentHealthChannel, @unchecked Sendable {
    enum FakeError: Error { case unreachable }

    var status: FfiAgentStatus
    var holds: [FfiEngineHold] = []
    /// Throw `.unreachable` on the next `listEngineHolds()` call when true —
    /// simulates an unreachable agent, whose honest fold is an empty roster
    /// (never a skip).
    var holdsUnreachable = false
    private(set) var callCount = 0
    private(set) var holdsCallCount = 0

    /// The agent's sync signal; `nil` = unreachable (the model then carries no
    /// sync leg).
    var syncSignal: FfiAgentSyncStatus?

    init(status: FfiAgentStatus) { self.status = status }

    func syncStatus() async -> FfiAgentSyncStatus? { syncSignal }

    func agentHealth(localBuildVersion: String) async -> FfiAgentStatus {
        callCount += 1
        return status
    }

    func listEngineHolds() async throws -> [FfiEngineHold] {
        holdsCallCount += 1
        if holdsUnreachable { throw FakeError.unreachable }
        return holds
    }

    /// The agent's binding rows (the park rides them). `nil` throws — an
    /// unreachable agent, whose honest fold is NOTHING (the rows keep their
    /// last-known park), unlike the hold's empty mirror.
    var locations: [FfiAgentLocation]? = []

    func listLocations() async throws -> [FfiAgentLocation] {
        guard let locations else { throw FakeError.unreachable }
        return locations
    }
}

final class LocationsCapture: @unchecked Sendable {
    var value: [FfiAgentLocation]?
}

/// The binding park rides the same tick (`file-sync.md` § Multi-writer shared
/// sets → *Revocation*): `poll()` hands `listLocations()`'s rows to
/// `onLocationsTick`, the only way a demoted writer's app learns the agent
/// parked its binding.
@Test @MainActor
func pollFoldsTheAgentsLocationsOntoTheParkHook() async {
    let model = SyncAgentHealthModel()
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .running, version: "1.4.2", uptimeSecs: 10))
    fake.locations = [FfiAgentLocation(
        path: "/shared", folder: "photos", folderId: "ref:photos", mode: "always",
        accessRevoked: true)]
    model.setChannelForTest(fake)

    let received = LocationsCapture()
    model.onLocationsTick = { rows in received.value = rows }

    await model.poll()

    #expect(received.value?.map(\.accessRevoked) == [true])
}

/// An unreachable agent folds no park at all — never an empty list that would
/// read as "nothing is parked any more".
@Test @MainActor
func pollSkipsTheParkFoldWhenListLocationsErrors() async {
    let model = SyncAgentHealthModel()
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .running, version: "1.4.2", uptimeSecs: 10))
    fake.locations = nil
    model.setChannelForTest(fake)

    let received = LocationsCapture()
    model.onLocationsTick = { rows in received.value = rows }

    await model.poll()

    #expect(received.value == nil)
}

/// Reference-type capture for a `@Sendable ... async -> Void` hook's result —
/// mutating a captured `var` directly inside such a closure is a Swift 6
/// concurrency error; a class held `let` and mutated through a property is
/// not, matching how the fakes above hold their own mutable state.
final class HoldsCapture: @unchecked Sendable {
    var value: [FfiEngineHold]?
}

@Test @MainActor
func pollRunningShowsVersionAndUptime() async {
    let model = SyncAgentHealthModel()
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .running, version: "1.4.2", uptimeSecs: 3_661))
    model.setChannelForTest(fake)

    await model.poll()

    #expect(model.stateText == L.status.syncAgent.running)
    #expect(model.versionText == "1.4.2")
    #expect(!model.uptimeText.isEmpty)
    #expect(fake.callCount == 1)
}

/// The agent's own *Keys pending* (its status reply's field) reads "Keys pending"
/// (sync-agent.md § Local agent health), with the answering agent's children.
@Test @MainActor
func pollKeysPendingReadsKeysPendingAndShowsVersionAndUptime() async {
    let model = SyncAgentHealthModel()
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .keysPending, version: "1.4.2", uptimeSecs: 42))
    model.setChannelForTest(fake)

    await model.poll()

    #expect(model.stateText == L.status.syncAgent.keysPending)
    #expect(model.versionText == "1.4.2")
    #expect(!model.uptimeText.isEmpty)
}

/// The agent's own refused-renewal report reads "Not enrolled"
/// (sync-agent.md § Local agent health), with the answering agent's children.
@Test @MainActor
func pollNotEnrolledReadsNotEnrolledAndShowsVersionAndUptime() async {
    let model = SyncAgentHealthModel()
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .notEnrolled, version: "1.4.2", uptimeSecs: 42))
    model.setChannelForTest(fake)

    await model.poll()

    #expect(model.stateText == L.status.syncAgent.notEnrolled)
    #expect(model.versionText == "1.4.2")
    #expect(!model.uptimeText.isEmpty)
}

@Test @MainActor
func pollRestartPendingStillShowsVersionAndUptime() async {
    let model = SyncAgentHealthModel()
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .restartPending, version: "1.4.1", uptimeSecs: 42))
    model.setChannelForTest(fake)

    await model.poll()

    #expect(model.stateText == L.status.syncAgent.restartPending)
    #expect(model.versionText == "1.4.1")
    #expect(!model.uptimeText.isEmpty)
}

@Test @MainActor
func pollNotRunningBlanksVersionAndUptime() async {
    let model = SyncAgentHealthModel()
    // A failed `GetServiceStatus` call maps to `.notRunning` with empty
    // version/`0` uptime (`FfiSyncAgentProvisioner.agent_health`'s contract).
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .notRunning, version: "", uptimeSecs: 0))
    model.setChannelForTest(fake)

    await model.poll()

    #expect(model.stateText == L.status.syncAgent.notRunning)
    #expect(model.versionText.isEmpty)
    #expect(model.uptimeText.isEmpty)
}

@Test @MainActor
func stopResetsToNotRunningAndDropsTheChannel() async {
    let model = SyncAgentHealthModel()
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .running, version: "1.4.2", uptimeSecs: 100))
    model.setChannelForTest(fake)
    await model.poll()
    #expect(model.stateText == L.status.syncAgent.running)

    model.stop()

    #expect(model.stateText == L.status.syncAgent.notRunning)
    #expect(model.versionText.isEmpty)
    #expect(model.uptimeText.isEmpty)

    // A poll after stop() is a no-op (no channel) — must not crash or call the
    // fake again.
    await model.poll()
    #expect(fake.callCount == 1)
}

/// Row 123 — the mass-delete floor's per-set hold rides the SAME 10 s tick as
/// health, never a separate hook: `poll()` calls `listEngineHolds()` and hands
/// the roster to `onEngineHoldsTick`, alongside the ordinary health read.
@Test @MainActor
func pollFoldsEngineHoldsOntoTheTickHook() async {
    let model = SyncAgentHealthModel()
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .running, version: "1.4.2", uptimeSecs: 10))
    fake.holds = [FfiEngineHold(folder: "docs", folderId: "local:1", deletesHeld: 3)]
    model.setChannelForTest(fake)

    let received = HoldsCapture()
    model.onEngineHoldsTick = { holds in received.value = holds }

    await model.poll()

    #expect(fake.holdsCallCount == 1)
    #expect(received.value?.map(\.folder) == ["docs"])
    #expect(received.value?.map(\.deletesHeld) == [3])
}

/// An unreachable/too-old agent's `listEngineHolds()` errors — the honest fold
/// for that is an EMPTY roster (a mirror, never a skip), so a hold that was
/// showing retracts rather than sticking at its last-known count.
@Test @MainActor
func pollFoldsAnEmptyRosterWhenListEngineHoldsErrors() async {
    let model = SyncAgentHealthModel()
    let fake = FakeAgentHealthChannel(
        status: FfiAgentStatus(state: .running, version: "1.4.2", uptimeSecs: 10))
    fake.holdsUnreachable = true
    model.setChannelForTest(fake)

    let received = HoldsCapture()
    model.onEngineHoldsTick = { holds in received.value = holds }

    await model.poll()

    #expect(received.value?.isEmpty == true)
}
