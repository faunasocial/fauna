import Foundation
import Testing
@testable import FaunaKit

// Pins `ProfileEditVM.open()` to the shared read-prove-record door
// (`APIClient.loadProfileEditBase`, itself `FfiProfileClient.loadEditBase`)
// rather than the plain `profileGet` read (`profile.md` § After an identity
// succession → the linkless bullet, mechanism 2). `APIClient` has no protocol
// seam; mirrors `BackupDestinationsVMTests.RecordingAPI` — subclass +
// override the one call under test, `@testable` lets the override stand.

@MainActor private final class CallLog {
    var calls: [String] = []
}

private final class RecordingAPI: APIClient {
    let log: CallLog
    var baseBody: Data?
    var throwsOnLoad = false

    init(log: CallLog, baseBody: Data? = nil) {
        self.log = log
        self.baseBody = baseBody
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func loadProfileEditBase() async throws -> Data? {
        await MainActor.run { log.calls.append("loadProfileEditBase") }
        if throwsOnLoad { throw APIError.ffiError("read failed") }
        return baseBody
    }

    // Never expected to be called by `open()` any more — throwing here (rather
    // than just logging) fails the test loudly if a future edit regresses back
    // to the plain read.
    override func profileGet(actorId: String) async throws -> Data {
        await MainActor.run { log.calls.append("profileGet") }
        throw APIError.ffiError("open() must not call profileGet directly")
    }
}

@Test @MainActor func profileEditVMOpenReadsBaseThroughSharedDoor() async {
    let log = CallLog()
    let api = RecordingAPI(log: log, baseBody: nil)
    let vm = ProfileEditVM()

    await vm.open(api: api, actorId: "self-actor")

    #expect(log.calls == ["loadProfileEditBase"])
    #expect(vm.isOpen)
}

@Test @MainActor func profileEditVMOpenStartsBlankOnFirstPublishNilBase() async {
    let log = CallLog()
    let api = RecordingAPI(log: log, baseBody: nil)
    let vm = ProfileEditVM()

    await vm.open(api: api, actorId: "self-actor")

    #expect(vm.displayName == "")
    #expect(vm.bio == "")
    #expect(vm.links.isEmpty)
    #expect(vm.isOpen)
}

@Test @MainActor func profileEditVMOpenStartsBlankOnReadFailure() async {
    let log = CallLog()
    let api = RecordingAPI(log: log)
    api.throwsOnLoad = true
    let vm = ProfileEditVM()

    await vm.open(api: api, actorId: "self-actor")

    #expect(log.calls == ["loadProfileEditBase"])
    #expect(vm.displayName == "")
    #expect(vm.isOpen)
}
