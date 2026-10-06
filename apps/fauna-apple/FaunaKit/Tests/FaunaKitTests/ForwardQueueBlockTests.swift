import Testing
@testable import FaunaKit

// The Nests page forward-queue block (`docs/goal/ui/nests.md` § Forward queue).
// Pins the two shell-side rules the shared projection cannot: the block paints
// nothing unless something is queued, and the relay-chosen reason reaches the
// screen as the exact string it was given — no markdown, no link. The app-level
// witness is `tests/e2e-unified/tests/test_forward_queue.py` (macos, ios).

@Suite struct ForwardQueueBlockTests {
    private func status(queued: UInt64, stuck: UInt64 = 0, lastError: String? = nil)
        -> ForwardQueueStatus
    {
        ForwardQueueStatus(queued: queued, stuck: stuck, lastError: lastError)
    }

    @Test func aZeroQueuePaintsNoBlock() {
        #expect(!ForwardQueueBlockView.isVisible(status(queued: 0)))
        #expect(!ForwardQueueBlockView.isVisible(status(queued: 0, stuck: 0, lastError: "x")))
    }

    @Test func aNonEmptyQueuePaintsTheBlock() {
        #expect(ForwardQueueBlockView.isVisible(status(queued: 1)))
    }

    @Test func theReasonIsRenderedVerbatimNotParsed() {
        let hostile = "\u{1B}[31m<b>x</b>**bold** https://e.x"
        let text = ForwardQueueBlockView.reasonText(of: status(queued: 2, lastError: hostile))
        #expect(text == L.nests.forwardQueueLastError(error: hostile))
        #expect(text?.contains("**bold** https://e.x") == true)
    }

    @Test func noRecordedReasonMeansNoReasonElement() {
        #expect(ForwardQueueBlockView.reasonText(of: status(queued: 2)) == nil)
        #expect(ForwardQueueBlockView.reasonText(of: status(queued: 2, lastError: "")) == nil)
    }

    @Test func theStuckClauseIsAppendedOnlyWhenSomethingIsStuck() {
        let plain = ForwardQueueBlockView.summaryText(of: status(queued: 3))
        #expect(plain == L.nests.forwardQueueSummary(count: "3"))
        let stuck = ForwardQueueBlockView.summaryText(of: status(queued: 3, stuck: 2))
        #expect(stuck == "\(plain) \(L.nests.forwardQueueStuck(count: "2"))")
    }
}
