import Testing
@testable import FaunaKit

// Pins the live-read SOURCE that the in-process `recipient-resolve-status`
// registry read depends on. The shared `fauna_conversations` manager stamps a
// SYNCHRONOUS state in `set_new_thread_recipient_input` (empty → Idle, any other
// input → Resolving — the probe is owed, and only the async `resolveRecipient`
// reports resolved / not-found / error), and the `ConversationsVM` snapshot
// getter reflects that update immediately.
//
// RecipientPicker's `automationValue("recipient-resolve-status", …)` closure now
// re-reads THIS live source (through the parent's `liveResolveState`, which
// closes over the @Observable `ConversationsVM`) instead of the value-type
// `RecipientPickerState` snapshot it had captured at `.onAppear`. The captured
// snapshot stayed `.idle` forever in-process because the single, in-place-
// updating `recipient-resolve-status` element registers its read closure ONCE
// and never re-registers — the backup-destination rename bug class.
//
// The registry-closure liveness itself is render-gated (needs `.onAppear` +
// SwiftUI lifecycle → covered by the macOS e2e harness); this test pins
// the data source so a manager/vm regression fails fast WITHOUT the (slow,
// render-gated) e2e harness. A stale snapshot read would be STUCK at its first
// value, so tracking the state both up (Idle→Resolved) and back down
// (Resolved→Idle) is what proves the source is live.
@MainActor
struct RecipientPickerResolveTests {
    @Test func newThreadResolveStateTracksManagerLiveBothWays() {
        let vm = ConversationsVM()
        vm.startNewConversation()
        #expect(vm.newThreadCompose?.recipientPicker?.resolveState == .idle)

        // Any non-empty input ⇒ Resolving, synchronously: typing owes the async
        // probe, and only `resolveRecipient` moves the state on (2026-08-29,
        // conversations.md § Errors & edge cases → *The picker tells the truth*;
        // the former shape-derived `.resolved` is retired).
        vm.setNewThreadRecipientInput("alice@self-nest.test")
        #expect(vm.newThreadCompose?.recipientPicker?.resolveState == .resolving)

        // Clearing the field re-derives Idle — a captured-Resolving snapshot would
        // be stuck at .resolving; the live read tracks it back down.
        vm.setNewThreadRecipientInput("")
        #expect(vm.newThreadCompose?.recipientPicker?.resolveState == .idle)

        // Unparseable input (no `@`, not a did/npub/fediverse-acct form) is STILL
        // non-empty ⇒ Resolving too, synchronously: `unprobed_resolve_state`
        // is purely empty-vs-non-empty now (`libs/fauna-conversations/src/
        // manager.rs`) — the state is never derived from the text's shape, so
        // there is no synchronous `.error` path left; only the async
        // `resolveRecipient` can report `.error` for a genuinely unparseable
        // address.
        vm.setNewThreadRecipientInput("not-an-address")
        #expect(vm.newThreadCompose?.recipientPicker?.resolveState == .resolving)
    }
}
