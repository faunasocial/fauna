import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for the VISIBLE half of apple's offline gate — the caption
// `FaunaOfflineReason` renders beside a control the gate has desensitized
// (`account-data-plane.md` § The offline-mutation contract → *How a surface
// asks*; `ui/README.md` § Copy comprehensibility rule 5, "every disabled
// control the user can see has an on-screen reason within eyeshot").
//
// ⚠ **Why this file has to exist, stated plainly, because the reasoning is the
// interesting part.** The caption is deliberately **un-id'd chrome** — that is
// what lets it ship with no `ui.yaml` element id and therefore no rule-A
// approval, which is the finding android paid for and apple inherited
// (`account-data-plane.md` § Implementation status → the android leg). But
// android and apple do not have the same harness: android's reads arbitrary
// rendered text, and **apple's driver resolves elements through
// `AutomationRegistry`**, which by construction contains only registered ids.
// So on apple an un-id'd caption is invisible to every driver-level test, and
// the android finding transfers on the *approval* axis while NOT transferring
// on the *verification* axis.
//
// That is exactly the shape this project's authoring rules name as its most
// expensive kind of claim: a mechanism shipped with no test, excused by "a
// human has to look at it" — every later reader inherits the excuse and stops
// looking. So the mile is split. Everything except the
// pixels — which kinds caption, which connection words count as offline, what
// an unknown word does, that the caption and the paint cannot disagree — is a
// pure function of `(kind, connectionState)` and is pinned here. The human is
// left only the last inch: whether the caption *looks* right where it sits.
//
// What this file CANNOT grade, so nobody over-reads it: that the caption is
// actually placed beside its control, and that its `Text` is on screen. The
// first is what `check-offline-gate-kinds.py`'s **rule 5** enforces statically
// (a caption whose kind no control gates is an error); the second is the last
// inch above.
@Suite("Offline reason caption (the visible half of the gate)")
struct OfflineReasonCaptionTests {
    /// A class-3 kind — the only class that desensitizes (ruling 1).
    private static let onlineOnly = "fauna.backup.destination.remove"

    @Test("a disconnected class-3 control gets a non-empty reason")
    func disconnectedOnlineOnlyCaptions() {
        let caption = FaunaOfflineReason.captionText(
            kind: Self.onlineOnly, connectionState: "disconnected"
        )
        #expect(caption != nil)
        #expect(caption?.isEmpty == false)
    }

    @Test("a connected control captions NOTHING — a live control has no reason to give")
    func connectedCaptionsNothing() {
        #expect(
            FaunaOfflineReason.captionText(
                kind: Self.onlineOnly, connectionState: "connected"
            ) == nil
        )
    }

    // ⚠ The over-claim direction, and the one worth being loudest about. A
    // caption is a CLAIM to the user that they are offline. `FaunaClient` is
    // absent from the environment in a preview, in a presentation that loses
    // it, and in a view built before the session is injected — and the gate
    // maps that to the word `"unknown"` precisely so ruling 3 answers
    // *available*. If this ever returns a caption, the app tells the user the
    // nest is unreachable while it is perfectly reachable, which is worse than
    // saying nothing: the control beside it is live, so the caption and the
    // paint would be openly contradicting each other on screen.
    @Test("an UNKNOWN connection word captions nothing (ruling 3, made visible)")
    func unknownStateCaptionsNothing() {
        #expect(
            FaunaOfflineReason.captionText(
                kind: Self.onlineOnly, connectionState: "unknown"
            ) == nil
        )
    }

    // Ruling 2: an unregistered kind reads as available. A typo must therefore
    // caption nothing — which is silent, and is why rule 5 of
    // `check-offline-gate-kinds.py` catches the typo statically instead of
    // leaving this behaviour to be discovered on screen.
    @Test("an unregistered kind captions nothing (ruling 2), which is why rule 5 exists")
    func unregisteredKindCaptionsNothing() {
        #expect(
            FaunaOfflineReason.captionText(
                kind: "fauna.backup.destination.remov", connectionState: "disconnected"
            ) == nil
        )
    }

    // The property that makes one call site honest: the caption and the
    // control's own desensitizing come from the SAME `offlineAffordance` call,
    // so "captioned" and "disabled" cannot disagree for any connection word.
    // Written as a sweep rather than three asserts so a newly-recognised
    // offline word cannot quietly land on one side only.
    @Test("captioned iff gated, for every connection word the transport can report")
    func captionAgreesWithTheGateForEveryWord() {
        for word in ["connected", "connecting", "disconnected", "reconnecting", "unknown", ""] {
            let available = offlineAffordance(
                kind: Self.onlineOnly, connectionState: word
            ).available
            let captioned = FaunaOfflineReason.captionText(
                kind: Self.onlineOnly, connectionState: word
            ) != nil
            #expect(
                captioned == !available,
                """
                caption/paint disagreement on connection word \(word.isEmpty ? "<empty>" : word): \
                available=\(available) captioned=\(captioned). These read one verdict from one \
                call precisely so a user can never see a dead control with no reason, or a live \
                control captioned as offline.
                """
            )
        }
    }
}
