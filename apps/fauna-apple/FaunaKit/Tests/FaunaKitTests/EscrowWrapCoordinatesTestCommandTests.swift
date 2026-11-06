import Testing
import Foundation
@testable import FaunaKit

// Non-e2e pins for the shared `escrow_wrap_coordinates` TestAgent command
// (`EscrowWrapCoordinatesTestCommand`). The real read is the recovery-kit let-go
// witness's (`test_recovery_kit_let_go.py`), which stages its dead generation
// from this answer: a dropped command would leave it nothing to drop.
#if DEBUG

private final class RecordingEscrowAPI: APIClient {
    var answer: String?
    private(set) var asks = 0

    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func escrowWrapCoordinatesJson() async -> String? {
        asks += 1
        return answer
    }
}

@MainActor
@Suite("escrow_wrap_coordinates test-agent command")
struct EscrowWrapCoordinatesTestCommandTests {
    @Test func theReadersAnswerPassesThroughUntouched() async {
        let api = RecordingEscrowAPI()
        api.answer = #"{"found":true,"rows":[{"plane":"account","item_key":"ab"}]}"#

        let outcome = await EscrowWrapCoordinatesTestCommand.apply(api: api)

        #expect(outcome == .report(api.answer!))
        #expect(api.asks == 1)
    }

    @Test func noConnectedClientAnswersAQuietNotFoundReport() async {
        let outcome = await EscrowWrapCoordinatesTestCommand.apply(api: nil)

        #expect(outcome == .report(EscrowWrapCoordinatesTestCommand.notFoundJSON))
    }

    @Test func aClientWithNoRuntimeAnswersNotFoundRatherThanNothing() async {
        let api = RecordingEscrowAPI()
        api.answer = nil

        let outcome = await EscrowWrapCoordinatesTestCommand.apply(api: api)

        #expect(outcome == .report(EscrowWrapCoordinatesTestCommand.notFoundJSON))
        #expect(api.asks == 1)
    }
}

#endif
