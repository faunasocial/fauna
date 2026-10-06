import Foundation
import Testing
@testable import FaunaKit

// `PushManager` over the shared registration machine
// (`docs/goal/architecture/apps/common.md` § Push Notifications →
// *Registration*): which of the machine's verbs each Apple-side gesture
// issues, and that what the toggle renders is the stored bit. The machine's
// own rules (a re-arm never opts in, a drop keeps the bit, a Disable clears it
// first) are pinned in Rust, in `fauna_client_push::registration`'s tests;
// these pin the Swift wiring onto it.
//
// Level: `PushManager` over a recording `APIClient` double whose registration
// stands in for the nest — no notification centre, no live connection. The
// intent record is the real shared file, at a per-test temporary path, so no
// test writes to the host's real Application Support (`e2e-conventions.md`
// point 10's isolation rule, one tier down).

private struct StubNestError: Error {}

/// Stands in for `FfiPushRegistration`: records the verbs, and writes the
/// stored bit the way the shared machine does on a success.
private final class RecordingRegistration: PushRegistering, @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [String] = []
    let intentPath: String
    var fails = false

    init(intentPath: String) { self.intentPath = intentPath }

    var calls: [String] { lock.withLock { recorded } }
    private func record(_ call: String) { lock.withLock { recorded.append(call) } }

    func enable(subscription: FfiPushEndpoint) async throws {
        record("enable:\(subscription.transport):\(subscription.endpoint)")
        if fails { throw StubNestError() }
        try pushSeedOptIn(intentPath: intentPath)
    }

    func rearm(subscription: FfiPushEndpoint) async throws -> Bool {
        record("rearm:\(subscription.transport):\(subscription.endpoint)")
        if fails { throw StubNestError() }
        return pushIntent(intentPath: intentPath).optedIn
    }

    func disable() async throws {
        record("disable")
        try pushClearOptIn(intentPath: intentPath)
        if fails { throw StubNestError() }
    }

    func dropActorRow() async throws {
        record("drop")
        if fails { throw StubNestError() }
    }

    func announcePresence() async { record("announce") }
}

private final class RecordingPushAPI: APIClient {
    let registration: RecordingRegistration
    /// The connection itself cannot be reached (offline at the gesture).
    var unreachable = false
    private(set) var requestedDeviceIds: [String] = []

    init(intentPath: String) {
        registration = RecordingRegistration(intentPath: intentPath)
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func pushRegistration(
        deviceId: String, intentPath: String
    ) async throws -> any PushRegistering {
        requestedDeviceIds.append(deviceId)
        if unreachable { throw StubNestError() }
        return registration
    }
}

private func freshStore() -> PushIntentStore {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("fauna-push-manager-tests-\(UUID().uuidString)")
    return PushIntentStore(path: dir.appendingPathComponent(PushIntentStore.fileName).path)
}

private let apns = FfiPushEndpoint(
    transport: "apns", endpoint: "a1b2", keyP256dh: "pk", keyAuth: "auth")

@Suite struct PushManagerDropActorRowTests {
    @Test @MainActor func aLeaveDropIssuesTheMachinesDropAndKeepsTheBit() async throws {
        let store = freshStore()
        try pushSeedOptIn(intentPath: store.path)
        let api = RecordingPushAPI(intentPath: store.path)
        let manager = PushManager(api: api, deviceId: "device-1", intent: store,
                                  hostAvailable: false)

        await manager.dropActorRow()

        #expect(api.registration.calls == ["drop"])
        #expect(api.requestedDeviceIds == ["device-1"])
        // The install intent bit survives every leave-shape — only the user's
        // own switch-off clears it.
        #expect(store.isOptedIn)
        #expect(manager.isOptedIn)
    }

    @Test @MainActor func aLeaveDropSwallowsAFailure() async {
        let store = freshStore()
        let api = RecordingPushAPI(intentPath: store.path)
        api.registration.fails = true
        let manager = PushManager(api: api, deviceId: "device-1", intent: store,
                                  hostAvailable: false)

        // Must not throw or crash — a leave gesture completes offline; the
        // stranded row is left for the ruling's three reapers.
        await manager.dropActorRow()
        api.unreachable = true
        await manager.dropActorRow()

        #expect(api.registration.calls == ["drop"])
    }

