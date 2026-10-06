import Foundation
import Testing
@testable import FaunaKit

// Unit tests for the pure rules behind the shared Family surface
// (family-safety.md § App surface), the apple twins of linux's
// `views/family.rs` unit tests and windows' `FamilyApprovalRowTests`. The live
// wire round-trip is the tier_3 `test_family.py`; these pin the rules whose
// inversion is a *safety* bug, so they fail fast rather than in a suite run.

// MARK: - Fail-closed knob mapping (family-safety.md § Implementation status
// today: "A knob value a client cannot parse renders fail-closed, on every
// app")

// The option catalogs + fail-closed label/wire mapping are shared Rust now
// (`unknownSenderOptions`/`feedSourcesOptions`/`unknownSenderLabel`/
// `feedSourcesLabel`, pinned by their own `fauna_core::format` unit tests) —
// these three only pin the Swift-side wiring (`FamilyReachPolicyFormat`),
// mirroring android's `normalizeUnknownSenderWire`/`normalizeFeedSourcesWire`
// tests.

@Test func policyLabelsMatchTheCrossClientContract() {
    // The e2e drives both selects with these exact localized labels
    // (`actions/family.py` UNKNOWN_SENDER_LABELS / FEED_SOURCES_LABELS), so a
    // relabel here silently breaks every app's shared test.
    #expect(unknownSenderOptions().map { renderLocalizedText($0.label) } == ["Allow", "Hold for review", "Reject"])
    #expect(feedSourcesOptions().map { renderLocalizedText($0.label) } == ["Allow", "Block"])
}

@Test func knownKnobValuesRoundTripThroughTheirLabels() {
    let unknownSender = unknownSenderOptions()
    for wire in ["allow", "hold", "reject"] {
        let label = renderLocalizedText(unknownSenderLabel(value: wire))
        #expect(FamilyReachPolicyFormat.wire(forPickedLabel: label, options: unknownSender, label: unknownSenderLabel(value:)) == wire)
    }
    let feedSources = feedSourcesOptions()
    for wire in ["allow", "block"] {
        let label = renderLocalizedText(feedSourcesLabel(value: wire))
        #expect(FamilyReachPolicyFormat.wire(forPickedLabel: label, options: feedSources, label: feedSourcesLabel(value:)) == wire)
    }
}

@Test func anUnparseableKnobValueRendersFailClosedNeverAllow() {
    // A newer nest may store a value this (older) client cannot name. Rendering
    // it as the permissive `allow` would show the guardian a policy weaker than
    // the one actually enforced — and, since save writes the editor's state back,
    // the next save would genuinely downgrade the ward. So: strictest option.
    #expect(renderLocalizedText(unknownSenderLabel(value: "quarantine")) == "Hold for review")
    #expect(renderLocalizedText(feedSourcesLabel(value: "curated")) == "Block")

    // The same rule binds on the way *out* of the editor…
    #expect(FamilyReachPolicyFormat.wire(
        forPickedLabel: "Nonsense", options: unknownSenderOptions(), label: unknownSenderLabel(value:)) == "hold")
    #expect(FamilyReachPolicyFormat.wire(
        forPickedLabel: "Nonsense", options: feedSourcesOptions(), label: feedSourcesLabel(value:)) == "block")

    // …and on the draft the editor loads, so a save can only ever write the
    // strict value it displayed, never the unparseable one it round-tripped.
    #expect(FamilyReachPolicyFormat.normalizedWire(
        "quarantine", options: unknownSenderOptions(), label: unknownSenderLabel(value:)) == "hold")
    #expect(FamilyReachPolicyFormat.normalizedWire(
        "curated", options: feedSourcesOptions(), label: feedSourcesLabel(value:)) == "block")
    #expect(FamilyReachPolicyFormat.normalizedWire(
        "reject", options: unknownSenderOptions(), label: unknownSenderLabel(value:)) == "reject")
}

// MARK: - The approvals queue (family-safety.md § Reach approvals)

private func approval(
    kind: String, peerAddress: String = "", summary: String = "", peerHandle: String = ""
) -> FfiFamilyApprovalEntry {
    FfiFamilyApprovalEntry(
        supervisedActorId: Data(), supervisedHandle: "ward", kind: kind,
        peerActorId: Data(), peerAddress: peerAddress, messageId: Data(),
        summary: summary, peerHandle: peerHandle,
        // A `feed_source` item's key; empty for every other kind.
        bridgeId: "", operation: "", target: "", createdAt: 0)
}

// The display-text DECISION itself (mail_hold → peerAddress vs. every other
// kind → summary) is shared Rust now (`fauna_core::format::approval_display_text`,
// pinned by its own unit tests there) — these three only pin the Swift-side
// wiring: the FFI export's `nil` (the mail_hold null reverse-path) falls back
// to the localized no-sender label, exactly as `FamilyView.approvalRow` does.

@Test func aMailHoldRowRendersItsEnvelopeAddressNotTheEmptySummary() {
    // A mail hold's `summary` is deliberately ALWAYS empty (a subject line is
    // content, and the message is sealed to the ward), so binding it renders a
    // blank row with live Approve/Deny buttons — the bug windows shipped.
    let hold = approval(kind: "mail_hold", peerAddress: "stranger@example.com", summary: "")
    #expect((approvalDisplayText(entry: hold) ?? L.family.approvalNoSender) == "stranger@example.com")
}

