import Testing
import Foundation
@testable import FaunaKit

/// The appeal form's local state (moderation.md § Legal takedown — the transparency
/// triple's second leg). Dispatch needs a live nest and is covered by
/// `test_moderation_appeal.py`; the guard, wording and appealability are shared Rust.
/// These pin the three paint properties that are this VM's own: the draft captures
/// its row's `content_id` at open, cancel drops draft and status, and a server row —
/// not a local detection — offers the handle.

private let serverRow = QueueRow(contentId: "content-1", contentType: "post",
                                 category: "spam", confidencePerMille: 900, action: 1,
                                 timestamp: 0, source: .server)
private let otherRow = QueueRow(contentId: "content-2", contentType: "post",
                                category: "spam", confidencePerMille: 900, action: 1,
                                timestamp: 0, source: .server)
private let localRow = QueueRow(contentId: "content-3", contentType: "post",
                                category: "spam", confidencePerMille: 900, action: nil,
                                timestamp: 0, source: .local)

@Suite("ModerationQueueVM appeal form")
@MainActor
struct ModerationQueueAppealTests {
    @Test("only a nest-issued row is appealable")
    func serverRowOnly() {
        #expect(ModerationQueueVM.isAppealable(serverRow))
        #expect(!ModerationQueueVM.isAppealable(localRow))
    }

    @Test("open captures the row's content id, and the form blocks a reasonless submit")
    func openCapturesContentId() {
        let vm = ModerationQueueVM()
        vm.openAppeal(row: serverRow)
        #expect(vm.appealOpen)
        #expect(vm.appealContentId == "content-1")
        #expect(!vm.appealForm.canSubmit)
        vm.appealReason = "this was a mistake"
        #expect(vm.appealForm.canSubmit)
        // Opening on another row re-captures and clears the stale draft.
        vm.openAppeal(row: otherRow)
        #expect(vm.appealContentId == "content-2")
        #expect(vm.appealReason.isEmpty)
    }

    @Test("cancel drops the draft and the status")
    func cancelClosesTheForm() {
        let vm = ModerationQueueVM()
        vm.openAppeal(row: serverRow)
        vm.appealReason = "x"
        vm.cancelAppeal()
        #expect(!vm.appealOpen)
        #expect(vm.appealReason.isEmpty)
        #expect(vm.appealStatus == nil)
    }
}
