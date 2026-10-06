import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for who may clear an `AppMessages` banner slot.
//
// A page can hold two banners showing the SAME text at once. The iOS
// conversations page is the live case: the list root's page-level
// `error-message` and the pushed thread screen's banner both render the
// selected thread's failed send. The list root sits under the pushed screen,
// so SwiftUI tells it to disappear while the thread screen's banner is still
// on screen. Guarded only on "the slot still holds my text", that disappear
// emptied the slot the visible banner had just filled, and the state protocol
// then read `messages.error = null` while the refusal was plainly on screen:
// the iOS witness for `email-in-conversations` outcome 10 timed out that way,
// only when the two events landed in that order (the app log showed the two
// appears 13 ms apart and nothing after).
//
// So a banner may only retract its OWN showing: the slot mirrors the newest
// banner still on screen, whichever instance filled it.
//
// `.serialized` because every case mutates the same process-wide statics.
@MainActor
@Suite("Banner slot ownership", .serialized)
struct BannerSlotOwnershipTests {
    private let refusal = "Could not send this message: You have reached your sending limit for now."

    private func cleanSlate() {
        AppMessages.resetBannerSlots()
    }

    /// The failing case: two banners, one text, the covered one disappears.
    @Test func aCoveredTwinDisappearingLeavesTheVisibleBannersMessage() {
        cleanSlate()
        let listRoot = UUID(), threadScreen = UUID()

        AppMessages.bannerAppeared(.error, owner: threadScreen, message: refusal)
        AppMessages.bannerAppeared(.error, owner: listRoot, message: refusal)
        AppMessages.bannerDisappeared(.error, owner: listRoot, message: refusal)

        #expect(AppMessages.error == refusal, """
            a banner that is still on screen lost its message to a twin's \
            disappear. The harness reads `messages.error` and sees nothing \
            while the refusal is visible. Got \(String(describing: AppMessages.error)).
            """)
    }

    /// Same pair, the other order: the covered banner fills the slot first.
    @Test func eitherOrderOfTheTwinsKeepsTheMessage() {
        cleanSlate()
        let listRoot = UUID(), threadScreen = UUID()

        AppMessages.bannerAppeared(.error, owner: listRoot, message: refusal)
        AppMessages.bannerAppeared(.error, owner: threadScreen, message: refusal)
        AppMessages.bannerDisappeared(.error, owner: listRoot, message: refusal)

        #expect(AppMessages.error == refusal)
    }

    /// The last banner going away still empties the slot: a clean page reads
    /// no error.
    @Test func theLastBannerLeavingEmptiesTheSlot() {
        cleanSlate()
        let a = UUID(), b = UUID()

        AppMessages.bannerAppeared(.error, owner: a, message: refusal)
        AppMessages.bannerAppeared(.error, owner: b, message: refusal)
        AppMessages.bannerDisappeared(.error, owner: a, message: refusal)
        AppMessages.bannerDisappeared(.error, owner: b, message: refusal)

        #expect(AppMessages.error == nil)
    }

    /// A newer banner leaving hands the slot back to the older one still on
    /// screen, instead of blanking it.
    @Test func aNewerBannerLeavingRevealsTheOlderOnesMessage() {
        cleanSlate()
        let page = UUID(), sheet = UUID()

        AppMessages.bannerAppeared(.error, owner: page, message: "Could not reach the nest")
        AppMessages.bannerAppeared(.error, owner: sheet, message: "Name is taken")
        #expect(AppMessages.error == "Name is taken")

        AppMessages.bannerDisappeared(.error, owner: sheet, message: "Name is taken")

        #expect(AppMessages.error == "Could not reach the nest")
    }

    /// One banner whose text is replaced in place (`.id(message)`): SwiftUI may
    /// deliver the new text's appear BEFORE the old text's disappear. The late
    /// disappear must not retract the new text.
    @Test func aTextSwapWithTheOldDisappearArrivingLateKeepsTheNewText() {
        cleanSlate()
        let banner = UUID()

        AppMessages.bannerAppeared(.error, owner: banner, message: "Checking your claim…")
        AppMessages.bannerAppeared(.error, owner: banner, message: "This nest is already claimed")
        AppMessages.bannerDisappeared(.error, owner: banner, message: "Checking your claim…")

        #expect(AppMessages.error == "This nest is already claimed")
    }

    /// The nav clear (`applyNavPatch`'s `AppMessages.error = nil`) is not
    /// undone by a later disappear: errors belong to the page that raised
    /// them, and the page is gone.
    @Test func aDisappearAfterTheNavClearDoesNotResurrectAMessage() {
        cleanSlate()
        let old = UUID(), other = UUID()

        AppMessages.bannerAppeared(.error, owner: other, message: "Stale page error")
        AppMessages.bannerAppeared(.error, owner: old, message: refusal)
        AppMessages.error = nil
        AppMessages.bannerDisappeared(.error, owner: old, message: refusal)

        #expect(AppMessages.error == nil)
    }

    /// The warning and info slots follow the same rule.
    @Test func warningAndInfoSlotsAreOwnedTheSameWay() {
        cleanSlate()
        let a = UUID(), b = UUID()

        AppMessages.bannerAppeared(.warning, owner: a, message: "Slow connection")
        AppMessages.bannerAppeared(.warning, owner: b, message: "Slow connection")
        AppMessages.bannerDisappeared(.warning, owner: a, message: "Slow connection")
        AppMessages.bannerAppeared(.info, owner: a, message: "Synced")
        AppMessages.bannerAppeared(.info, owner: b, message: "Synced")
        AppMessages.bannerDisappeared(.info, owner: b, message: "Synced")

        #expect(AppMessages.warning == "Slow connection")
        #expect(AppMessages.info == "Synced")
    }
}
