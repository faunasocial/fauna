import FaunaFFISwift
import Foundation
import Testing
@testable import FaunaKit

// The two payment-link gestures subscribe FIRST and only then open the checkout
// the shared rule composes, bound to the buyer's actor (`docs/goal/behavior/
// monetization.md` § Pillar 3 → *Buyer↔actor binding (Q4)*, the Q13 rules (1)
// and (2)): `subscription-offer-payment-link` on the OTHER-profile offers tab
// (`ProfileOffersVM.payTo`) and the feed's `gated-post-payment-link`
// (`PostUnlockOfferTeaser.pay` over `FeedVM.payUnlockOffer`). Mirrors tui's
// `Action::OpenPaymentLink` → `Op::Subscribe { checkout: Some(url), .. }` and
// `feed::Op::PayUnlockOffer`.

private struct StubFailure: Error {}

/// A scripted nest recording, in order, every subscribe the offers VM sends;
/// the opener the test hands `payTo` appends to the same log, so the test reads
/// the subscribe-before-open order straight off it.
private final class ScriptedCheckoutAPI: APIClient {
    var log: [String] = []
    var checkout: FfiCheckoutOpen? = FfiCheckoutOpen(
        reply: .queued(requestId: 7),
        url: "https://buy.stripe.com/test_abc?client_reference_id=bound")
    var refuse = false

    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }

    override func subscriptionOffersList(authorIdHex: String, home: String?) async throws -> [FfiTierItem] { [] }

    override func subscriptionStatusGet(authorIdHex: String) async throws -> FfiSubscriptionStatus {
        FfiSubscriptionStatus(tier: nil, expiresAt: nil, autoApprove: false, followPending: false)
    }

    override func subscriptionSubscribe(
        authorIdHex: String, tier: String, homeNestUrl: String? = nil
    ) async throws -> FfiSubscribeReply {
        log.append("subscribe \(tier) home=\(homeNestUrl ?? "-")")
        return .queued(requestId: 1)
    }

    override func subscriptionSubscribeAndCheckout(
        authorIdHex: String, tier: String, paymentUrl: String, homeNestUrl: String?
    ) async throws -> FfiCheckoutOpen? {
        log.append("checkout \(tier) \(paymentUrl) home=\(homeNestUrl ?? "-")")
        if refuse { throw StubFailure() }
        return checkout
    }
}

@MainActor
struct ProfileOfferPaymentLinkTests {
    private let author = String(repeating: "ab", count: 32)
    private let home = "https://home.example/"
    private let link = "https://buy.stripe.com/test_abc"

    @Test("the payment link subscribes across nests at the page home, then opens the bound checkout")
    func theLinkSubscribesBeforeItOpens() async {
        let api = ScriptedCheckoutAPI()
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        await vm.payTo("gold", paymentUrl: link, home: home) { api.log.append("open \($0.absoluteString)") }
        #expect(api.log == [
            "checkout gold \(link) home=\(home)",
            "open https://buy.stripe.com/test_abc?client_reference_id=bound",
        ])
        #expect(vm.pendingTiers.contains("gold"), "a queued reply reads Pending approval, as Subscribe's")
        #expect(vm.errorMessage == nil)
    }

    @Test("an approved reply lands the tier active before the checkout opens")
    func anApprovedReplyLandsActive() async {
        let api = ScriptedCheckoutAPI()
        api.checkout = FfiCheckoutOpen(reply: .approved(tier: "gold", expiresAt: nil), url: link)
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        var opened: [URL] = []
        await vm.payTo("gold", paymentUrl: link, home: home) { opened.append($0) }
        #expect(opened.map(\.absoluteString) == [link])
        #expect(!vm.pendingTiers.contains("gold"))
    }

    @Test("an unsafe link is the page error and sends nothing")
    func anUnsafeLinkSendsNothing() async {
        let api = ScriptedCheckoutAPI()
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        await vm.payTo("gold", paymentUrl: "http://evil.example/pay", home: home) {
            api.log.append("open \($0.absoluteString)")
        }
        #expect(api.log.isEmpty)
        #expect(vm.errorMessage == L.subscriptions.unsafePaymentUrl)
    }

    @Test("a refused subscribe is the page error and opens nothing")
    func aRefusedSubscribeOpensNothing() async {
        let api = ScriptedCheckoutAPI()
        api.refuse = true
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        var opened: [URL] = []
        await vm.payTo("gold", paymentUrl: link, home: home) { opened.append($0) }
        #expect(opened.isEmpty)
        #expect(vm.errorMessage != nil)
    }

    @Test("nothing to open opens nothing")
    func nilOpensNothing() async {
        let api = ScriptedCheckoutAPI()
        api.checkout = nil
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        var opened: [URL] = []
        await vm.payTo("gold", paymentUrl: link, home: home) { opened.append($0) }
        #expect(opened.isEmpty)
        #expect(vm.errorMessage == nil)
    }

    @Test("the row's plain Subscribe carries the page home too")
    func subscribeCarriesTheHome() async {
        let api = ScriptedCheckoutAPI()
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        await vm.subscribe("gold", home: home)
        #expect(api.log == ["subscribe gold home=\(home)"])
    }
}

@MainActor
struct GatedPostPaymentLinkTests {
    private let link = "https://buy.stripe.com/test_abc"

    @Test("the feed link runs the manager's subscribe-then-checkout before it opens the URL it returns")
    func theFeedLinkPaysBeforeItOpens() async {
        let vm = FeedVM()
        var log: [String] = []
        await PostUnlockOfferTeaser.pay(
            vm: vm, paymentUrl: link,
            checkout: { log.append("payUnlockOffer"); return "\(link)?client_reference_id=bound" },
            open: { log.append("open \($0.absoluteString)") })
        #expect(log == ["payUnlockOffer", "open \(link)?client_reference_id=bound"])
        #expect(vm.clientErrorMessage == nil)
    }

    @Test("an unsafe feed link is the page error and never reaches payUnlockOffer")
    func anUnsafeFeedLinkSendsNothing() async {
        let vm = FeedVM()
        var log: [String] = []
        await PostUnlockOfferTeaser.pay(
            vm: vm, paymentUrl: "javascript:alert(1)",
            checkout: { log.append("payUnlockOffer"); return link },
            open: { log.append("open \($0.absoluteString)") })
        #expect(log.isEmpty)
        #expect(vm.clientErrorMessage == L.subscriptions.unsafePaymentUrl)
    }

    @Test("nil from payUnlockOffer opens nothing")
    func nilOpensNothing() async {
        let vm = FeedVM()
        var opened: [URL] = []
        await PostUnlockOfferTeaser.pay(
            vm: vm, paymentUrl: link, checkout: { nil }, open: { opened.append($0) })
        #expect(opened.isEmpty)
        #expect(vm.clientErrorMessage == nil)
    }

    @Test("a refused subscribe lands feed.error_buy_unlock and opens nothing")
    func aRefusedFeedSubscribeOpensNothing() async {
        let vm = FeedVM()
        var opened: [URL] = []
        await PostUnlockOfferTeaser.pay(
            vm: vm, paymentUrl: link, checkout: { throw StubFailure() }, open: { opened.append($0) })
        #expect(opened.isEmpty)
        let text = DisplayError.message(StubFailure()) ?? ""
        #expect(vm.clientErrorMessage == L.feed.errorBuyUnlock(message: text))
    }
}
