import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for `Possibility.swift`, the apple apps' one painter of the
// shared possibility answer (`ui/README.md` § Offer only what is possible,
// rule 6). The painter's whole decision — shown or not, enabled or not, which
// reason — is a pure function of the answer, pinned here for each of the three
// cases; whether the caption LOOKS right where it sits is the last inch.
@Suite("Possibility painter (the shared answer, painted once)")
struct PossibilityPainterTests {
    private static let reasonKey = "onboarding.vps_config.status_pick_provider"
    private static let blocked = Possibility.blocked(
        reason: LocalizedText(key: reasonKey, args: [:])
    )

    @Test("possible: shown, enabled, no reason")
    func possible() {
        #expect(Possibility.possible.isShown)
        #expect(Possibility.possible.isPossible)
        #expect(Possibility.possible.reasonText == nil)
    }

    @Test("blocked: shown, disabled, its reason rendered through i18n")
    func blocked() {
        #expect(Self.blocked.isShown)
        #expect(!Self.blocked.isPossible)
        let text = Self.blocked.reasonText
        #expect(text?.isEmpty == false)
        #expect(text != Self.reasonKey, "the reason must render, not echo its key")
    }

    @Test("absent: not shown, and nothing to explain")
    func absent() {
        #expect(!Possibility.absent.isShown)
        #expect(!Possibility.absent.isPossible)
        #expect(Possibility.absent.reasonText == nil)
    }

    @Test("and: absent wins, then the receiver's reason, then the other's")
    func andFolds() {
        let other = Possibility.blocked(reason: LocalizedText(key: "common.needs_nest", args: [:]))
        #expect(Possibility.possible.and(.possible).isPossible)
        #expect(Possibility.possible.and(other).reasonText == other.reasonText)
        #expect(Self.blocked.and(other).reasonText == Self.blocked.reasonText)
        #expect(!Self.blocked.and(.absent).isShown)
        #expect(!Possibility.absent.and(.possible).isShown)
    }

    // The offline gate is this painter fed by `offlinePossibility`: its answer
    // for a class-3 kind is exactly blocked-with-a-reason offline and possible
    // connected, so `faunaGate` and `FaunaOfflineReason` read one value.
    @Test("the offline gate's answer is a Possibility the painter reads directly")
    func offlineGateFeedsThePainter() {
        let kind = "fauna.backup.destination.remove"
        let offline = offlinePossibility(kind: kind, connectionState: "disconnected")
        #expect(offline.isShown && !offline.isPossible && offline.reasonText != nil)
        #expect(
            FaunaOfflineReason.captionText(kind: kind, connectionState: "disconnected")
                == offline.reasonText
        )
        #expect(offlinePossibility(kind: kind, connectionState: "connected").isPossible)
    }
}
