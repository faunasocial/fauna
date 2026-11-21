#if !FAUNA_KIDS
import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for `profile.md` § Errors & edge cases (the form case of
// `ui/README.md` § Offer only what is possible, rule 4) on the apple apps:
// `subscription-tier-form-save` and `subscription-claim-redeem-button` paint
// from `fauna_client_subscriptions::forms` over UniFFI, and each verb asks the
// same predicate first and refuses with its reason on `error-message` — never
// a silent return, never a hard-coded string.

@MainActor
private func reason(_ answer: Possibility) throws -> String {
    let text = try #require(answer.reasonText, "expected a blocked answer with a reason")
    #expect(!text.isEmpty)
    return text
}

@Suite("Profile forms — painted from the shared predicates")
@MainActor
struct ProfileFormPossibilityTests {
    @Test("subscription-tier-form-save: a blank name is blocked, and the save refuses with the same reason")
    func tierForm() async throws {
        let vm = SubscriptionsVM()
        vm.formName = "   "
        let why = try reason(vm.tierFormPossibility)
        await vm.saveForm()
        #expect(vm.errorMessage == why)
        vm.formName = "Gold"
        #expect(vm.tierFormPossibility.isPossible)
    }

#if !FAUNA_EXCISE_PAYMENTS
    @Test("subscription-claim-redeem-button: a blank code is blocked, and both doors refuse with the same reason")
    func redeem() async throws {
        let why = try reason(subscriptionRedeemCodePossibility(code: " "))
        #expect(subscriptionRedeemCodePossibility(code: "CODE-1").isPossible)

        // The payments plane answered available, so the code's own reason is
        // the one each door refuses with (the gate's are pinned in
        // `UnreadStateAnswersTests`).
        let open = [unreadStateFeatureRow("payments", affordance: "available")]
        let settings = SubscriptionSettingsVM()
        settings.applyFeaturesRead(open)
        settings.claimCodeInput = " "
        await settings.redeemClaim()
        #expect(settings.errorMessage == why)

        let offers = ProfileOffersVM()
        offers.applyFeaturesRead(open)
        offers.claimCodeInput = ""
        await offers.redeemClaim()
        #expect(offers.errorMessage == why)
    }
#endif
}
#endif
