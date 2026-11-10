import Testing
import Foundation
@testable import FaunaKit

#if !FAUNA_EXCISE_PAYMENTS
// The §4 provider form's optional product (`subscription-provider-form-product`;
// `monetization.md` § Pillar 3 → *Configuration*): picking a configured kind
// pre-fills its saved product, a kind with no config starts empty, and save sends
// the field as shown — an emptied field clears the mapping. Mirrors android's
// `ProfileTiersContentTest` pre-fill/save pin and tui's `ProviderForm::product`.

private final class ScriptedProvidersAPI: APIClient {
    var providers: [FfiProviderItem] = []
    private(set) var sent: [(kind: String, tier: String, productRef: String?)] = []

    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }

    override func paymentProviderKinds() -> [String] { ["stripe", "lemonsqueezy"] }
    override func listSubscriptionTiers() async throws -> [FfiTierItem] { [] }
    override func listSubscriptionRequests() async throws -> [FfiPendingRequest] { [] }
    override func listPaymentProviders() async throws -> [FfiProviderItem] { providers }
    override func listPaymentClaims() async throws -> [FfiClaimItem] { [] }
    override func setPaymentProvider(kind: String, webhookSecret: String, tier: String,
                                     productRef: String?) async throws -> Bool {
        sent.append((kind, tier, productRef))
        return true
    }
}

private func provider(_ kind: String, product: String?) -> FfiProviderItem {
    FfiProviderItem(kind: kind, tier: "gold", createdAt: 0, lastVerifiedAt: nil,
                    lastRejectedAt: nil, productRef: product)
}

@MainActor
@Suite("SubscriptionsVM provider product")
struct SubscriptionsVMProviderProductTests {
    @Test("the form pre-fills the saved product of the picked kind, empty for an unconfigured kind")
    func prefillFollowsTheKind() async {
        let api = ScriptedProvidersAPI()
        api.providers = [provider("stripe", product: "plink_123")]
        let vm = SubscriptionsVM()
        await vm.configure(api: api, authorIdHex: "actor")

        vm.openProviderForm()
        #expect(vm.formProviderKind == "stripe")
        #expect(vm.formProviderProduct == "plink_123")

        vm.formProviderKind = "lemonsqueezy"
        #expect(vm.formProviderProduct.isEmpty)

        vm.formProviderKind = "stripe"
        #expect(vm.formProviderProduct == "plink_123")
    }

    @Test("save sends the product as shown, so an emptied field clears the mapping")
    func saveSendsTheProductAsShown() async {
        let api = ScriptedProvidersAPI()
        api.providers = [provider("stripe", product: "plink_123")]
        let vm = SubscriptionsVM()
        await vm.configure(api: api, authorIdHex: "actor")

        vm.openProviderForm()
        vm.formProviderTier = "gold"
        vm.formProviderSecret = "whsec"
        vm.formProviderProduct = "plink_456"
        await vm.saveProviderForm()

        vm.openProviderForm()
        vm.formProviderTier = "gold"
        vm.formProviderSecret = "whsec"
        vm.formProviderProduct = ""
        await vm.saveProviderForm()

        #expect(api.sent.map(\.productRef) == ["plink_456", ""])
    }
}
#endif
