import Foundation
import Testing
@testable import FaunaKit

// Unit tests for the apple leg of the client render-enforcement engine
// (family-safety.md § Content policy, Slice C) — the twins of android's
// `ContentPolicyInputsTest` and linux's `content_policy::tests`.
//
// The *composition* (own thresholds ∪ guardian floor, strictest-wins,
// fail-closed on an unparseable floor) lives entirely in shared Rust and is
// pinned there by `fauna_core::obligation`'s 19 tests + `fauna-ffi`'s six
// `family::content_policy_tests` arms. These pin the **Swift-side wiring** only:
// that `ContentPolicyInputs` hands the shared engine the right ingredients, and
// that its FFI-free short-circuit cannot change a verdict.

private func entry(_ category: String, _ perMille: UInt16) -> ContentLabelEntry {
    ContentLabelEntry(category: category, confidencePerMille: perMille)
}

private func floors(
    nsfw: String = "inherit", spam: String = "inherit",
    phishing: String = "inherit", commercial: String = "inherit"
) -> FfiContentPolicy {
    FfiContentPolicy(nsfw: nsfw, spam: spam, phishing: phishing, commercial: commercial)
}

// MARK: - The short-circuit (no floor, no thresholds ⇒ no rule can fire)

@Test func noPolicyAndNoThresholdsRendersShowWithoutCallingTheEngine() {
    // A default-constructed value is what every VM-free preview/test harness
    // renders with; it must stay FFI-free (android's identical rationale) *and*
    // agree with the engine — with neither half set, no rule composes, so the
    // strictest verdict reachable is `badge`, which renders like `show`.
    #expect(ContentPolicyInputs().verdictFor([]) == "show")
    #expect(ContentPolicyInputs().verdictFor([entry("spam", 990)]) == "show")

    // The short-circuit is only sound because the engine agrees: ask it directly
    // with the same (empty) ingredients and it must not escalate either.
    #expect(contentRenderVerdict(
        labels: [entry("spam", 990)], contentPolicy: nil,
        ownSpamPermille: nil, ownPhishingPermille: nil) == "badge")
}

// MARK: - The guardian floor (the supervised half)

@Test func aGuardianBlockFloorBlocksAFlaggedItem() {
    let inputs = ContentPolicyInputs(contentPolicy: floors(spam: "block"))
    #expect(inputs.verdictFor([entry("spam", 900)]) == "block")
    // A different category the floor doesn't name is NOT enforced — it keeps
    // its badge. `badge` and `show` are distinct verdicts at the engine but
    // identical at the render gate: only `block` and `collapse` change what a
    // surface paints, which is exactly what the three render arms branch on.
    #expect(inputs.verdictFor([entry("nsfw", 900)]) == "badge")
    // …and a clean item is never enforced.
    #expect(inputs.verdictFor([]) == "show")
}

/// The render contract the three surfaces rely on: exactly two verdicts change
/// what gets painted. If the engine ever grew a third enforcing verb, this is
/// the test that fails and sends someone to the render arms.
@Test func onlyBlockAndCollapseAreEnforcingVerdicts() {
    let enforcing = Set(["block", "collapse"])
    #expect(!enforcing.contains("show"))
    #expect(!enforcing.contains("badge"))
    let inputs = ContentPolicyInputs(contentPolicy: floors(spam: "block"))
    #expect(enforcing.contains(inputs.verdictFor([entry("spam", 900)])))
    #expect(!enforcing.contains(inputs.verdictFor([entry("nsfw", 900)])))
}

@Test func aGuardianCollapseFloorCollapsesRatherThanBlocks() {
    let inputs = ContentPolicyInputs(contentPolicy: floors(nsfw: "collapse"))
    #expect(inputs.verdictFor([entry("nsfw", 900)]) == "collapse")
}