@Test func aNullPathMailHoldRowRendersTheNoSenderLabel() {
    // The SMTP null reverse-path (`MAIL FROM:<>`) is truthfully an empty address
    // on the wire (family-safety.md § The mail gate, null-path rule); rendering
    // it verbatim would leave a blank row.
    let nullPath = approval(kind: "mail_hold", peerAddress: "")
    #expect((approvalDisplayText(entry: nullPath) ?? L.family.approvalNoSender) == "No sender (delivery notice)")
}

@Test func aContactRowRendersItsSummary() {
    let contact = approval(kind: "contact", summary: "alice wants to connect")
    #expect((approvalDisplayText(entry: contact) ?? L.family.approvalNoSender) == "alice wants to connect")
}

// MARK: - The supervised side's read-only summary

@Test func thePolicySummaryRendersEveryKnobFailClosed() {
    let summary = FamilyVM.policySummary(FfiReachPolicy(
        contactApproval: true,
        unknownSenderMail: "not-a-value-this-client-knows",
        federationContact: false,
        feedSources: "block",
        contentPolicy: nil,
        screenTime: nil,
        contentNotify: nil,
        unknownPeerDm: nil))

    #expect(summary.contains("Require my approval for new contacts: Enable"))
    #expect(summary.contains("Allow contact from other nests: Disable"))
    // The unparseable value must summarize as the strict option, exactly as the
    // editor renders it — never as "Allow".
    #expect(summary.contains("Unknown email senders: Hold for review"))
    #expect(summary.contains("New feed sources: Block"))
    #expect(!summary.contains("Unknown email senders: Allow"))
}

// The screen-time usage line folds into this SAME summary rather than a
// second element (family-safety.md § Screen time —
// "the ward's summary shows the same number the guardian sees"). Mirrors
// linux's `views/family.rs::policy_summary`.

@Test func thePolicySummaryFoldsInTheScreenTimeUsageLineWhenPresent() {
    let policy = FfiReachPolicy(
        contactApproval: false, unknownSenderMail: "allow", federationContact: true,
        feedSources: "allow", contentPolicy: nil,
        screenTime: FfiScreenTimePolicy(windowStart: nil, windowEnd: nil, dailyMinutes: 120),
        contentNotify: nil, unknownPeerDm: nil)

    let summary = FamilyVM.policySummary(policy, usageTodayMinutes: 45)
    #expect(summary.contains("Screen time today: 45 of 120 minutes"))
}

@Test func thePolicySummaryOmitsTheUsageLineWhenNoBudgetIsAccounted() {
    // `nil` usageTodayMinutes is the wire's own "no accounting without a
    // declared policy" contract — must render nothing at all, not a stray
    // "Screen time today: 0" line implying a budget that isn't set.
    let policy = FfiReachPolicy(
        contactApproval: false, unknownSenderMail: "allow", federationContact: true,
        feedSources: "allow", contentPolicy: nil, screenTime: nil,
        contentNotify: nil, unknownPeerDm: nil)

    let summary = FamilyVM.policySummary(policy, usageTodayMinutes: nil)
    #expect(!summary.contains("Screen time today"))
}

// MARK: - Guardian Notify's readout join

@Test func contentNoticesTextJoinsOneLinePerCategory() {
    let text = FamilyVM.contentNoticesText([
        FfiFamilyContentNotice(category: "nsfw", count: 2),
        FfiFamilyContentNotice(category: "spam", count: 1),
    ])
    #expect(text == "Adult content: 2 flagged today\nSpam: 1 flagged today")
}

@Test func contentNoticesTextIsEmptyForNoNotices() {
    #expect(FamilyVM.contentNoticesText([]).isEmpty)
}

// MARK: - Screen time's usage readout join

@Test func usageTodayTextWithABudgetNamesBothFigures() {
    #expect(FamilyVM.usageTodayText(usedMinutes: 45, budgetMinutes: 60)
        == "Screen time today: 45 of 60 minutes")
}

@Test func usageTodayTextWithNoBudgetNamesOnlyTheUsedMinutes() {
    // The wire's own contract: `usage_today_minutes` is Some only while a
    // caller has a daily budget set, so this arm is reachable only from a
    // stale/racing read — pinned anyway since the join itself must not
    // silently drop the "of {budget}" half without a budget crashing.
    #expect(FamilyVM.usageTodayText(usedMinutes: 10, budgetMinutes: nil)
        == "Screen time today: 10 minutes")
}

// MARK: - Screen time's editor round-trip (parse ↔ format, both shared Rust —
// pins the Swift-side wiring `loadEditor`/`savePolicy` drive, mirroring the
// fail-closed knob tests above)

@Test func aTypedWindowBoundParsesAndFormatsBackToTheSameText() throws {
    let minutes = try parseTimeOfDay(input: "21:05")
    #expect(minutes == UInt16(21 * 60 + 5))
    #expect(formatTimeOfDay(minutesFromMidnight: minutes!) == "21:05")
}

@Test func anEmptyWindowBoundParsesAsClearingIt() throws {
    #expect(try parseTimeOfDay(input: "") == Optional<UInt16>.none)
    #expect(try parseTimeOfDay(input: "   ") == Optional<UInt16>.none)
}

@Test func anUnparseableWindowBoundThrows() {
    #expect(throws: (any Error).self) { try parseTimeOfDay(input: "not a time") }
}

@Test func aTypedDailyBudgetParsesToWholeMinutesIncludingZero() throws {
    #expect(try parseDailyMinutes(input: "90") == UInt16(90))
    // Zero is the deliberate full-lock value (family-safety.md § Screen
    // time), NOT the same as an empty/cleared budget — must not throw and
    // must not collapse to `nil`.
    #expect(try parseDailyMinutes(input: "0") == UInt16(0))
    #expect(try parseDailyMinutes(input: "") == Optional<UInt16>.none)
}
