import SwiftUI

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
    /// Whether the viewer follows this creator — drives the header
    /// `profile-follow-button` label ("Follow" ⇄ "Following", via the shared
    /// `followToggleLabel`). Optimistic: flips true on a successful `follow()`,
    /// matching the other natives. The `status_get`-derived *initial* state is
    /// per-app latitude (ratified 2026-07-08) and intentionally not derived here.
    public private(set) var isFollowing = false

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
    /// `statusTier`, `pendingTiers` and `isFollowing` are the VIEWER's own
    /// relationship to the creator rather than the creator's public offers — so a
    /// survivor tells the incoming account it holds a paid tier, or follows someone,
    /// on the strength of the OUTGOING account's subscription.
    public func reset() {
        api = nil
        authorIdHex = nil
        offers = []
        statusTier = nil
        pendingTiers = []
        isFollowing = false
        errorMessage = nil
    }

    /// Wire the API + the viewed author, then load the offers + status.
    public func configure(api: APIClient, authorIdHex: String) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        self.authorIdHex = authorIdHex
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

    /// Subscribe to `tier`: plaintext + auto-approve grants inline → status flips
    /// to active; encrypted mode enqueues → pending. Re-reads to confirm.
    public func subscribe(_ tier: String) async {
        guard let api, let authorIdHex else { return }
        do {
            let reply = try await api.subscriptionSubscribe(authorIdHex: authorIdHex, tier: tier)
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

    /// Follow = subscribe to the free "followers" tier (the header button).
    /// ⚠ Nest-gated until the nest auto-provisions the "followers" tier
    /// (`monetization.md` § Pillar 1 — currently returns `tier_not_found`); wired
    /// for parity with the other apps.
    public func follow() async {
        await subscribe(Self.followersTier)
        // Optimistic on-success flip (the button reads "Following"); `subscribe`
        // clears `errorMessage` on success and sets it on failure.
        if errorMessage == nil { isFollowing = true }
    }
}