@Test func anUnparseableFloorFailsClosedToBlockNeverInherit() {
    // family-safety.md:265 — "A rule value a client cannot parse renders
    // fail-closed (`block`)". Within a major version this client may be OLDER
    // than its nest, so a newer floor spelling must never degrade to the
    // permissive `inherit` (which would show the ward content the guardian
    // actually blocked). The rule is enforced once, in shared Rust.
    let inputs = ContentPolicyInputs(contentPolicy: floors(spam: "quarantine"))
    #expect(inputs.verdictFor([entry("spam", 900)]) == "block")
}

// MARK: - The viewer's OWN thresholds (the every-user half —
// moderation.md § Categories & enforcement item 1)

@Test func theViewersOwnThresholdCollapsesFlaggedContentWithNoGuardianAtAll() {
    // The un-darking that has never shipped: an *unsupervised* viewer's own
    // spam threshold collapses a spam-labeled item in their own view.
    let inputs = ContentPolicyInputs(ownSpamPermille: 500, ownPhishingPermille: 500)
    #expect(inputs.verdictFor([entry("spam", 900)]) == "collapse")
    // Below the threshold it renders normally (a badge, which renders like show).
    #expect(inputs.verdictFor([entry("spam", 100)]) == "badge")
}

@Test func aHalfKnownThresholdPairComposesNoOwnRule() {
    // `fauna-ffi`'s `content_render_verdict` zips the pair, so one-of-two is no
    // rule at all (the shared `ViewerThresholds` pairing). Pinned here because
    // the Swift store CAN produce a half-known pair if a future refactor reads
    // the two thresholds independently — this test is what would catch it.
    let inputs = ContentPolicyInputs(ownSpamPermille: 500, ownPhishingPermille: nil)
    #expect(inputs.verdictFor([entry("spam", 900)]) == "badge")
}

@Test func theGuardianFloorOutranksTheViewersOwnThresholdStrictestWins() {
    // A ward may set themselves STRICTER than the floor, never looser: the
    // guardian's `block` survives a viewer threshold that would only collapse.
    let inputs = ContentPolicyInputs(
        contentPolicy: floors(spam: "block"), ownSpamPermille: 500, ownPhishingPermille: 500)
    #expect(inputs.verdictFor([entry("spam", 900)]) == "block")
}

// MARK: - The test-inject `labels` staging seam

// Neither inject path classifies — only the real MLS receive path's
// `observe_local_detection` does — so `labels=` on `conversations_inject_inbound`
// is the ONLY way a tier_3 test can stage a flagged message. The payload parse
// itself is shared Rust's (`inject_inbound_from_test_payload`, pinned in
// `manager_integration_tests.rs`); this pins the apple link end to end — the
// exact wire shape the Python action layer sends, through the shell's inject
// arm, onto the message the bubble renders — so a content-floor e2e failure
// can never be a dropped label wearing a product bug's clothes (e2e point 11).

@MainActor
@Test func aStagedSpamLabelReachesTheMessageAndTripsAGuardianBlockFloor() {
    let manager = ConversationsManager()
    manager.installMockBackendsForTest()
    ConversationsTestInject.injectInbound([
        "rail": "FaunaMls",
        "sender": "labelled@self-nest.test",
        "body": "a staged spam message",
        "labels": [["category": "spam", "confidence_per_mille": 900]],
    ], into: manager)

    let threadId = manager.snapshot().threads.first?.threadId
    let labels = threadId.flatMap { manager.threadDetail(id: $0) }?.messages.first?.labels ?? []
    #expect(labels.count == 1)
    #expect(labels.first?.category == "spam")
    #expect(labels.first?.confidencePerMille == 900)
    let inputs = ContentPolicyInputs(contentPolicy: floors(spam: "block"))
    #expect(inputs.verdictFor(labels) == "block")
}

// MARK: - The store's fail-closed contract

@Test func aFreshStoreEnforcesNothing() {
    // Fail-closed here means "enforce nothing", not "block everything": a failed
    // status/preferences read must never hide content the nest never asked us to
    // hide. Under-enforcing a viewer's own collapse is the safe direction; the
    // guardian floor re-arrives on the next refresh (login / reconnect).
    #expect(ContentPolicyStore().inputs.verdictFor([entry("spam", 990)]) == "show")
}
