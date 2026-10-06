import Testing
import Foundation
@testable import FaunaKit

// Non-e2e pins for the shared `device_set_state` TestAgent command
// (`DeviceSetStateTestCommand`) — the three answers a shell can get wrong
// without a nest in the loop. The real read (a `Removed` row landing after a
// kill between the two removal legs) is the e2e's:
// `test_crash_recovery_journeys.py::
// test_kill_client_between_the_removal_legs_reconcile_finishes_it`.
//
// Why these need their own pin: the journey's `_require_device_set_reader`
// treats ANY non-`nil` answer as "the reader is built", and its assertions then
// read `found`/`state` off the JSON. So a shell that dropped the command (`nil`)
// silently turns the journey into a skip, and one that folded a malformed
// request into "not found" lets a "the row is NOT there" assertion pass on a
// typo'd key — neither is a crash, both read as green.
#if DEBUG

/// An `APIClient` double that records what the shell asked and answers a canned
/// reader value — the `RecordingEnrollmentAPI` pattern
/// (`DevicesEnrollmentNoticeTests`).
private final class RecordingDeviceSetAPI: APIClient {
    var answer: String?
    private(set) var asked: [String] = []

    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func deviceSetStateJson(deviceIdHex: String) async -> String? {
        asked.append(deviceIdHex)
        return answer
    }
}

@MainActor
@Suite("device_set_state test-agent command")
struct DeviceSetStateTestCommandTests {
    private let someDevice = String(repeating: "ab", count: 32)

    /// The shared reader's own found-shape for a `Removed` row
    /// (`fauna_client_account_runtime::device_set_state_json`) — handed back
    /// byte-for-byte, so the shell never re-encodes (and never reorders or drops)
    /// a field the journey reads.
    @Test func theReadersAnswerAndTheAskedIdPassThroughUntouched() async {
        let api = RecordingDeviceSetAPI()
        api.answer = #"{"found":true,"removed_at_ms":1,"removed_by":"cd","state":"removed"}"#

        let outcome = await DeviceSetStateTestCommand.apply(
            ["device_id_hex": someDevice], api: api)

        #expect(outcome == .report(api.answer!))
        #expect(api.asked == [someDevice])
    }

    /// No live client is the ordinary pre-auth state, and tui/linux/android all
    /// answer it as a quiet "not found" — a *report*, not a refusal, because the
    /// probe must still see a built reader (see the header).
    @Test func noConnectedClientAnswersAQuietNotFoundReport() async {
        let outcome = await DeviceSetStateTestCommand.apply(
            ["device_id_hex": someDevice], api: nil)

        #expect(outcome == .report(DeviceSetStateTestCommand.notFoundJSON))
    }

    /// A client that exists but has no `FfiNestClient` yet (`APIClient` answers
    /// `nil` when `nestClient` is unset) is the same quiet "not found".
    @Test func aClientWithNoRuntimeToAskAnswersTheSameQuietNotFound() async {
        let api = RecordingDeviceSetAPI()
        api.answer = nil

        let outcome = await DeviceSetStateTestCommand.apply(
            ["device_id_hex": someDevice], api: api)

        #expect(outcome == .report(DeviceSetStateTestCommand.notFoundJSON))
        #expect(api.asked == [someDevice])
    }

    /// The fallback is the JSON object the probe and the journey parse — pinned as
    /// data, so a reword of the constant cannot make `found` read as absent.
    @Test func theNotFoundFallbackIsAJSONObjectWithFoundFalse() throws {
        let object = try JSONSerialization.jsonObject(
            with: Data(DeviceSetStateTestCommand.notFoundJSON.utf8))
        let body = try #require(object as? [String: Any])
        #expect(body["found"] as? Bool == false)
    }

    /// Convention 11's bad-payload clause: a missing or mistyped id is LOUD, never
    /// folded into `notFoundJSON`, and never reaches the reader.
    @Test func aMissingOrMistypedDeviceIdIsARefusalThatNeverReachesTheReader() async {
        let api = RecordingDeviceSetAPI()
        api.answer = #"{"found":true,"state":"enrolled"}"#

        let missing = await DeviceSetStateTestCommand.apply([:], api: api)
        let mistyped = await DeviceSetStateTestCommand.apply(["device_id_hex": 7], api: api)

        for outcome in [missing, mistyped] {
            guard case .refused(let reason) = outcome else {
                Issue.record("expected a refusal, got \(outcome)")
                continue
            }
            #expect(reason.contains("device_id_hex"))
        }
        #expect(api.asked.isEmpty)
    }
}

#endif
