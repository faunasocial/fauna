import Testing
import Foundation
@testable import FaunaKit

// Non-e2e pins for the shared `folder_set_custody` TestAgent command
// (`FolderSetCustodyTestCommand`). The real read is the succession witnesses'
// (`test_identity_succession_shared_folder_rekey.py`), which compare the
// custody of three seats: a dropped command or a swallowed refusal would
// leave them nothing to compare.
#if DEBUG

private struct RefusedSelector: Error, CustomStringConvertible {
    var description: String { "a channel that is not 32 bytes of hex" }
}

private final class RecordingCustodyAPI: APIClient {
    var answer: String?
    var refuse = false
    private(set) var asks: [(String?, String?)] = []

    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func folderSetCustodyJson(setName: String?, channelIdHex: String?) async throws -> String? {
        asks.append((setName, channelIdHex))
        if refuse { throw RefusedSelector() }
        return answer
    }
}

@MainActor
@Suite("folder_set_custody test-agent command")
struct FolderSetCustodyTestCommandTests {
    @Test func theSelectorsReachTheReaderAndItsAnswerPassesThrough() async {
        let api = RecordingCustodyAPI()
        api.answer = #"{"found":true,"channel_id_hex":"ab","generations":[{"version":2,"key_fp":"cd"}]}"#

        let outcome = await FolderSetCustodyTestCommand.apply(
            ["set_name": "Photos", "channel_id_hex": "ab"], api: api
        )

        #expect(outcome == .report(api.answer!))
        #expect(api.asks.count == 1)
        #expect(api.asks.first?.0 == "Photos")
        #expect(api.asks.first?.1 == "ab")
    }

    @Test func aSelectorSharedRustRefusesIsALoudRefusal() async {
        let api = RecordingCustodyAPI()
        api.refuse = true

        let outcome = await FolderSetCustodyTestCommand.apply(["channel_id_hex": "zz"], api: api)

        guard case .refused(let reason) = outcome else {
            Issue.record("a refused selector must be loud, got \(outcome)")
            return
        }
        #expect(reason.contains("folder_set_custody"))
    }

    @Test func noConnectedClientAnswersAQuietNotFoundReport() async {
        let outcome = await FolderSetCustodyTestCommand.apply(["set_name": "Photos"], api: nil)

        #expect(outcome == .report(FolderSetCustodyTestCommand.notFoundJSON))
    }
}

#endif
