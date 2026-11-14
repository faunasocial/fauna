// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// The sold-post buyer teaser (gap (2c), `monetization.md` § Per-post pay-to-unlock →
/// *the buyer's price read is post-addressed*) — price + optional external payment
/// link + buy affordance, present iff `fauna.subscriptions.post_unlock.get` resolved
/// an offer for this post (the caller's fire-once `resolvePostUnlockOffer` trigger,
/// mirrored on `resolveMedia`/`resolvePostTips`). One component for BOTH the feed
/// list card and the post-detail pane (`GatedPostBadge`/`TipSurface`'s shared-view
/// precedent), shared between both apple apps.
///
/// Mirrors linux `post_list.rs`'s `top_line` teaser group and windows'
/// `FeedPage.xaml`/`.cs` — same three ids, same presence
/// conditions: `gated-post-price` + `gated-post-buy-button` iff the offer
/// resolved at all, `gated-post-payment-link` iff it additionally carries a
/// `payment_url`. The payment link's https-only guard fires on click via the
/// shared `isSafePaymentUrl` UniFFI export (F-CL2 anti-phishing-redirect class) — the same check `ProfileView.swift`'s
/// `subscription-offer-payment-link` applies — not on visibility, matching the
/// reference (a non-https url still shows the button). A safe link subscribes
/// first and opens the checkout the shared rule binds to the buyer after
/// (`FeedVM.payUnlockOffer`; `monetization.md` § Pillar 3 → *Buyer↔actor
/// binding (Q4)*).
public struct PostUnlockOfferTeaser: View {
    public let postId: String
    public let offer: UnlockOfferView?
    /// `PostSummary.unlockPurchasePending` already narrowed by the caller to a
    /// post that is not open (`!gatedUnlocked`).
    public let purchasePending: Bool
    public let vm: FeedVM
    @Environment(\.openURL) private var openURL

    public init(postId: String, offer: UnlockOfferView?, purchasePending: Bool = false, vm: FeedVM) {
        self.postId = postId
        self.offer = offer
        self.purchasePending = purchasePending
        self.vm = vm
    }

    /// The price, the external payment link and the buy affordance are the
    /// money plane's buyer half (dynamic-features.md § Platform-family surface
    /// excision → *The price-and-route class*): a store-safe build shows a sold
    /// post as an ordinary gated post — the badge and nothing more. The resolve
    /// that fills `offer` is an ungated `fauna.subscriptions.*` read, so the
    /// render carries its own condition (the inert-record trap).
    public var body: some View {
        #if !FAUNA_EXCISE_PAYMENTS
        if let offer {
            automationText(Ids.gatedPostPrice, offer.priceHint ?? "")
                .font(.caption2)
                .foregroundStyle(.secondary)
            if let url = offer.paymentUrl, !url.isEmpty {
                Button(L.subscriptions.paymentUrl) { openPaymentLink(url) }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.gatedPostPaymentLink)
                    .automationActivate(Ids.gatedPostPaymentLink) { openPaymentLink(url) }
                    // The link subscribes before it opens the checkout (Q13
                    // rule (1)), so it is gated like the buy button.
                    .faunaGate("fauna.subscriptions.subscribe")
            }
            Button(L.feed.post.buyButton) { Task { await buy() } }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.gatedPostBuyButton)
                .automationActivate(Ids.gatedPostBuyButton) { Task { await buy() } }
                .faunaGate("fauna.subscriptions.subscribe")
        } else if purchasePending {
            // Bought, the author's grant still on its way (`monetization.md` §
            // Per-post pay-to-unlock → *a completed buy must not be re-offered*):
            // the shared marker the buy sets and every later declined resolve
            // re-sets. Never set while the offer is (the buy clears it in the same
            // write), so it takes the buy button's place — linux `post_list.rs`'s
            // `gated-post-purchase-pending` label, same condition.
            automationText(Ids.gatedPostPurchasePending, L.feed.post.purchasePending)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        #else
        EmptyView()
        #endif
    }

    private func openPaymentLink(_ url: String) {
        let openURL = openURL
        Task {
            await Self.pay(
                vm: vm, paymentUrl: url,
                checkout: { [vm, postId] in try await vm.payUnlockOffer(postId: postId) },
                open: { ExternalURLOpener.open($0, with: openURL) })
        }
    }

    /// `openPaymentLink`'s body: a URL the shared `isSafePaymentUrl` refuses is
    /// the page error and nothing is sent (a synchronous write — no await has
    /// happened, so no account-switch window); otherwise `checkout` (the
    /// manager's subscribe-then-compose) runs, and only the URL it returns is
    /// handed to `open` — the app's one external opener, `ExternalURLOpener`.
    /// `nil` opens nothing; a failure lands `feed.error_buy_unlock` the way
    /// ``buy(vm:purchase:)`` does, behind the same generation captured before
    /// the await — and an account switch mid-await opens nothing either.
    @MainActor
    static func pay(
        vm: FeedVM, paymentUrl: String,
        checkout: () async throws -> String?,
        open: @MainActor (URL) -> Void
    ) async {
        guard isSafePaymentUrl(url: paymentUrl) else {
            vm.setClientErrorMessage(L.subscriptions.unsafePaymentUrl)
            return
        }
        let generation = vm.managerGeneration
        do {
            let url = try await checkout()
            guard generation == vm.managerGeneration else { return }
            if let url, let parsed = URL(string: url) { open(parsed) }
        } catch {
            guard let text = DisplayError.message(error) else { return }
            vm.landClientErrorMessage(generation: generation, message: L.feed.errorBuyUnlock(message: text))
        }
    }

    /// Buy the sold post's unlock tier off the resolved teaser offer — the existing
    /// subscribe flow against the resolved offer's tier, queued pending the author's
    /// own §2 approve (a client-minted unlock tier is never `auto_approve`). Errors
    /// surface through the page's `error-message` (`clientErrorMessage`, mirrors
    /// linux's `error_label`/windows' `ErrorBar`) — the interaction bar has no
    /// dedicated per-card error slot, matching this repo's other post-card verbs.
    ///
    /// A thin caller: the awaited buy rides into `buy(vm:purchase:)` below as a
    /// closure, the same unit-tier seam `FeedPostActionsButton`'s web-publish
    /// verbs use.
    private func buy() async {
        await Self.buy(vm: vm) { _ = try await vm.buyUnlockOffer(postId: postId) }
    }

    /// `buy`'s guarded body. Generation captured before the await — same
    /// account-switch guard as `FeedPostActionsButton`'s web-publish verbs
    /// (`account-scoping.md` § The scoping taxonomy, `:208-236`); `CallSiteCaptureOrderingTests` executes the
    /// ordering with a purchase that suspends across a `reset()` and throws.
    @MainActor
    static func buy(vm: FeedVM, purchase: () async throws -> Void) async {
        let generation = vm.managerGeneration
        do {
            try await purchase()
        } catch {
            guard let text = DisplayError.message(error) else { return }
            vm.landClientErrorMessage(generation: generation, message: L.feed.errorBuyUnlock(message: text))
        }
    }
}
#endif // !FAUNA_KIDS
