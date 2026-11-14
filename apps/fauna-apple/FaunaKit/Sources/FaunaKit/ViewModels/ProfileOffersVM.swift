import SwiftUI
import FaunaFFISwift

/// View-model for the profile **OTHER-profile offers section** (subscriber browse
/// — `monetization.md` § Pillar 1, surface 2), shared by macOS + iOS. Reads
/// another creator's offered tiers (`offers_list_across_nests` from the page's
/// actor-home slot — relayed to the creator's home when it is another nest,
/// `subscriptions.md` § Pillar 1 → *The offers browse across nests*) + the
/// viewer's status (`status_get`, this nest), and subscribes per row at the same
/// home. The free "followers" tier is excluded
/// (it is the header `profile-follow-button`'s job). Observer-free: re-reads after
/// each subscribe. Mirrors linux `apps/fauna-linux/src/views/profile/offers.rs` +
/// android `ProfileOffersVM`.
@MainActor @Observable
public final class ProfileOffersVM {
    /// The creator's paid offered tiers (the free "followers" tier filtered out).
    public private(set) var offers: [FfiTierItem] = []
    /// The viewer's currently-held tier name (from `status_get`), if any.
    public private(set) var statusTier: String?
    /// Tiers the viewer just requested that enqueued (`Queued`, encrypted mode) —
    /// rendered "Pending approval" until the author mints.
    public private(set) var pendingTiers: Set<String> = []
    /// Shared page-level error (`error-message`).
    public var errorMessage: String?
    /// Whether the viewer's follow is queued for the author's approval (the
    /// `status_get` reply's `follow_pending`).
    public private(set) var followPending = false
    /// The optimistic post-press state, set by the reply to a follow or unfollow
    /// press: it outranks the status read for the label, so a queued leave reads
    /// *Follow* at once (`profile.md` § Where logic lives → *Follow / unfollow /
    /// follow-status* — the flip stays app-side). Cleared by a fresh `configure`.
    private var followFlip: Bool?
    /// The first press on a paid holder's *Following* only arms the confirm: the
    /// button reads `profile.unfollow_confirm` and the second press leaves
    /// (`subscriptions.md` § The free followers tier — follow and unfollow, rule 4).
    /// Any other press, a fresh open or the reply disarms it. Mirrors tui
    /// `ProfileState::unfollow_armed`.
    public private(set) var unfollowArmed = false

    #if !FAUNA_EXCISE_PAYMENTS
    /// `subscription-claim-redeem-input` on the OTHER Tiers tab — a claim code
    /// of THIS creator, redeemed at the page's actor home. Cleared only when the
    /// nest takes the code (a refused paste stays to correct) and on every fresh
    /// open, so a code typed for one creator never rides to the next. Mirrors tui
    /// `ProfileState::redeem_input`.
    public var claimCodeInput = ""
    #endif

    /// Whether the viewer follows this creator — drives the header
    /// `profile-follow-button` toggle. The optimistic flip if a press has
    /// answered, else the shared `is_following` over the status read (following ⇔ a
    /// tier is named or a follow is pending).
    public var isFollowing: Bool {
        followFlip ?? FaunaFFISwift.isFollowing(tier: statusTier, followPending: followPending)
    }

    /// The `profile-follow-button` label: the confirm while armed, else
    /// Follow ⇄ Following through the shared `followToggleLabel`.
    public var followButtonLabel: String {
        unfollowArmed ? L.profile.unfollowConfirm
            : renderLocalizedText(followToggleLabel(isFollowing: isFollowing))
    }

    private var api: APIClient?
    private var authorIdHex: String?
    /// The page's actor-home slot the offers are read from and a per-row Subscribe
    /// goes to (`nil` = this nest) — `ProfileKnockVM.home(peer:)`, handed in at
    /// ``configure(api:authorIdHex:home:)`` and replaced through ``noteHome(_:)``
    /// when the header read moves it. Mirrors tui `ProfileState`'s offers home.
    public private(set) var home: String?

