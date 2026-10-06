import Testing
import Foundation
@testable import FaunaKit

// Reply-recipient editable "To" line (#2b — `docs/goal/ui/conversations.md`
// § Participants vs reply recipients). The seeding *behaviour* — reply seeds
// the sender, reply-all seeds participants-minus-self, dedup, the SMTP backend
// honouring `reply_recipients` — is covered by `fauna-conversations`' Rust
// tests (`manager_integration_tests.rs`, `smtp_backend_tests.rs`). These guard
// the *Swift seam* the apple lift added: the new `tryParseTypedAddress` UniFFI
// export crossing the boundary, and the `ConversationsVM` glue that parses +
// dispatches add/remove through the manager's draft. (The capability gate that
// shows the To line only on mail is a `DmComposeBar` view concern, exercised by
// the cross-app e2e.)

@Test func tryParseTypedAddressBindingRecognizesEmailAndRejectsJunk() {
    // The new free-function export is reachable across FFI…
    #expect(tryParseTypedAddress(raw: "") == nil)
    #expect(tryParseTypedAddress(raw: "   ") == nil)
    #expect(tryParseTypedAddress(raw: "not-an-address") == nil)
    // …and recognizes an email-shaped address (the `dm-reply-recipient-add` case).
    guard case let .email(addr)? = tryParseTypedAddress(raw: "alice@example.com") else {
        Issue.record("expected .email for alice@example.com")
        return
    }
    #expect(addr == "alice@example.com")
}

@MainActor
@Test func vmAddRemoveReplyRecipientRoundTripsThroughDraft() {
    let vm = ConversationsVM()
    let alice = tryParseTypedAddress(raw: "alice@example.com")!
    let tid = vm.manager.createMlsGroup(participants: [alice])

    // Parseable input is appended to the editable To line (the draft).
    #expect(vm.addReplyRecipient(tid, "bob@example.com") == true)
    // Unparseable input is a no-op — returns false so the field can keep the
    // text for the user to fix, and the recipient set is unchanged.
    #expect(vm.addReplyRecipient(tid, "not-an-address") == false)

    let after = vm.detail(tid)?.compose.replyRecipients ?? []
    #expect(after.count == 1)
    #expect(after.first.map(ConversationsUI.display) == "bob@example.com")

    // Removing a chip drops it from this reply only.
    if let only = after.first { vm.removeReplyRecipient(tid, only) }
    #expect(vm.detail(tid)?.compose.replyRecipients.isEmpty == true)
}