    @Test @MainActor func theToggleOnAHostThatCannotRegisterSettlesBackOffWithTheInlineLine() async {
        // The bare debug binary (no notification centre) and — until the
        // `aps-environment` entitlement exists — every build: the toggle
        // renders, the enable fails, the bit stays clear, the line says why.
        let store = freshStore()
        let api = RecordingPushAPI(intentPath: store.path)
        let manager = PushManager(api: api, deviceId: "device-1", intent: store,
                                  hostAvailable: false)

        await manager.setOptIn(true)

        #expect(api.registration.calls.isEmpty)
        #expect(manager.isOptedIn == false)
        #expect(store.isOptedIn == false)
        #expect(manager.lastError == L.status.notifications.unavailable)
        #expect(manager.isWorking == false)
        let state = PushSectionState.resolve(
            optedIn: manager.isOptedIn, permission: manager.permission,
            lastError: manager.lastError)
        #expect(state.isOn == false)
        #expect(state.error == L.status.notifications.unavailable)
    }

    @Test @MainActor func aLaunchTokenOnlyRearmsAndNeverOptsTheInstallIn() async {
        // No Enable is pending, so the token callback's subscription goes to
        // `rearm` — which the shared machine answers with nothing while the
        // bit is clear.
        let store = freshStore()
        let api = RecordingPushAPI(intentPath: store.path)
        let manager = PushManager(api: api, deviceId: "device-1", intent: store,
                                  hostAvailable: false)

        await manager.register(apns)

        #expect(api.registration.calls == ["rearm:apns:a1b2"])
        #expect(manager.isOptedIn == false)
    }

    @Test @MainActor func aFailedRegistrationLeavesTheToggleOffAndSaysWhy() async {
        let store = freshStore()
        let api = RecordingPushAPI(intentPath: store.path)
        api.registration.fails = true
        let manager = PushManager(api: api, deviceId: "device-1", intent: store,
                                  hostAvailable: false)

        await manager.register(apns)

        #expect(manager.isOptedIn == false)
        #expect(manager.lastError?.isEmpty == false)
    }

    @Test @MainActor func switchingOffClearsTheBitEvenWhenTheNestCannotBeReached() async throws {
        let store = freshStore()
        try pushSeedOptIn(intentPath: store.path)
        let api = RecordingPushAPI(intentPath: store.path)
        api.unreachable = true
        let manager = PushManager(api: api, deviceId: "device-1", intent: store,
                                  hostAvailable: false)
        #expect(manager.isOptedIn)

        await manager.setOptIn(false)

        // Off stays off: no connection to build the machine over, and the bit
        // is clear all the same.
        #expect(api.registration.calls.isEmpty)
        #expect(store.isOptedIn == false)
        #expect(manager.isOptedIn == false)
        #expect(manager.lastError?.isEmpty == false)
    }

    @Test @MainActor func switchingOffIssuesTheMachinesDisable() async throws {
        let store = freshStore()
        try pushSeedOptIn(intentPath: store.path)
        let api = RecordingPushAPI(intentPath: store.path)
        let manager = PushManager(api: api, deviceId: "device-1", intent: store,
                                  hostAvailable: false)

        await manager.setOptIn(false)

        #expect(api.registration.calls == ["disable"])
        #expect(manager.isOptedIn == false)
        #expect(manager.lastError == nil)
    }

    @Test @MainActor func sessionStartRereadsTheStoredBitAndAnnouncesTheRowsDeviceId() async throws {
        let store = freshStore()
        let api = RecordingPushAPI(intentPath: store.path)
        let manager = PushManager(api: api, deviceId: "device-1", intent: store,
                                  hostAvailable: false)
        #expect(manager.isOptedIn == false)
        // The record is the source, not the value the manager was built
        // with: a session start renders whatever it holds now.
        try pushSeedOptIn(intentPath: store.path)

        await manager.onSessionStart()

        // The toggle renders the record as stored now; the announce went out
        // over the registration built for the same device id the rows are
        // keyed under.
        #expect(manager.isOptedIn)
        #expect(api.registration.calls == ["announce"])
        #expect(api.requestedDeviceIds == ["device-1"])
    }
}
