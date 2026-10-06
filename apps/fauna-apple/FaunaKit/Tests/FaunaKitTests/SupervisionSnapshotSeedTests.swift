import Testing
import Foundation
@testable import FaunaKit

// The apple clause-2 leg's restore call sites (family-safety.md § Content
// policy, clause 2): `ContentPolicyStore.seed`,
// `ScreenTimeStore.seed` and `FamilyStatusStore.seed`, driven directly (no
// live `APIClient` needed — the same reason `ContentPolicyStoreRefreshTests`
// drives `merged` rather than `refresh`). The row's own success bar: load a
// floor via a persisted snapshot, fail the read, assert `verdictFor` still
// returns the floor's verdict — and each restore field mutation-graded
// individually, since tui's reference leg once passed a happy-path pin while
// silently dropping `content_notify` on the very same restore.

private func guardian(handle: String = "alex") -> FfiFamilyGuardianInfo {
    FfiFamilyGuardianInfo(actorId: Data(repeating: 0xAB, count: 32), handle: handle)
}

private func floor(nsfw: String = "block") -> FfiContentPolicy {
    FfiContentPolicy(nsfw: nsfw, spam: "inherit", phishing: "inherit", commercial: "inherit")
}

private func flaggedNsfw() -> [ContentLabelEntry] {
    [ContentLabelEntry(category: "nsfw", confidencePerMille: 900)]
}

private func snapshot(
    handle: String = "alex", nsfw: String = "block", notify: Bool = true,
    dailyMinutes: UInt16? = nil
) -> FfiSupervisionSnapshot {
    FfiSupervisionSnapshot(
        supervisedBy: guardian(handle: handle),
        contentPolicy: floor(nsfw: nsfw),
        contentNotify: notify,
        screenTime: dailyMinutes.map { FfiScreenTimePolicy(windowStart: nil, windowEnd: nil, dailyMinutes: $0) })
}

// MARK: - ContentPolicyStore — the row's own success bar

@MainActor
@Test("clause 2: a snapshot-restored floor survives a nil-api refresh — verdictFor still enforces")
func seededFloorSurvivesAFailedRefresh() async {
    let store = ContentPolicyStore()
    store.seed(from: snapshot(nsfw: "block", notify: true))
    #expect(store.inputs.verdictFor(flaggedNsfw()) == "block", "sanity: the seed actually loaded the floor")

    // `refresh(api: nil)` models "the read hasn't (yet) succeeded" — the
    // client not wired, or a session between teardown and re-establish; its
    // own keep-on-failure guard (clause 1) must not undo the restore.
    await store.refresh(api: nil)

    #expect(store.inputs.verdictFor(flaggedNsfw()) == "block",
            "a failed/unwired read must not drop a snapshot-restored floor before the first live read lands")
}

@Test("mutation grade: seed restores contentNotify independently of the floor")
@MainActor
func seedRestoresContentNotifyNotJustTheFloor() {
    let store = ContentPolicyStore()
    store.seed(from: snapshot(nsfw: "block", notify: true))

    #expect(store.inputs.contentPolicy == floor(nsfw: "block"), "the whole floor struct must round-trip, not just nsfw")
    #expect(store.inputs.contentNotify,
            "contentNotify must be seeded too — a pin that only checks verdictFor cannot catch a mutant that drops this field (the tui 7th-mutant shape)")
}

@Test("mutation grade: seed with contentNotify false does not default it true")
@MainActor
func seedRestoresContentNotifyFalseWhenSnapshotSaysFalse() {
    let store = ContentPolicyStore()
    store.seed(from: snapshot(notify: false))
    #expect(!store.inputs.contentNotify, "a mutant that hardcodes contentNotify=true would slip past a positive-only check")
}

@Test("seed(nil) clears the guardian half to unsupervised")
@MainActor
func seedNilClearsTheGuardianHalf() {
    let store = ContentPolicyStore()
    store.seed(from: snapshot())
    #expect(store.inputs.contentPolicy != nil, "sanity: seeded")

    store.seed(from: nil)

    #expect(store.inputs.contentPolicy == nil,
            "seed(nil) — the account-switch / teardown case — must clear a previously-restored floor")
    #expect(!store.inputs.contentNotify)
}

