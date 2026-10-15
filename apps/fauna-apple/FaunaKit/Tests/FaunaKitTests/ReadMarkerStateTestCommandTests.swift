import Testing
import Foundation
@testable import FaunaKit

// Non-e2e pins for the shared `read_marker_state` TestAgent command
// (`ReadMarkerStateTestCommand`) — the answers a shell can get wrong without a
// nest in the loop. The real read is the read-state relaunch witness's
// (`test_conversations_read_state_across_restart.py`), which treats ANY non-`nil`
// answer as "the reader is built": a dropped command silently turns the witness
// into a skip, a malformed request folded into "not found" lets an "unread"
// assertion pass on a typo'd key.
#if DEBUG

private final class RecordingReadMarkerAPI: APIClient {
    var answer: String?
    private(set) var asked: [String] = []

    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func readMarkerStateJson(channelIdHex: String) async -> String? {
        asked.append(channelIdHex)
        return answer
    }
}

@MainActor
@Suite("read_marker_state test-agent command")
struct ReadMarkerStateTestCommandTests {
    private let someChannel = String(repeating: "ab", count: 16)

    @Test func theReadersAnswerAndTheAskedIdPassThroughUntouched() async {
        let api = RecordingReadMarkerAPI()
        api.answer = #"{"found":true,"through":7}"#

        let outcome = await ReadMarkerStateTestCommand.apply(
            ["channel_id_hex": someChannel], api: api)

        #expect(outcome == .report(api.answer!))
        #expect(api.asked == [someChannel])
    }

    @Test func noConnectedClientAnswersAQuietNotFoundReport() async {
        let outcome = await ReadMarkerStateTestCommand.apply(
            ["channel_id_hex": someChannel], api: nil)

        #expect(outcome == .report(ReadMarkerStateTestCommand.notFoundJSON))
    }

    @Test func aMissingOrMistypedChannelIdIsARefusalThatNeverReachesTheReader() async {
        let api = RecordingReadMarkerAPI()
        api.answer = #"{"found":true,"through":1}"#

        let missing = await ReadMarkerStateTestCommand.apply([:], api: api)
        let mistyped = await ReadMarkerStateTestCommand.apply(["channel_id_hex": 7], api: api)

        for outcome in [missing, mistyped] {
            guard case .refused(let reason) = outcome else {
                Issue.record("expected a refusal, got \(outcome)")
                continue
            }
            #expect(reason.contains("channel_id_hex"))
        }
        #expect(api.asked.isEmpty)
    }
}

#endif
