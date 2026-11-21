import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pins for the gates tui built first and the apple apps lifted:
// the Logs pages' copy/clear (`observability.md` § Surfaces, *Copy and clear
// offer only what they can do*), the copy outcome every painted copy records
// (`ui/README.md` § Offer only what is possible, rule 7 — a failure is never
// silent), and the four controls that now paint from the shared `Possibility`
// instead of an app-read bool (rule 6): the reply bar, the rule editor's Save,
// the offline-share Begin/Expect and the Media `share-link-button`. Each
// answer comes from shared Rust over FFI, so these also pin that the bindings
// reach the apple apps; how a painted control LOOKS is the last inch.

private struct CopyRefused: Error, LocalizedError {
    var errorDescription: String? { "no pasteboard" }
}

private let entry = LogEntry(timestampMs: 1_700_000_000_000, level: .info,
                             target: "fauna.test", message: "hello")

@Suite("Logs copy and clear offer only what they can do")
@MainActor
struct LogsRingActionTests {
    @Test("an empty view blocks copy and clear with the page's own empty line")
    func emptyRingBlocks() {
        let device = LogsFormat.ringActionPossibility(shown: 0, admin: false)
        #expect(device.isShown && !device.isPossible)
        #expect(device.reasonText == LogsFormat.emptyText(admin: false))
        #expect(device.reasonText == L.logs.empty)

        let admin = LogsFormat.ringActionPossibility(shown: 0, admin: true)
        #expect(!admin.isPossible)
        #expect(admin.reasonText == L.admin.logsPage.empty)
    }

    @Test("a view with entries offers both")
    func entriesArePossible() {
        #expect(LogsFormat.ringActionPossibility(shown: 3, admin: false).isPossible)
        #expect(LogsFormat.ringActionPossibility(shown: 1, admin: true).isPossible)
    }

    @Test("the copy press on an empty view refuses with the reason and copies nothing")
    func emptyCopyRefuses() {
        var copied: String? = "stale"
        var failure: String?
        var wrote = false
        LogsFormat.copy([], answer: LogsFormat.ringActionPossibility(shown: 0, admin: false),
                        copied: &copied, failure: &failure, writer: { _ in wrote = true })
        #expect(!wrote)
        #expect(copied == nil)
        #expect(failure == L.logs.empty)
    }

    @Test("a copy that reached the pasteboard confirms with the exact text and clears a failure")
    func copyOkConfirms() {
        var copied: String?
        var failure: String? = "an earlier failure"
        var written: String?
        LogsFormat.copy([entry], answer: LogsFormat.ringActionPossibility(shown: 1, admin: false),
                        copied: &copied, failure: &failure, writer: { written = $0 })
        #expect(written == LogsFormat.renderedText([entry]))
        #expect(copied == written, "the `copied` attr carries what went to the pasteboard")
        #expect(failure == nil)
    }

    @Test("a failed copy says common.copy_failed on error-message, nothing confirmed")
    func copyFailSurfaces() {
        var copied: String? = "stale"
        var failure: String?
        LogsFormat.copy([entry], answer: LogsFormat.ringActionPossibility(shown: 1, admin: true),
                        copied: &copied, failure: &failure, writer: { _ in throw CopyRefused() })
        #expect(copied == nil)
        #expect(failure == L.common.copyFailed(error: "no pasteboard"))
    }
}

@Suite("The invite code's copy confirms or says why not")
@MainActor
struct InviteCodeCopyTests {
    @Test("a copy that reached the pasteboard confirms and leaves the action error alone")
    func copyOk() {
        let outcome = CopyOutcome.attempt("INVITE-123", writer: { _ in })
        #expect(outcome == .copied("INVITE-123"))
        #expect(outcome.copiedText == "INVITE-123")
        let vm = AdminVM()
        vm.recordMintedCodeCopy(outcome)
        #expect(vm.actionError == nil)
    }

    @Test("a failed copy lands common.copy_failed on admin-users-action-error")
    func copyFail() {
        let outcome = CopyOutcome.attempt("INVITE-123", writer: { _ in throw CopyRefused() })
        #expect(outcome.copiedText == nil)
        let vm = AdminVM()
        vm.recordMintedCodeCopy(outcome)
        #expect(vm.actionError == L.common.copyFailed(error: "no pasteboard"))
    }
}

