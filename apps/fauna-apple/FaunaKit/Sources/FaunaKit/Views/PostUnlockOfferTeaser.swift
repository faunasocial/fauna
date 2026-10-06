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
/// reference (a non-https url still shows the button).
public struct PostUnlockOfferTeaser: View {
    public let postId: String
    public let offer: UnlockOfferView?
    public let vm: FeedVM

    public init(postId: String, offer: UnlockOfferView?, vm: FeedVM) {
        self.postId = postId
        self.offer = offer
        self.vm = vm
    }

    public var body: some View {
        if let offer {
            automationText(Ids.gatedPostPrice, offer.priceHint ?? "")
                .font(.caption2)
                .foregroundStyle(.secondary)
            if let url = offer.paymentUrl, !url.isEmpty {
                Button(L.subscriptions.paymentUrl) { openPaymentLink(url) }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.gatedPostPaymentLink)
                    .automationActivate(Ids.gatedPostPaymentLink) { openPaymentLink(url) }
            }
            Button(L.feed.post.buyButton) { Task { await buy() } }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.gatedPostBuyButton)
                .automationActivate(Ids.gatedPostBuyButton) { Task { await buy() } }
                .faunaGate("fauna.subscriptions.subscribe")
        }
    }

    private func openPaymentLink(_ url: String) {
        // Synchronous — `openPaymentURL` calls `setError` inline, before any
        // suspension, so there is no account-switch window to guard against
        // (`FeedVM.setClientErrorMessage` doc comment).
        openPaymentURL(url) { vm.setClientErrorMessage($0) }
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
