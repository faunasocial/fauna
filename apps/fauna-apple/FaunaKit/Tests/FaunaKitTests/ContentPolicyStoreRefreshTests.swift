import Testing
import Foundation
@testable import FaunaKit

// `ContentPolicyStore`'s keep-on-failure refresh semantics (family-safety.md §
// Content policy, the unfetched-policy ruling's clause 1): a failed read must leave the last-known
// guardian floor / own thresholds in force, never reset them. "Read failed"
// and "read succeeded and reports unsupervised" are different facts; only the
// second may clear a loaded half. android's `ContentPolicyStoreTest` is the
// reference leg this mirrors (same ruling, same fold shape).
//
// `ContentPolicyStore.refresh(api:)` itself needs a live `APIClient` (a
// concrete class, not a protocol seam), so these drive the extracted
// `ContentPolicyStore.merged` fold directly — the same logic `refresh` calls,
// with the "did the read succeed" fact made explicit instead of buried in a
// `try?`.

private func floor(nsfw: String = "block") -> FfiContentPolicy {
    FfiContentPolicy(nsfw: nsfw, spam: "inherit", phishing: "inherit", commercial: "inherit")
}

private func flaggedNsfw() -> [ContentLabelEntry] {
    [ContentLabelEntry(category: "nsfw", confidencePerMille: 900)]
}

@Test("a failed status read keeps the loaded guardian floor in force")
func failedStatusReadKeepsTheLoadedFloor() {
    let loaded = ContentPolicyInputs(contentPolicy: floor())
    #expect(loaded.verdictFor(flaggedNsfw()) == "block", "sanity: the floor is actually enforcing")

    let after = ContentPolicyStore.merged(
        previous: loaded,
        policy: .failed,
        own: .succeeded((spam: 700, phishing: 900)))

    #expect(after.contentPolicy == loaded.contentPolicy,
            "a failed status read must not clear the previously-loaded floor")
    #expect(after.verdictFor(flaggedNsfw()) == "block",
            "the floor must still enforce after a failed refresh, not silently become \"show\"")
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

@Test("a successful read reporting unsupervised DOES clear a previously-loaded floor")
func successfulUnsupervisedReadClearsTheFloor() {
    let loaded = ContentPolicyInputs(contentPolicy: floor())

    let after = ContentPolicyStore.merged(
        previous: loaded,
        policy: .succeeded((nil, false)),
        own: .succeeded((spam: 0, phishing: 0)))

    #expect(after.contentPolicy == nil,
            "a successful read that reports no floor (graduation, revocation) must clear it — this is the one outcome that legitimately does")
}

@Test("both reads failing leaves both halves untouched")
func bothReadsFailingLeavesBothHalvesUntouched() {
    let loaded = ContentPolicyInputs(
        contentPolicy: floor(), ownSpamPermille: 500, ownPhishingPermille: 500, contentNotify: true)

    let after = ContentPolicyStore.merged(previous: loaded, policy: .failed, own: .failed)

    #expect(after == loaded, "a fully-failed refresh must be a complete no-op on the cache")
}

@Test("a failed status read keeps contentNotify at its previous value, not just the floor")
func failedStatusReadKeepsContentNotifyInForce() {
    let loaded = ContentPolicyInputs(contentPolicy: floor(), contentNotify: true)

    let after = ContentPolicyStore.merged(
        previous: loaded,
        policy: .failed,
        own: .succeeded((spam: 0, phishing: 0)))

    #expect(after.contentNotify, "contentNotify rides the SAME status read as the floor — a failed read must not silently turn Notify off")
}

@Test("a successful status read moves contentNotify off the wire value, defaulting absent to false")
func successfulStatusReadMovesContentNotify() {
    let loaded = ContentPolicyInputs(contentNotify: false)

    let turnedOn = ContentPolicyStore.merged(
        previous: loaded,
        policy: .succeeded((floor(), true)),
        own: .succeeded((spam: 0, phishing: 0)))
    #expect(turnedOn.contentNotify)

    let turnedOff = ContentPolicyStore.merged(
        previous: turnedOn,
        policy: .succeeded((floor(), false)),
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
// `SupervisionSnapshotSeedTests.swift`'s `seededFloorSurvivesAFailedRefresh`.