@Suite("Four controls paint from the shared Possibility")
struct SharedPossibilityGateTests {
    private static func caps(canSend: Bool) -> ThreadCapabilities {
        ThreadCapabilities(
            supportsAttachments: true,
            supportsMarkdown: true,
            supportsReactions: true,
            supportsMessageDelete: true,
            supportsPerMessageReply: true,
            supportsMembershipChange: false,
            supportsRecipientSelection: false,
            supportsRename: false,
            supportsSubject: false,
            deliveryMode: .realtime,
            encryption: .e2e,
            canInvite: false,
            canRemoveMembers: false,
            canSetPolicy: false,
            canAppointAdmins: false,
            canTransferOwnership: false,
            canLeaveRoom: false,
            canSearchRoom: false,
            canSend: canSend,
            supportsLocalRemove: false
        )
    }

    @Test("reply bar: a disconnected bridge blocks with the room notice; a live room is possible")
    func replyBar() {
        let blocked = DmComposeBar.replyAnswerFor(Self.caps(canSend: false))
        #expect(blocked.isShown && !blocked.isPossible)
        guard case .blocked(let reason) = blocked else {
            Issue.record("expected blocked, got \(blocked)")
            return
        }
        #expect(reason.key == "conversations.unified.room_notice_bridge_disconnected")
        #expect(DmComposeBar.replyAnswerFor(Self.caps(canSend: true)).isPossible)
        #expect(DmComposeBar.replyAnswerFor(nil).isPossible, "new-thread compose answers per control")
    }

    @Test("rule editor Save: blocked by the first defect, possible on a clean document")
    func ruleEditorSave() {
        let clean = FfiRuleEditor.forGuardian(policy: nil)
        #expect(clean.savePossibility().isPossible)
        #expect(clean.savePossibility().isPossible == clean.view().canSave)

        let defective = FfiRuleEditor.forGuardian(policy: nil)
        _ = defective.addRule(kind: .category)
        let answer = defective.savePossibility()
        #expect(!answer.isPossible)
        #expect(answer.isPossible == defective.view().canSave)
        guard case .blocked(let reason) = answer else {
            Issue.record("expected blocked, got \(answer)")
            return
        }
        #expect(reason.key == "rule_editor.defect.no_reason")
    }

    private static let own = String(repeating: "11", count: 32)
    private static let peer = String(repeating: "22", count: 32)

    private static func share(_ panel: OfflineSharePanel, peer: String,
                              status: CeremonyStatus = .idle) -> OfflineShareView {
        OfflineShareView(panel: panel, ownCode: own, peerCode: peer, status: status, available: true)
    }

    @Test("offline share Begin: blocked without a code, possible with one, absent off its panel")
    func offlineShareBegin() {
        let empty = Self.share(.initiate, peer: "")
        #expect(offlineShareBeginPossibility(view: empty).isShown)
        #expect(!offlineShareBeginPossibility(view: empty).isPossible)
        #expect(offlineShareBeginPossibility(view: empty).reasonText != nil)

        let ready = Self.share(.initiate, peer: Self.peer)
        #expect(offlineShareBeginPossibility(view: ready).isPossible)
        #expect(OfflineShareSectionView.actAnswer(ready).isPossible)

        #expect(!offlineShareBeginPossibility(view: Self.share(.receive, peer: Self.peer)).isShown)
    }

    @Test("offline receive Expect: blocked by its own code, possible with the peer's")
    func offlineReceiveExpect() {
        let own = Self.share(.receive, peer: Self.own)
        #expect(!offlineShareExpectPossibility(view: own).isPossible)
        #expect(offlineShareExpectPossibility(view: own).isShown)

        let ready = Self.share(.receive, peer: Self.peer)
        #expect(offlineShareExpectPossibility(view: ready).isPossible)
        #expect(OfflineShareSectionView.actAnswer(ready).isPossible)
    }

    @Test("share-link-button: possible where eligible, absent elsewhere — never inert")
    func shareLink() {
        #expect(shareLinkPossibility(eligible: true).isPossible)
        let absent = shareLinkPossibility(eligible: false)
        #expect(!absent.isShown)
        #expect(absent.reasonText == nil)
    }
}
