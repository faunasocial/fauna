import FaunaFFISwift
import Foundation
import Testing
@testable import FaunaKit

// An unread state is not possible (`docs/goal/ui/README.md` § Offer only what
// is possible, rules 2, 5 and 6; `docs/goal/ui/profile.md` and
// `docs/goal/behavior/notifications.md` § Errors & edge cases): every
// affordance whose precondition a read decides answers `Possibility` — blocked
// `common.still_loading` before the read, `common.load_failed` after a failed
// one — and its action asks the same answer and refuses with it on
// `error-message`; a per-feature gate whose row is hidden or missing is absent.
// The FaunaKit leg shared by macOS and iOS; mirrors tui's tests of apps 6045
// and android's of apps 6102.

private let stillLoading = Possibility.blocked(reason: LocalizedText(key: "common.still_loading", args: [:]))
private let loadFailed = Possibility.blocked(reason: LocalizedText(key: "common.load_failed", args: [:]))

/// One `features.status` row, as the courtesy read answers it.
func unreadStateFeatureRow(
    _ feature: String, affordance: String, restriction: LocalizedText? = nil
) -> FfiFeatureRow {
    FfiFeatureRow(
        feature: feature,
        name: LocalizedText(key: "features.\(feature)", args: [:]),
        availability: affordance == "available" ? "available" : "restricted",
        deniedBy: nil, cells: [], perOperationMax: nil, perOperationMaxTier: nil,
        unit: "", affordance: affordance, restriction: restriction,
        status: LocalizedText(key: "features.status_\(affordance)", args: [:]))
}

/// A restriction whose `{window}` argument is itself an i18n key.
private let dayRestriction = LocalizedText(
    key: "features.restriction_exhausted", args: ["window": "features.window_day"])

@MainActor
struct UnreadStateAnswersTests {
    // MARK: - The features gate (shared painter)

