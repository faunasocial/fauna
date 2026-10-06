import Testing
import Foundation
@testable import FaunaKit

// The conversations page-error read leg (`docs/goal/ui/conversations.md` §
// Errors & edge cases — the two-truths split, plus the fourth truth added
// 2026-09-13 and the fifth (floor) truth added 2026-09-15):
// `ConversationsVM.pageError` reads `snapshot.error` (membership/label
// wire-op failures) FIRST, falling back to the selected thread's compose
// `send_state` (send failures), then the unopenable-mail floor. Mirrors
// tui's `sync_page_error` (`apps/fauna-tui/src/conversations/mod.rs`) — same
// assertions, ported to the shared FaunaKit VM both apple targets read.

@MainActor
@Test func pageErrorSurfacesAFailedMembershipOpFromTheSnapshot() {
    let vm = ConversationsVM()
    #expect(vm.pageError == "", "sanity: no error before the gesture")

    vm.manager.injectPageErrorForTest(error: LocalizedText(
        key: "conversations.unified.error_add_participant",
        args: ["message": "no key package published"]))

    let shown = vm.pageError
    #expect(shown.contains("no key package published"), "the backend's own reason must reach the user, got \(shown)")
    #expect(!shown.contains("conversations.unified"), "the i18n key must be resolved, not painted raw: \(shown)")
}

@MainActor
@Test func pageErrorFallsBackToTheSelectedThreadsSendFailure() {
    let vm = ConversationsVM()
    let alice = tryParseTypedAddress(raw: "alice@example.com")!
    let tid = vm.manager.createMlsGroup(participants: [alice])
    #expect(vm.pageError == "", "sanity: no error before the failure")

    vm.manager.injectSendFailureForTest(id: tid, reason: "nest rejected fauna.email.send")

    let shown = vm.pageError
    #expect(
        shown.contains("nest rejected fauna.email.send"),
        "a send_state=Failed stamp on the selected thread's compose must surface via the fallback, got \(shown)")
}

@MainActor
@Test func pageErrorPrefersTheSnapshotErrorOverAConcurrentSendFailure() {
    // The two truths never overlap in product operation (every producer
    // clears the other on entry), but the precedence itself is load-bearing
    // — reversing it, or dropping either read, breaks one of the two
    // surfaces. Assert the order directly rather than trusting it holds.
    let vm = ConversationsVM()
    let alice = tryParseTypedAddress(raw: "alice@example.com")!
    let tid = vm.manager.createMlsGroup(participants: [alice])
    vm.manager.injectSendFailureForTest(id: tid, reason: "send failure")

    vm.manager.injectPageErrorForTest(error: LocalizedText(
        key: "conversations.unified.error_rename_thread",
        args: ["message": "nest unreachable"]))

    let shown = vm.pageError
    #expect(shown.contains("nest unreachable"), "the page-level error must win, got \(shown)")
    #expect(!shown.contains("send failure"), "the page-level error must fully shadow the send failure, got \(shown)")
}

@MainActor
@Test func pageErrorRanksReceiveStoppedAboveTheSnapshotErrorAndBelowServedElsewhere() {
    // The fourth truth (`conversations.md` § Errors & edge cases, 2026-09-13):
    // a dead receive rail is standing like the served-elsewhere refusal, so it
    // must outrank the snapshot error / send-state truths but never mask the
    // role-lock refusal above it.
    let vm = ConversationsVM()
    vm.manager.injectPageErrorForTest(error: LocalizedText(
        key: "conversations.unified.error_add_participant",
        args: ["message": "no key package published"]))
    #expect(vm.pageError.contains("no key package published"), "sanity: the snapshot error shows before the rail dies")

    vm.manager.markReceiveStoppedForTest()

    let shown = vm.pageError
    #expect(shown == L.conversations.errors.receiveStopped, "the dead-rail notice must shadow the snapshot error, got \(shown)")

    vm.manager.setEngineServedElsewhere(served: true)
    #expect(vm.pageError == L.conversations.errors.servedElsewhere, "the standing role-lock refusal must still outrank the dead rail")
}

@MainActor
@Test func pageErrorReceiveStoppedIsNotClearedByASuccess() {
    // "No gesture clears it" (`conversations.md` § Errors & edge cases): only
    // a newer receive loop over the same manager retires the notice. A
    // producer succeeding and clearing the UNRELATED snapshot-error truth
    // must leave the dead-rail notice standing.
    let vm = ConversationsVM()
    vm.manager.markReceiveStoppedForTest()
    #expect(vm.pageError == L.conversations.errors.receiveStopped, "sanity: the notice shows right after the loop dies")

    vm.manager.clearPageError()

    #expect(vm.pageError == L.conversations.errors.receiveStopped, "an unrelated success clearing the snapshot error must not clear the dead-rail notice")
}

@MainActor
@Test func pageErrorSurfacesUnopenableMailWhenNothingElseIsStanding() {
    // The fifth truth, the floor of the stack (`conversations.md` § Errors &
    // edge cases, 2026-09-15): a received record this run could not open
    // shows as a count, substituted through the generated formatter.
    let vm = ConversationsVM()
    #expect(vm.pageError == "", "sanity: no error before any record is skipped")

    vm.manager.noteUnopenableMailForTest(uid: 1)

    let shown = vm.pageError
    #expect(
        shown == L.conversations.errors.mailUnopenable(count: "1"),
        "the floor notice must show the resolved count, got \(shown)")

    vm.manager.noteUnopenableMailForTest(uid: 2)
    #expect(
        vm.pageError == L.conversations.errors.mailUnopenable(count: "2"),
        "a second skip must bump the count")
}

@MainActor
@Test func pageErrorRanksUnopenableMailBelowEveryOtherTruth() {
    // The floor is never a mask: a fresh failure of any gesture, a dead
    // rail, or the role refusal must all outrank it.
    let vm = ConversationsVM()
    let alice = tryParseTypedAddress(raw: "alice@example.com")!
    let tid = vm.manager.createMlsGroup(participants: [alice])

    vm.manager.noteUnopenableMailForTest(uid: 1)
    #expect(
        vm.pageError == L.conversations.errors.mailUnopenable(count: "1"),
        "sanity: the floor shows with nothing else standing")

    vm.manager.injectSendFailureForTest(id: tid, reason: "send failure")
    #expect(vm.pageError.contains("send failure"), "a fresh send failure must outrank the floor")

    vm.manager.markReceiveStoppedForTest()
    #expect(
        vm.pageError == L.conversations.errors.receiveStopped,
        "the dead-rail notice must outrank both the send failure and the floor")
}
