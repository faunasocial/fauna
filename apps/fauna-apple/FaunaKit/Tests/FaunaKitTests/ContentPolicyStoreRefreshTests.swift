import Testing
import Foundation
@testable import FaunaKit

// `ContentPolicyStore`'s keep-on-failure refresh semantics (family-safety.md §
// Content policy, the unfetched-policy ruling's clause 1): a failed read must leave the last-known
// guardian policy / own thresholds in force, never reset them. "Read failed"
// and "read succeeded and reports unsupervised" are different facts; only the
// second may clear a loaded half. android's `ContentPolicyStoreTest` is the
// reference leg this mirrors (same ruling, same fold shape).
//
// `ContentPolicyStore.refresh(api:)` itself needs a live `APIClient` (a
// concrete class, not a protocol seam), so these drive the extracted
// `ContentPolicyStore.merged` fold directly — the same logic `refresh` calls,
// with the "did the read succeed" fact made explicit instead of buried in a
// `try?`.

private func flaggedNsfw() -> [ContentLabelEntry] {
    [ContentLabelEntry(category: "nsfw", confidencePerMille: 900)]
}

@Test("a failed status read keeps the loaded guardian policy in force")
func failedStatusReadKeepsTheLoadedGuardianPolicy() {
    let loaded = ContentPolicyInputs(contentPolicy: nsfwBlockRule())
    #expect(loaded.verdictFor(flaggedNsfw()) == "block", "sanity: the guardian policy is actually enforcing")

    let after = ContentPolicyStore.merged(
        previous: loaded,
        policy: .failed,
        own: .succeeded((spam: 700, phishing: 900)))

    #expect(after.contentPolicy == loaded.contentPolicy,
            "a failed status read must not clear the previously-loaded guardian policy")
    #expect(after.verdictFor(flaggedNsfw()) == "block",
            "the guardian policy must still enforce after a failed refresh, not silently become \"show\"")
    #expect(after.ownSpamPermille == 700 && after.ownPhishingPermille == 900,
            "the OTHER half still moves on its own successful read")
}

@Test("a failed own-thresholds read keeps the loaded thresholds in force")
func failedOwnThresholdsReadKeepsThemInForce() {
    let loaded = ContentPolicyInputs(ownSpamPermille: 500, ownPhishingPermille: 500)

    let after = ContentPolicyStore.merged(
        previous: loaded,
        policy: .succeeded((nil, false)),
        own: .failed)

    #expect(after.ownSpamPermille == 500 && after.ownPhishingPermille == 500,
            "a failed preferences read must not clear the previously-loaded thresholds")
    #expect(after.contentPolicy == nil, "the OTHER half still moves on its own successful read")
}

@Test("a successful read reporting unsupervised DOES clear a previously-loaded guardian policy")
func successfulUnsupervisedReadClearsTheGuardianPolicy() {
    let loaded = ContentPolicyInputs(contentPolicy: nsfwBlockRule())

    let after = ContentPolicyStore.merged(
        previous: loaded,
        policy: .succeeded((nil, false)),
        own: .succeeded((spam: 0, phishing: 0)))

    #expect(after.contentPolicy == nil,
            "a successful read that reports no guardian policy (graduation, revocation) must clear it — this is the one outcome that legitimately does")
}

@Test("both reads failing leaves both halves untouched")
func bothReadsFailingLeavesBothHalvesUntouched() {
    let loaded = ContentPolicyInputs(
        contentPolicy: nsfwBlockRule(), ownSpamPermille: 500, ownPhishingPermille: 500, contentNotify: true)

    let after = ContentPolicyStore.merged(previous: loaded, policy: .failed, own: .failed)

    #expect(after == loaded, "a fully-failed refresh must be a complete no-op on the cache")
}

@Test("a failed status read keeps contentNotify at its previous value, not just the guardian policy")
func failedStatusReadKeepsContentNotifyInForce() {
    let loaded = ContentPolicyInputs(contentPolicy: nsfwBlockRule(), contentNotify: true)

    let after = ContentPolicyStore.merged(
        previous: loaded,
        policy: .failed,
        own: .succeeded((spam: 0, phishing: 0)))

    #expect(after.contentNotify, "contentNotify rides the SAME status read as the guardian policy — a failed read must not silently turn Notify off")
}

@Test("a successful status read moves contentNotify off the wire value, defaulting absent to false")
func successfulStatusReadMovesContentNotify() {
    let loaded = ContentPolicyInputs(contentNotify: false)

    let turnedOn = ContentPolicyStore.merged(
        previous: loaded,
        policy: .succeeded((nsfwBlockRule(), true)),
        own: .succeeded((spam: 0, phishing: 0)))
    #expect(turnedOn.contentNotify)

    let turnedOff = ContentPolicyStore.merged(
        previous: turnedOn,
        policy: .succeeded((nsfwBlockRule(), false)),
        own: .succeeded((spam: 0, phishing: 0)))
    #expect(!turnedOff.contentNotify)
}

// `refresh(api: nil)`'s own keep-on-failure guard (`guard let api else { return }`) was not
// pinned here as a store-level test when this file was written: `inputs` is `private(set)`, and
// back then nothing outside ContentPolicyStore.swift could load it to a non-default state without
// a live `APIClient` driving a real successful `refresh` first. `merged`'s four tests above cover
// the actual behavior change (the `try?`-collapsed success/failure distinction) either way.
//
// That limitation no longer holds: `ContentPolicyStore.seed(from:)` (family-safety.md § Content
// policy, clause 2) loads `inputs` to a non-default state with no
// `APIClient` at all, so the real `refresh(api: nil)` guard IS now driven directly — see
// `SupervisionSnapshotSeedTests.swift`'s `seededGuardianPolicySurvivesAFailedRefresh`.

// A save on the Privacy page re-binds the own thresholds at once: no push
// carries them, so without the re-bind a ward moving their own threshold
// stricter than the guardian policy saw nothing fold until the next login
// (`family-client-enforcement.md` § Content policy).

@MainActor
@Test("a saved own threshold binds the render at once")
func savedOwnThresholdBindsAtOnce() {
    let store = ContentPolicyStore()
    let spam300 = [ContentLabelEntry(category: "spam", confidencePerMille: 300)]
    #expect(store.inputs.verdictFor(spam300) == "show", "sanity: nothing folds a 300 spam label by default")

    store.setOwnThresholds(spamPermille: 200, phishingPermille: 500, scope: store.scope)

    #expect(store.inputs.ownSpamPermille == 200 && store.inputs.ownPhishingPermille == 500)
    #expect(store.inputs.verdictFor(spam300) != "show",
            "a 200 own threshold must fold a 300 spam label without a relaunch")
}

@MainActor
@Test("a save that outlived an identity change binds nothing")
func staleScopeSaveBindsNothing() {
    let store = ContentPolicyStore()
    let scope = store.scope
    store.seed(from: nil)  // the identity moved while the save was in flight

    store.setOwnThresholds(spamPermille: 200, phishingPermille: 500, scope: scope)

    #expect(store.inputs.ownSpamPermille == nil && store.inputs.ownPhishingPermille == nil,
            "the outgoing account's thresholds must never land on the next one")
}
