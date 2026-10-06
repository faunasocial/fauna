import SwiftUI

/// Shared view-model for the admin **`admin-settings`** page (nav label "Tiers",
/// admin.md § 3 Settings), shared by macOS + iOS (one FaunaKit VM). The page
/// covers tier *definitions* (each `AdminTier`'s caps, **in-place editing**) and
/// membership *designations* (monetization.md § Pillar 4 — linking one of the
/// admin's own subscription tiers to an admitted/lapsed quota tier; a link
/// editor, never a third tier list). Admission (assigning a tier to a user)
/// lives on the `admin-users` hub (§ 2), not here.
///
/// Drives the shared `fauna.admin.tiers.{list,update}` +
/// `fauna.admin.membership_tiers.{list,set,clear}` kinds via the UniFFI
/// `FfiAdminClient` (`FfiNestClient.admin()`), plus `fauna.subscriptions.tiers.list`
/// via `APIClient.listSubscriptionTiers()` for the membership row set — no
/// `/admin/api/*` twins. A save PUTs the row via the matching `*Update`/`*Set`
/// call then refetches everything, so the row re-renders from persisted state
/// (proves the round-trip, not an optimistic flip). Reference renderers: linux
/// (`views/admin.rs::build_tier_definition_row` / `build_membership_row`),
/// android (`AdminSettingsVM.kt`), windows (`AdminSettingsViewModel`), web
/// (`admin/settings/+page.svelte`).
@MainActor @Observable
public final class AdminTiersVM: BusyAdminCommandVM {
    /// Tier definitions (one editable `admin-settings-tier-item` row each).
    public private(set) var tiers: [FfiAdminTier] = []
    /// The admin's own subscription-tier names (`fauna.subscriptions.tiers.list`)
    /// — the row set for the `admin-settings-membership-item` link editor
    /// (monetization.md § Pillar 4). Empty is the normal out-of-the-box state (no
    /// subscription tiers minted yet), not an error.
    public private(set) var ownMembershipTierNames: [String] = []
    /// Which of the rows above already carry a designation, and what it links to.
    public private(set) var membershipTiers: [FfiAdminMembershipTier] = []
    /// Bumped on every (re)load so each row re-seeds its edit buffers from the
    /// freshly-fetched caps — the `reloadToken` the row `.task(id:)` keys on,
    /// mirroring `AdminMailView`'s per-section seed-on-snapshot pattern.
    public private(set) var snapshotVersion: Int = 0
    /// Page-level error surface (`error-message`).
    public var errorMessage: String?
    public internal(set) var isBusy = false

    /// Quota-tier names the membership row's "Admits at"/"Lapses to" pickers
    /// cycle over — same source `AdminVM.tierNames` computes.
    public var tierNames: [String] { tiers.map { $0.name } }

    private var admin: FfiAdminClient?
    private var api: APIClient?

    public init() {}

    /// Vend the admin client from APIClient and load the tier definitions.
    /// Idempotent (client built once); always re-hydrates so a re-navigation
    /// refetches (the macOS shell re-runs this on every `navGeneration` bump).
    public func configure(api: APIClient) async {
        self.api = api
        if admin == nil {
            do { admin = try await api.adminClient() }
            catch { errorMessage = DisplayError.message(error); return }
        }
        await hydrate()
    }

    /// Re-read `fauna.admin.tiers.list` + the membership-designation sources
    /// (own subscription-tier names, current designations) and bump
    /// `snapshotVersion` so the rows re-seed from persisted state.
    public func hydrate() async {
        guard let admin, let api else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            tiers = try await admin.tiersList()
            ownMembershipTierNames = try await api.listSubscriptionTiers().map { $0.name }
            membershipTiers = try await admin.membershipTiersList()
            errorMessage = nil
            snapshotVersion &+= 1
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Save one tier's caps (full update — the tier `name` identifies the row and
    /// is not editable), then refetch so the row re-renders from persisted state.
    public func saveTier(_ tier: FfiAdminTier) async {
        guard let admin else { return }
        await runAdminCommand {
            try await admin.tiersUpdate(
                name: tier.name,
                maxInboxBytes: tier.maxInboxBytes,
                maxStorageBytes: tier.maxStorageBytes,
                maxDevices: tier.maxDevices,
                maxBlobSize: tier.maxBlobSize,
                maxFeeds: tier.maxFeeds
            )
        }
    }

    /// Designate/re-point a membership tier via `fauna.admin.membership_tiers.set`
    /// (an upsert), then refetch so the row re-renders from persisted state.
    /// `lapseTier` always rides an explicit selection — the row never relies on
    /// the wire's omit-means-default.
    public func saveMembershipTier(tierName: String, adminTier: String, lapseTier: String) async {
        guard let admin else { return }
        await runAdminCommand {
            try await admin.membershipTiersSet(tierName: tierName, adminTier: adminTier, lapseTier: lapseTier)
        }
    }

    /// Drop a membership designation via `fauna.admin.membership_tiers.clear`;
    /// the subscription tier itself survives, reverting to undesignated.
    public func clearMembershipTier(tierName: String) async {
        guard let admin else { return }
        await runAdminCommand { try await admin.membershipTiersClear(tierName: tierName) }
    }
}
