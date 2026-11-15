import Foundation
import Testing
@testable import FaunaKit

// The unsent-changes surface's words on apple (`account-offline-mutation.md`
// § The offline-mutation contract → *The unsent-changes surface*), the
// twin of tui's `the_unsent_changes_section_is_standing_and_offers_retry_on_a_not_sent_row_only`:
// every word comes from the shared describer, and only a not-sent row offers
// retry and discard. The journey is the tier_3 `test_unsent_changes.py`.

private func change(_ id: String, _ state: UnsentState) -> UnsentChange {
    UnsentChange(
        intentId: id,
        kind: "fauna.posts.interact",
        scope: "posts.interact:p",
        state: state,
        createdAt: 0,
        attempts: 0,
        description: IntentDescription(
            label: LocalizedText(key: "settings.unsent_changes.kind_like", args: [:]),
            target: nil),
        hold: nil)
}

@Test func unsentRowsAreWordedByTheSharedDescriber() {
    let notSent = change("aa", .unsent(reasonCode: "not_found"))
    let held = change("bb", .held)
    let waiting = change("cc", .waiting)

    #expect(UnsentChangesSection.describe(notSent) == L.settings.unsentChanges.kindLike)
    #expect(
        UnsentChangesSection.status(notSent)
            == L.settings.unsentChanges.statusWithReason(
                state: L.settings.unsentChanges.stateUnsent,
                reason: L.settings.unsentChanges.reasonFallback(code: "not_found")))
    #expect(UnsentChangesSection.status(held) == L.settings.unsentChanges.stateHeld)
    #expect(UnsentChangesSection.status(waiting) == L.settings.unsentChanges.stateWaiting)
}

@Test func onlyANotSentRowOffersRetryAndDiscard() {
    #expect(UnsentChangesSection.isNotSent(change("aa", .unsent(reasonCode: "x"))))
    #expect(!UnsentChangesSection.isNotSent(change("bb", .held)))
    #expect(!UnsentChangesSection.isNotSent(change("cc", .waiting)))
}

@Test func theHeartsStateWordIsTheSharedOne() {
    #expect(likeStateWord(state: .on) == "on")
    #expect(likeStateWord(state: .off) == "off")
    #expect(likeStateWord(state: .queued) == "queued")
    #expect(likeStateWord(state: .unsent) == "unsent")
}