    @Test("the features gate: loading, failed, absent, blocked with the nested window, possible")
    func theFeaturesGateMapsEveryReadState() {
        #expect(featureGateAnswer(rows: nil, feature: "payments") == stillLoading)
        #expect(featureGateAnswer(rows: nil, failed: true, feature: "payments") == loadFailed)
        #expect(featureGateAnswer(rows: [], feature: "payments") == .absent,
                "a loaded read with no row: the nest has no plane")
        #expect(featureGateAnswer(rows: [unreadStateFeatureRow("payments", affordance: "hidden")],
                                  feature: "payments") == .absent)
        #expect(featureGateAnswer(rows: [unreadStateFeatureRow("payments", affordance: "available")],
                                  feature: "payments") == .possible)

        let blocked = featureGateAnswer(
            rows: [unreadStateFeatureRow("payments", affordance: "disabled", restriction: dayRestriction)],
            feature: "payments")
        guard case .blocked(let reason) = blocked else {
            Issue.record("a disabled row is blocked, got \(blocked)")
            return
        }
        #expect(reason.key == "features.restriction_exhausted")
        #expect(reason.args["window"] == L.lookup("features.window_day"),
                "the nested window word is resolved before painting")
    }

    // MARK: - Notifications: mark-read and clear wait on the first read

    @Test("mark all read and clear all are blocked still loading before the first read, and refuse")
    func notificationsActionsWaitOnTheFirstRead() async {
        let vm = NotificationsVM()
        #expect(vm.markAllReadAnswer == stillLoading)
        #expect(vm.clearAllAnswer == stillLoading)

        await vm.markAllRead()
        #expect(vm.errorMessage == stillLoading.refusal)
        vm.errorMessage = nil
        await vm.clearAll()
        #expect(vm.errorMessage == stillLoading.refusal)
    }

    @Test("a failed first read blocks with load-failed; a resolved one opens both, clear only with rows")
    func notificationsActionsAfterTheRead() {
        let vm = NotificationsVM()
        vm.failReadForTest()
        #expect(vm.markAllReadAnswer == loadFailed)
        #expect(vm.clearAllAnswer == loadFailed)

        vm.seedForTest(notifications: [], unreadCount: 0)
        guard case .blocked(let unreadReason) = vm.markAllReadAnswer else {
            Issue.record("nothing unread: mark all read is blocked")
            return
        }
        #expect(unreadReason.key == "notifications.nothing_unread")
        vm.seedForTest(notifications: [], unreadCount: 1)
        #expect(vm.markAllReadAnswer == .possible)
        guard case .blocked(let reason) = vm.clearAllAnswer else {
            Issue.record("an empty page has nothing to clear")
            return
        }
        #expect(reason.key == "notifications.nothing_to_clear")

        vm.reset()
        #expect(vm.markAllReadAnswer == stillLoading, "a reset page has not been read")
    }

    // MARK: - Profile block

    @Test("block is blocked still loading until the open-time edge read lands")
    func blockWaitsOnTheEdgeRead() {
        var edge = ProfileBlockEdge()
        #expect(edge.answer == stillLoading)
        edge.applyRead(blocked: false)
        #expect(edge.answer == .possible)
        edge.reset()
        #expect(edge.answer == stillLoading)
        edge.applyPress(blocked: true)
        #expect(edge.answer == .possible)
    }

    @Test("a failed edge read leaves block blocked load-failed, never live on a guess")
    func blockStaysBlockedAfterAFailedEdgeRead() {
        var edge = ProfileBlockEdge()
        edge.applyReadFailed()
        #expect(edge.answer == loadFailed)
        #expect(!edge.labelBlocked)
        // The viewer's own press after it knows the edge: the toggle is live.
        edge.applyPress(blocked: true)
        #expect(edge.answer == .possible)
        // A fresh open forgets the failure.
        edge.reset()
        #expect(edge.answer == stillLoading)
        // A late read after the press never rewinds it, failed or not.
        edge.applyPress(blocked: false)
        edge.applyReadFailed()
        #expect(edge.answer == .possible)
    }

    // MARK: - Profile follow

    #if !FAUNA_KIDS
    @Test("follow is blocked still loading before the status read, and the press refuses")
    func followWaitsOnTheStatusRead() async {
        let vm = ProfileOffersVM()
        #expect(vm.followAnswer == stillLoading)
        await vm.toggleFollow()
        #expect(vm.errorMessage == stillLoading.refusal)
    }

    @Test("a failed status read blocks follow with load-failed; an answered one opens it")
    func followAfterTheStatusRead() {
        let vm = ProfileOffersVM()
        vm.applyStatusRead(nil)
        #expect(vm.followAnswer == loadFailed)
        vm.applyStatusRead((tier: nil, followPending: false))
        #expect(vm.followAnswer == .possible)
        vm.applyStatusRead(nil)
        #expect(vm.followAnswer == .possible, "a later failure keeps the answered read")
        vm.reset()
        #expect(vm.followAnswer == stillLoading)
    }
    #endif

    // MARK: - Claim redeem (profile and Settings → Subscriptions)

    #if !FAUNA_KIDS && !FAUNA_EXCISE_PAYMENTS
    @Test("the profile redeem waits on the features read and refuses with its reason")
    func theProfileRedeemWaitsOnTheFeaturesRead() async {
        let vm = ProfileOffersVM()
        vm.claimCodeInput = "CODE42"
        #expect(vm.redeemAnswer == stillLoading)
        await vm.redeemClaim()
        #expect(vm.errorMessage == stillLoading.refusal)

        vm.applyFeaturesRead(nil)
        #expect(vm.redeemAnswer == loadFailed)
        vm.applyFeaturesRead([unreadStateFeatureRow("payments", affordance: "disabled", restriction: dayRestriction)])
        guard case .blocked = vm.redeemAnswer else {
            Issue.record("a disabled payments row blocks the redeem")
            return
        }
        await vm.redeemClaim()
        #expect(vm.errorMessage == vm.redeemAnswer.refusal)

        vm.applyFeaturesRead([unreadStateFeatureRow("payments", affordance: "hidden")])
        #expect(vm.redeemAnswer == .absent, "a hidden plane leaves the redeem block out")
        vm.applyFeaturesRead([])
        #expect(vm.redeemAnswer == .absent, "no payments row: the nest has no plane")
        vm.applyFeaturesRead([unreadStateFeatureRow("payments", affordance: "available")])
        #expect(vm.redeemAnswer == .possible)
    }

    @Test("the settings redeem waits on the features read and refuses with its reason")
    func theSettingsRedeemWaitsOnTheFeaturesRead() async {
        let vm = SubscriptionSettingsVM()
        vm.claimCodeInput = "CODE42"
        #expect(vm.redeemAnswer == stillLoading)
        await vm.redeemClaim()
        #expect(vm.errorMessage == stillLoading.refusal)

        vm.applyFeaturesRead(nil)
        #expect(vm.redeemAnswer == loadFailed)
        vm.applyFeaturesRead([unreadStateFeatureRow("zaps", affordance: "available")])
        #expect(vm.redeemAnswer == .absent, "a read with no payments row is absent")
        vm.applyFeaturesRead([unreadStateFeatureRow("payments", affordance: "available")])
        #expect(vm.redeemAnswer == .possible)
    }
    #endif

    // MARK: - Nostr zap-signer designate

    #if !FAUNA_KIDS && !FAUNA_EXCISE_PAYMENTS
    @Test("the designate waits on the zaps gate, which outranks a valid draft, and refuses with it")
    func theDesignateWaitsOnTheZapsGate() async {
        let vm = NostrVM()
        vm.zapSignerPubkeyInput = String(repeating: "ab", count: 32)
        #expect(vm.zapSignerGate == stillLoading)
        #expect(vm.zapSignerAddAnswer == stillLoading, "the plane's reason wins over a valid draft")
        await vm.addZapSigner()
        #expect(vm.errorMessage == stillLoading.refusal)

        vm.applyZapSignerFeaturesRead(nil)
        #expect(vm.zapSignerGate == loadFailed)
        vm.applyZapSignerFeaturesRead([unreadStateFeatureRow("zaps", affordance: "hidden")])
        #expect(vm.zapSignerGate == .absent, "a hidden zaps row leaves the designate row out")
        vm.applyZapSignerFeaturesRead([unreadStateFeatureRow("zaps", affordance: "available")])
        #expect(vm.zapSignerAddAnswer == .possible)
    }
    #endif
}
