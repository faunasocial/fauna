import FaunaFFISwift
import Foundation
import Testing
@testable import FaunaKit

#if !FAUNA_KIDS && !FAUNA_EXCISE_PAYMENTS
// The claim-redeem pair's two doors (`docs/goal/behavior/monetization.md`
// § Pillar 3 → *Which nest an in-app redeem binds on* → *The doors*;
// `docs/goal/ui/profile.md` § Layout & flow → *Another's profile*): the OTHER
// profile Tiers tab redeems the viewed creator's code at the page's actor home,
// and the `subscription-settings` page redeems with no home. Both ride the one
// shared `PaymentsClient::claims_redeem_across_nests`. Mirrors the tui tests of
// `apps/fauna-tui/src/profile/mod.rs`.

/// One recorded across-nests redeem call.
private struct RedeemCall: Equatable {
    let code: String
    let authorIdHex: String
    let authorHomeUrl: String?
}

/// A scripted nest: the offers/status reads answer empty, every across-nests
/// redeem is recorded, and `refusal` (when set) is thrown instead of a reply.
private final class ScriptedRedeemAPI: APIClient {
    private(set) var redeems: [RedeemCall] = []
    private(set) var statusReads = 0
    var queued = true
    var refusal: Error?

    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }

    override func subscriptionOffersList(authorIdHex: String, home: String?) async throws -> [FfiTierItem] { [] }

    override func subscriptionStatusGet(authorIdHex: String) async throws -> FfiSubscriptionStatus {
        statusReads += 1
        return FfiSubscriptionStatus(tier: nil, expiresAt: nil, autoApprove: false, followPending: false)
    }

    override func subscriptionMineList() async throws -> [FfiMineSubscription] { [] }

    override func redeemPaymentClaimAcrossNests(
        code: String, authorIdHex: String, authorHomeUrl: String?
    ) async throws -> FfiClaimRedeemReply {
        redeems.append(RedeemCall(code: code, authorIdHex: authorIdHex, authorHomeUrl: authorHomeUrl))
        if let refusal { throw refusal }
        return FfiClaimRedeemReply(
            author: hex_to_data(authorIdHex), tier: "gold", validUntil: nil, queued: queued)
    }
}

@MainActor
struct ClaimRedeemAcrossNestsTests {
    private let author = String(repeating: "ab", count: 32)
    private let home = "https://author-home.test"

    @Test("the profile redeem goes to the viewed creator's home with the trimmed code")
    func theProfileRedeemGoesToTheViewedCreatorsHome() async {
        let api = ScriptedRedeemAPI()
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        vm.claimCodeInput = "  CODE42 "
        await vm.redeemClaim(home: home)
        #expect(api.redeems == [RedeemCall(code: "CODE42", authorIdHex: author, authorHomeUrl: home)])
    }

    @Test("a blank profile redeem is a no-op, never a nest round-trip")
    func aBlankProfileRedeemIsANoOp() async {
        let api = ScriptedRedeemAPI()
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        vm.claimCodeInput = "   "
        await vm.redeemClaim(home: home)
        #expect(api.redeems.isEmpty)
    }

    @Test("a taken code leaves the input, marks the tier pending and re-reads the status")
    func aTakenCodeClearsTheInputAndMarksPending() async {
        let api = ScriptedRedeemAPI()
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        let readsBefore = api.statusReads
        vm.claimCodeInput = "CODE42"
        await vm.redeemClaim(home: home)
        #expect(vm.claimCodeInput == "")
        #expect(vm.status(for: "gold") == .pending)
        #expect(vm.errorMessage == nil)
        #expect(api.statusReads > readsBefore, "the redeem re-reads offers + status")
    }

    @Test("a refused code stays in the input and surfaces on the page's error")
    func aRefusedCodeStays() async {
        let api = ScriptedRedeemAPI()
        api.refusal = APIError.ffiError("fauna.payments.claim_not_found")
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        vm.claimCodeInput = "CODE42"
        await vm.redeemClaim(home: home)
        #expect(vm.claimCodeInput == "CODE42")
        #expect(vm.errorMessage != nil)
    }

    @Test("a fresh profile open drops a code typed for the previous creator")
    func aFreshOpenDropsTheCode() async {
        let api = ScriptedRedeemAPI()
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        vm.claimCodeInput = "OTHER"
        await vm.configure(api: api, authorIdHex: String(repeating: "cd", count: 32))
        #expect(vm.claimCodeInput == "")
    }

    @Test("the settings redeem passes no home and the redeemer's own id in the author slot")
    func theSettingsRedeemPassesNoHome() async throws {
        let api = ScriptedRedeemAPI()
        api.primeSecret(String(repeating: "77", count: 32))
        let me = try #require(api.boundActorIdHex)
        let vm = SubscriptionSettingsVM()
        await vm.configure(api: api)
        vm.claimCodeInput = " CODE42 "
        await vm.redeemClaim()
        #expect(api.redeems == [RedeemCall(code: "CODE42", authorIdHex: me, authorHomeUrl: nil)])
        #expect(vm.claimCodeInput == "")
        #expect(vm.errorMessage == nil)
    }
}
#endif
