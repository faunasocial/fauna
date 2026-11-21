import FaunaFFISwift
import Foundation
import Testing
@testable import FaunaKit

// The share-signals and clear-engagement controls, the external-apps switch,
// sign out everywhere else, member-review Remove and the subscriptions list
// paint from the shared answers tui uses (`docs/goal/ui/README.md` § Offer
// only what is possible; the rulings in `engagement-cues.md` § Layer B,
// `connected-apps.md` and `sessions.md` § Errors & edge cases,
// `succession-propagation.md` § Propagation and `subscriptions.md` § Pillar 1
// — UX): none is enabled before its read or with nothing to act on, and each
// press asks the answer again and refuses on `error-message`. The FaunaKit leg
// shared by macOS and iOS.

private let stillLoading = Possibility.blocked(reason: LocalizedText(key: "common.still_loading", args: [:]))

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

@MainActor
struct GatedControlAnswersTests {
    @Test("share signals waits for the feed manager, then the opt-in read")
    func shareSignalsWaitsForManagerThenRead() {
        #expect(reasonKey(shareSignalsPossibility(managerReady: false, read: .resolved)) == "errors.feed_not_ready")
        #expect(shareSignalsPossibility(managerReady: true, read: .pending) == stillLoading)
        #expect(shareSignalsPossibility(managerReady: true, read: .resolved) == .possible)
    }

    @Test("clear engagement waits only for the feed manager")
    func clearEngagementWaitsForManager() {
        #expect(reasonKey(clearEngagementPossibility(managerReady: false)) == "errors.feed_not_ready")
        #expect(clearEngagementPossibility(managerReady: true) == .possible)
    }

    #if !FAUNA_KIDS
    @Test("the external-apps switch is still loading before the visit's read, and its press refuses")
    func externalAppsSwitchWaitsForTheRead() async {
        let vm = ConnectedAppsVM()
        #expect(vm.externalAppsAnswer == stillLoading)
        await vm.setExternalAppsEnabled(false)
        #expect(vm.errorMessage == stillLoading.refusal,
                "a press before the read is said on error-message, never a guessed write")
    }
    #endif

    @Test("sign out everywhere else is still loading before the first read, and arming refuses")
    func revokeOthersWaitsForTheRead() {
        let vm = SessionsVM()
        #expect(vm.revokeOthersAnswer == stillLoading)
        vm.armRevokeOthers()
        #expect(!vm.revokeOthersArmed)
        #expect(vm.errorMessage == stillLoading.refusal)
    }

    @Test("sign out everywhere else has nothing to end when only this app's row is listed")
    func revokeOthersNeedsAnotherSignIn() {
        let own = FfiSessionRow(
            tokenId: "own", kind: LocalizedText(key: "sessions.kind_app", args: [:]), mark: nil,
            detail: LocalizedText(key: "sessions.detail", args: [:]),
            createdAt: 0, lastUsedAt: 0, expiresAt: 0, ipAddress: nil, canRevoke: false)
        var other = own
        other.tokenId = "other"
        other.canRevoke = true
        let onlyOwn = FfiSessionsSnapshot(rows: [own], error: nil)
        #expect(reasonKey(sessionsRevokeOthersPossibility(snapshot: onlyOwn, busy: false))
                == "sessions.no_other_sessions")
        let two = FfiSessionsSnapshot(rows: [own, other], error: nil)
        #expect(sessionsRevokeOthersPossibility(snapshot: two, busy: false) == .possible)
        #expect(reasonKey(sessionsRevokeOthersPossibility(snapshot: two, busy: true)) == "common.in_flight")
    }

    @Test("member-review Remove waits for the conversations session")
    func memberReviewRemoveWaitsForTheSession() {
        let vm = MemberReviewVM()
        #expect(reasonKey(vm.removeAnswer) == "settings.recovery_kit.review_remove_not_ready")
        #expect(memberReviewRemovePossibility(conversationsReady: true) == .possible)
    }

    @Test("the subscriptions list starts loading, not empty")
    func subscriptionsListStartsLoading() {
        let vm = SubscriptionSettingsVM()
        #expect(vm.mineRead.state == .pending)
        #expect(vm.subscriptions.isEmpty)
    }
}