@Test("seed never touches the viewer's own thresholds — they are not in the snapshot")
@MainActor
func seedLeavesOwnThresholdsUntouched() {
    let store = ContentPolicyStore()
    // Load the own-threshold half the same way a successful preferences read
    // would, via the tested `merged` fold — `seed` must not disturb it.
    let withOwn = ContentPolicyStore.merged(
        previous: ContentPolicyInputs(), policy: .failed, own: .succeeded((spam: 700, phishing: 900)))
    #expect(withOwn.ownSpamPermille == 700)

    store.seed(from: snapshot())

    #expect(store.inputs.ownSpamPermille == nil && store.inputs.ownPhishingPermille == nil,
            "seed only ever seeds the guardian half — own thresholds re-arrive with their own read")
}

// MARK: - ScreenTimeStore — the window/guardian half, usage total excluded

@MainActor
@Test("clause 2: seed restores the window/budget policy AND the guardian, independently")
func seedRestoresPolicyAndGuardianIndependently() {
    let store = ScreenTimeStore()

    store.seed(from: snapshot(handle: "alex", dailyMinutes: 5))

    #expect(store.policyForTest?.dailyMinutes == 5,
            "mutation grade: a mutant dropping the policy assignment must fail this, not just a guardian-only check")
    #expect(store.guardianHandleForTest == "alex",
            "mutation grade: a mutant dropping the guardian assignment must fail this, not just a policy-only check")
}

@MainActor
@Test("clause 2: the day's usage total is NOT restored — it re-arrives with the first live read")
func seedNeverRestoresTheUsageTotal() {
    let store = ScreenTimeStore()

    store.seed(from: snapshot(dailyMinutes: 5))

    #expect(store.usedTodayMinutes() == nil,
            "a restored budget with an UNKNOWN usage total must not be evaluable yet (the ratified fail-open on the budget arm) — android's setWardScreenTime(policy, guardian, null) is the reference shape this mirrors")
    #expect(store.lockMessage == nil,
            "a budget lock needs a nest-CONFIRMED usage total (lock_state's Some/Some guard) — an unknown total must never lock out of caution")
}

@MainActor
@Test("seed(nil) clears a previously-restored policy and guardian")
func seedNilClearsARestoredScreenTimeState() {
    let store = ScreenTimeStore()
    store.seed(from: snapshot(handle: "alex", dailyMinutes: 5))
    #expect(store.guardianHandleForTest != nil, "sanity: seeded")

    store.seed(from: nil)

    #expect(store.policyForTest == nil && store.guardianHandleForTest == nil,
            "a departing account's restored policy/guardian must never bleed into the next session's first paint")
    #expect(store.lockMessage == nil)
}

// MARK: - FamilyStatusStore — the supervised half keeps a restored fallback
// through a fail-closed sweep on the OTHER (guardian/wards) half

@MainActor
@Test("clause 2: a restored guardian gates both the indicator and family-tab")
func seedRestoresTheSupervisedIndicatorAndFamilyTabGate() {
    let store = FamilyStatusStore()
    #expect(store.supervisedByHandle == nil && !store.hasRelationship, "sanity: nothing restored yet")

    store.seed(from: snapshot(handle: "alex"))

    #expect(store.supervisedByHandle == "alex",
            "the supervised-indicator's guardian must come from the restored snapshot ahead of the first read")
    #expect(store.hasRelationship,
            "family-tab must be reachable from the restored guardian alone — a ward must always be able to see who supervises them")
}

@MainActor
@Test("a fail-closed sweep on the guardian/wards half does not undo the restored supervised fallback")
func failClosedGuardianHalfDoesNotClobberTheRestoredSupervisedHalf() async {
    let store = FamilyStatusStore()
    store.seed(from: snapshot(handle: "alex"))
    #expect(store.supervisedByHandle == "alex", "sanity: seeded")

    // `refresh(api: nil)` models the bare teardown/not-yet-wired case, which
    // DOES fully clear (a departing account must not survive it) — so this
    // pins the store's clear() contract directly rather than faking a
    // "read failed but api was non-nil" case ContentPolicyStore's own test
    // file notes cannot be driven without a live APIClient.
    await store.refresh(api: nil)

    #expect(store.supervisedByHandle == nil && !store.hasRelationship,
            "refresh(api: nil) is the account-switch/teardown transition — it must fully clear, unlike a genuinely failed live read")
}

@MainActor
@Test("seed(nil) clears a previously-restored guardian fallback")
func seedNilClearsARestoredGuardianFallback() {
    let store = FamilyStatusStore()
    store.seed(from: snapshot(handle: "alex"))
    #expect(store.supervisedByHandle == "alex", "sanity: seeded")

    store.seed(from: nil)

    #expect(store.supervisedByHandle == nil && !store.hasRelationship,
            "a departing account's restored guardian must never bleed into the next session's first paint")
}