    /// The free default tier; following it is the header `profile-follow-button`,
    /// so it never appears as a per-row paid offer (mirrors linux `FOLLOWERS_TIER`).
    private static let followersTier = "followers"

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary), on `SearchVM.reset()`'s shape. Called by
    /// ``configure(api:authorIdHex:)`` on an api-identity change **before** it
    /// re-loads, and by the page's nil-client phase: More → Profile is not unmounted
    /// by the iOS switch teardown .
    ///
    /// `statusTier`, `pendingTiers` and the follow state are the VIEWER's own
    /// relationship to the creator rather than the creator's public offers — so a
    /// survivor tells the incoming account it holds a paid tier, or follows someone,
    /// on the strength of the OUTGOING account's subscription.
    public func reset() {
        api = nil
        authorIdHex = nil
        home = nil
        offers = []
        statusTier = nil
        pendingTiers = []
        followPending = false
        followFlip = nil
        unfollowArmed = false
        errorMessage = nil
        #if !FAUNA_EXCISE_PAYMENTS
        claimCodeInput = ""
        #endif
    }

    /// Wire the API + the viewed author + the page's actor-home slot as the open
    /// seeded it, then load the offers + status.
    public func configure(api: APIClient, authorIdHex: String, home: String? = nil) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        self.authorIdHex = authorIdHex
        self.home = home
        // A fresh open reads the follow state from the status, disarmed.
        followFlip = nil
        unfollowArmed = false
        #if !FAUNA_EXCISE_PAYMENTS
        claimCodeInput = ""
        #endif
        await refresh()
    }

    /// The header read replaced the page's actor-home slot: a moved slot re-reads
    /// the offers from the new home (tui's `offers_reload_op`; linux `home_moved`).
    /// An unchanged slot reads nothing.
    public func noteHome(_ newHome: String?) async {
        guard api != nil, newHome != home else { return }
        home = newHome
        await refresh()
    }

    /// Re-read the offered tiers (from the actor-home slot) + the viewer's status.
    public func refresh() async {
        guard let api, let authorIdHex else { return }
        let readHome = home
        do {
            let loaded = try await api.subscriptionOffersList(authorIdHex: authorIdHex, home: readHome)
            // The in-flight clause: the status read below is the VIEWER's own tier,
            // so a late landing would attribute the outgoing account's subscription
            // to the incoming one. An answer read under a home the slot has since
            // left is dropped too — the re-read from the new home paints instead.
            guard self.api === api, self.authorIdHex == authorIdHex, self.home == readHome else { return }
            offers = loaded.filter { $0.name != Self.followersTier }
            let status = try await api.subscriptionStatusGet(authorIdHex: authorIdHex)
            guard self.api === api, self.authorIdHex == authorIdHex else { return }
            statusTier = status.tier
            followPending = status.followPending
            errorMessage = nil
        } catch {
            guard self.api === api, self.authorIdHex == authorIdHex, self.home == readHome else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    /// The viewer's status for `tier` (active if held, pending if just enqueued).
    public func status(for tier: String) -> OfferStatus {
        offerStatus(tierName: tier, statusTier: statusTier, pending: pendingTiers.contains(tier))
    }

#if !FAUNA_KIDS
    /// Subscribe to `tier`: plaintext + auto-approve grants inline → status flips
    /// to active; encrypted mode enqueues → pending. Re-reads to confirm.
    ///
    /// `home` defaults to the page's actor-home slot (``home``), so a per-row
    /// Subscribe goes across nests to the home the offers were read from; the
    /// header follow passes the slot it read itself.
    public func subscribe(_ tier: String) async {
        await subscribe(tier, home: home)
    }

    /// ``subscribe(_:)`` to an explicit `home` (`nil` = the local arm).
    public func subscribe(_ tier: String, home: String?) async {
        guard let api, let authorIdHex else { return }
        unfollowArmed = false
        do {
            let reply = try await api.subscriptionSubscribe(
                authorIdHex: authorIdHex, tier: tier, homeNestUrl: home)
            apply(reply, tier: tier)
            await refresh()
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// The OTHER-profile offer row's payment-link press — the Subscribe with the
    /// checkout riding it (`monetization.md` § Pillar 3 → *Buyer↔actor binding
    /// (Q4)*, the Q13 rules (1) and (2)): a URL the shared `isSafePaymentUrl`
    /// refuses is the page error and nothing is sent; otherwise the subscribe to
    /// `tier` goes FIRST, across nests at `home` (the page's actor-home slot, the
    /// one the follow button carries), and only then is the checkout URL the
    /// shared rule composed — bound to the buyer's actor — handed to `open`. The
    /// reply lands on the row as the row's Subscribe does; a refused subscribe is
    /// the page error and opens nothing. Mirrors tui `Action::OpenPaymentLink` →
    /// `Op::Subscribe { checkout: Some(url), home_nest_url }`.
    ///
    /// `open` is the view's hand-off to the app's one external-URL opener
    /// (`ExternalURLOpener.open(_:with:)`, the e2e record seam); tests inject a
    /// recorder.
    public func payTo(
        _ tier: String, paymentUrl: String, home: String? = nil,
        open: @MainActor (URL) -> Void
    ) async {
        guard let api, let authorIdHex else { return }
        unfollowArmed = false
        guard isSafePaymentUrl(url: paymentUrl) else {
            errorMessage = L.subscriptions.unsafePaymentUrl
            return
        }
        do {
            let checkout = try await api.subscriptionSubscribeAndCheckout(
                authorIdHex: authorIdHex, tier: tier, paymentUrl: paymentUrl, homeNestUrl: home)
            // The in-flight clause: the checkout is bound to the account that
            // pressed; an account switch mid-subscribe opens nothing.
            guard self.api === api else { return }
            guard let checkout else { return }
            apply(checkout.reply, tier: tier)
            if let url = URL(string: checkout.url) { open(url) }
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    /// Land a subscribe reply on `tier`'s row: approved → active; queued → the
    /// pending transient until the author mints.
    private func apply(_ reply: FfiSubscribeReply, tier: String) {
        switch reply {
        case .approved(let granted, _):
            statusTier = granted
            pendingTiers.remove(tier)
        case .queued:
            pendingTiers.insert(tier)
        }
        errorMessage = nil
    }
#endif

#if !FAUNA_KIDS && !FAUNA_EXCISE_PAYMENTS
    /// `subscription-claim-redeem-button` on the OTHER Tiers tab — redeem the
    /// typed code of THIS creator through the shared across-nests redeem
    /// (`monetization.md` § Pillar 3 → *Which nest an in-app redeem binds on*),
    /// carrying `home`: the page's actor-home slot, exactly the value the follow
    /// passes (`nil` = the local redeem). A blank code is a no-op, never a nest
    /// round-trip. On success the spent code leaves the input, a queued grant
    /// reads "Pending approval" as a queued subscribe does, and the offers +
    /// status re-read; a refusal keeps the code and surfaces on `error-message`.
    /// Mirrors tui `profile::Action::RedeemClaim`.
    public func redeemClaim(home: String? = nil) async {
        guard let api, let authorIdHex else { return }
        let code = claimCodeInput.trimmingCharacters(in: .whitespaces)
        guard !code.isEmpty else { return }
        unfollowArmed = false
        do {
            let reply = try await api.redeemPaymentClaimAcrossNests(
                code: code, authorIdHex: authorIdHex, authorHomeUrl: home)
            guard self.api === api else { return }
            if reply.queued { pendingTiers.insert(reply.tier) }
            claimCodeInput = ""
            errorMessage = nil
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
    }
#endif

#if !FAUNA_KIDS
    /// The header `profile-follow-button` press — the Follow ⇄ Following toggle
    /// (`subscriptions.md` § The free followers tier — follow and unfollow, rule 4;
    /// tui `profile::Action::Follow`). *Follow* subscribes to the free "followers"
    /// tier; *Following* unfollows it — confirm-gated when the viewer holds a paid
    /// tier at the author (`unfollow_ends_paid_tier`): the first press only arms
    /// the confirm and sends nothing, the second leaves.
    ///
    /// `home` is the profile page's actor-home slot (`actor_home`): both directions
    /// carry it so an author homed on another nest is followed and left across nests
    /// (`federated-follow.md` § Decision 2); `nil` = the local arm.
    public func toggleFollow(home: String? = nil) async {
        guard isFollowing else {
            await follow(home: home)
            return
        }
        if !unfollowArmed && FaunaFFISwift.unfollowEndsPaidTier(tier: statusTier) {
            unfollowArmed = true
            return
        }
        await unfollow(home: home)
    }

    /// Follow = subscribe to the free "followers" tier.
    private func follow(home: String?) async {
        await subscribe(Self.followersTier, home: home)
        // Optimistic on-success flip (the button reads "Following"); `subscribe`
        // clears `errorMessage` on success and sets it on failure.
        if errorMessage == nil { followFlip = true }
    }

    /// Unfollow = leave the free "followers" tier — the leave's floor, so a paid
    /// tier held above it ends too (rule 1). Encrypted mode answers `Queued` until
    /// the author's client commits the removal; the flip reads *Follow* at once.
    private func unfollow(home: String?) async {
        guard let api, let authorIdHex else { return }
        unfollowArmed = false
        do {
            _ = try await api.subscriptionUnsubscribe(
                authorId: hex_to_data(authorIdHex), tier: Self.followersTier, homeNestUrl: home)
            guard self.api === api else { return }
            followFlip = false
            errorMessage = nil
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
    }
#endif
}
