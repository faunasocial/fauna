import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for `NewThreadControlAnswers` — the one decision the shared
// FaunaKit new-thread composer (`NewThreadComposeForm` → `DmComposeBar` in its
// `capabilities: nil` mode) paints Send, attach and topic from, and each
// handler asks again (`ui/conversations.md` § Errors & edge cases → *The
// new-thread composer offers only what it can send*; `ui/README.md` § Offer
// only what is possible, rules 4, 6 and 7). The answers come from the shared
// predicate over FFI (`newThreadControlPossibility`), so this also pins that
// the binding reaches the apple apps; how a disabled control LOOKS is the last
// inch. The send and Keep failure sentences are pinned here too: neither
// failure is log-only.
@Suite("New-thread composer controls answer in Possibility")
struct NewThreadControlAnswersTests {
    private static func compose(
        rawInput: String = "",
        chips: [TypedAddress] = [],
        resolveState: ResolveState = .idle,
        resolved: TypedAddress? = nil,
        subjectDraft: String? = nil
    ) -> ComposeState {
        ComposeState(
            bodyDraft: "",
            subjectDraft: subjectDraft,
            attachments: [],
            replyTo: nil,
            replyRecipients: [],
            recipientPicker: RecipientPickerState(
                rawInput: rawInput,
                chips: chips,
                suggestions: [],
                suggestionDisplays: [],
                resolveState: resolveState,
                resolved: resolved,
                includeHomeNest: false
            ),
            sendState: .idle,
            listSend: nil
        )
    }

    private static let mail = TypedAddress.email(emailAddress: "someone@example.test")
    private static let bridged = TypedAddress.bridged(bridgeId: "signal", address: "+15550100")

    private static func key(_ p: Possibility) -> String? {
        if case .blocked(let reason) = p { return reason.key }
        return nil
    }

    @Test("No composer open answers absent for every control")
    func noComposerIsAbsent() {
        let answers = NewThreadControlAnswers(nil)
        for control in [NewThreadControl.send, .attach, .topic] {
            #expect(!answers(control).isShown, "\(control)")
        }
    }

    @Test("An unaddressed draft blocks Send naming the recipient, but takes files and a topic")
    func unaddressedDraft() {
        let answers = NewThreadControlAnswers(Self.compose())
        #expect(Self.key(answers(.send)) == "conversations.compose.needs_recipient")
        // The reason a blocked Send gives is a real sentence, and its handler
        // refuses with it rather than dropping the click.
        #expect(answers(.send).reasonText != nil)
        #expect(answers(.send).refusal?.isEmpty == false)
        #expect(answers(.attach).isPossible)
        #expect(answers(.topic).isPossible)
    }

    @Test("Typed recipient text makes Send possible: Send flushes it into a chip")
    func typedRecipientMakesSendPossible() {
        let answers = NewThreadControlAnswers(Self.compose(rawInput: "someone@example.test", resolveState: .resolving))
        #expect(answers(.send).isPossible)
        #expect(answers(.send).refusal == nil)
    }

    @Test("A committed chip makes Send possible")
    func committedChipMakesSendPossible() {
        let answers = NewThreadControlAnswers(Self.compose(chips: [Self.mail]))
        #expect(answers(.send).isPossible)
        #expect(answers(.attach).isPossible)
        #expect(answers(.topic).isPossible)
    }

    @Test("A Bridged recipient blocks attach and topic, each with its reason")
    func bridgedRecipientBlocksAttachAndTopic() {
        let answers = NewThreadControlAnswers(Self.compose(chips: [Self.bridged]))
        #expect(answers(.send).isPossible)
        #expect(Self.key(answers(.attach)) == "conversations.compose.no_attachments_here")
        #expect(Self.key(answers(.topic)) == "conversations.compose.no_topic_here")
    }

    @Test("A resolved typed Bridged recipient decides the rail before it is committed")
    func resolvedTypedRecipientDecidesTheRail() {
        let answers = NewThreadControlAnswers(Self.compose(
            rawInput: "+15550100", resolveState: .resolved, resolved: Self.bridged))
        #expect(Self.key(answers(.attach)) == "conversations.compose.no_attachments_here")
    }

    @Test("An open topic can always be closed again, whatever the rail")
    func openTopicStaysClosable() {
        let answers = NewThreadControlAnswers(Self.compose(chips: [Self.bridged], subjectDraft: ""))
        #expect(answers(.topic).isPossible)
    }

    @Test("A send failure no draft carries is shown from its detail; a stamped one is not repeated")
    func sendFailureIsNeverSilent() {
        let stamped = SendState.failed(reason: LocalizedText(key: "conversations.compose.needs_recipient", args: [:]))
        let shown = newThreadSendFailureText(newThreadSend: .idle, selectedSend: nil, pageError: false, detail: "boom")
        #expect(shown?.contains("boom") == true)
        #expect(newThreadSendFailureText(newThreadSend: stamped, selectedSend: nil, pageError: false, detail: "boom") == nil)
        #expect(newThreadSendFailureText(newThreadSend: nil, selectedSend: stamped, pageError: false, detail: "boom") == nil)
        #expect(newThreadSendFailureText(newThreadSend: nil, selectedSend: .idle, pageError: true, detail: "boom") == nil)
    }

    @Test("A failed Keep says so with its reason")
    func keepFailureSaysSo() {
        let text = memberKeepFailureText(detail: "store offline")
        #expect(text.contains("store offline"))
        #expect(text != "store offline")
    }
}
