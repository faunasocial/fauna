import SwiftUI
import FaunaFFISwift

/// View-model for the profile **OTHER-profile offers section** (subscriber browse
/// — `monetization.md` § Pillar 1, surface 2), shared by macOS + iOS. Reads
/// another creator's offered tiers (`offers_list`) + the viewer's status
/// (`status_get`), and subscribes per row. The free "followers" tier is excluded
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
        offers = []
        statusTier = nil
        pendingTiers = []
        followPending = false
        followFlip = nil
        unfollowArmed = false
        errorMessage = nil
    }

    /// Wire the API + the viewed author, then load the offers + status.
    public func configure(api: APIClient, authorIdHex: String) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        self.authorIdHex = authorIdHex
        // A fresh open reads the follow state from the status, disarmed.
        followFlip = nil
        unfollowArmed = false
        await refresh()
    }

    /// Re-read the offered tiers + the viewer's status.
    public func refresh() async {
        guard let api, let authorIdHex else { return }
        do {
            let loaded = try await api.subscriptionOffersList(authorIdHex: authorIdHex)
            // The in-flight clause: the status read below is the VIEWER's own tier,
            // so a late landing would attribute the outgoing account's subscription
            // to the incoming one.
            guard self.api === api else { return }
            offers = loaded.filter { $0.name != Self.followersTier }
            let status = try await api.subscriptionStatusGet(authorIdHex: authorIdHex)
            guard self.api === api else { return }
            statusTier = status.tier
            followPending = status.followPending
            errorMessage = nil
        } catch {
            guard self.api === api else { return }
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
    public func subscribe(_ tier: String, home: String? = nil) async {
        guard let api, let authorIdHex else { return }
        unfollowArmed = false
        do {
            let reply = try await api.subscriptionSubscribe(
                authorIdHex: authorIdHex, tier: tier, homeNestUrl: home)
            switch reply {
            case .approved(let granted, _):
                statusTier = granted
                pendingTiers.remove(tier)
            case .queued:
                pendingTiers.insert(tier)
            }
            errorMessage = nil
            await refresh()
        } catch {
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
