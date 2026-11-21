import SwiftUI

/// Shared view-model for the admin **`admin-settings`** page (nav label "Tiers",
/// admin.md § 3 Settings), shared by macOS + iOS (one FaunaKit VM). The page
/// covers tier *definitions* (each `AdminTier`'s caps, **in-place editing**) and
/// membership *designations* (monetization.md § Pillar 4 — linking one of the
/// admin's own subscription tiers to an admitted/lapsed quota tier; a link
/// editor, never a third tier list). Admission (assigning a tier to a user)
/// lives on the `admin-users` hub (§ 2), not here.
///
/// Drives the shared `fauna.admin.tiers.{list,create,update}` +
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

    /// Where the page's own `tiers.list` read stands (admin.md § Errors &
    /// edge cases, *Controls wait for their read*): the tier add form waits
    /// for it — `Possibility::once_read`'s Swift twin.
    public private(set) var tiersRead = PageRead()
    /// Whether the read this page's read-gated controls wait for has resolved
    /// (``AdminPageReading``): the tier list read. An unanswered one is asked again when the
    /// nest connection comes back.
    public var readAnswered: Bool { tiersRead.state == .resolved }

    /// The tier add form's answer: the `tiers.list` read resolved and no
    /// request is on its way. The submit asks the same answer.
    public var addTierPossibility: Possibility {
        writeAfterReadPossibility(read: tiersRead.state, idle: !isBusy)
    }

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
            catch {
                errorMessage = DisplayError.message(error)
                tiersRead.failed()
                return
            }
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
            do {
                tiers = try await admin.tiersList()
                tiersRead.resolved()
            } catch {
                tiersRead.failed()
                throw error
            }
            ownMembershipTierNames = try await api.listSubscriptionTiers().map { $0.name }
            membershipTiers = try await admin.membershipTiersList()
            errorMessage = nil
            snapshotVersion &+= 1
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// A tier row's save answer over its five cap drafts (inbox, storage,
    /// devices, blob size, feeds): the shared `tier_caps_possibility`. The row's
    /// Save is painted from it and `saveTier` asks it again (admin.md § Errors &
    /// edge cases, *A form's submit is live only for a draft the nest would
    /// accept*).
    public static func tierCapsAnswer(_ caps: [String]) -> Possibility {
        adminTierCapsPossibility(caps: caps)
    }

    /// The add-a-tier form's answer: the page read first, then the shared
    /// `tier_create_possibility` over the name and five cap drafts.
    public func addTierAnswer(name: String, caps: [String]) -> Possibility {
        addTierPossibility.and(adminTierCreatePossibility(name: name, caps: caps))
    }

    /// A membership row's save answer: the shared `membership_save_possibility`
    /// (an admitted-at tier picked).
    public static func membershipSaveAnswer(adminTier: String) -> Possibility {
        adminMembershipSavePossibility(adminTier: adminTier)
    }

    /// Save one tier's caps (full update — the tier `name` identifies the row and
    /// is not editable), then refetch so the row re-renders from persisted state.
    /// `caps` are the row's five drafts; a malformed one refuses the press with
    /// the shared reason on `error-message` — never sent with the stored cap in
    /// its place.
    public func saveTier(name: String, caps: [String]) async {
        if refusePress(Self.tierCapsAnswer(caps), errorMessage: &errorMessage) { return }
        guard let admin else { return }
        let parsed = caps.compactMap { parseCap(input: $0) }
        // Unreachable once the answer above was possible (it asks `parse_cap`).
        guard parsed.count == 5 else { return }
        await runAdminCommand {
            try await admin.tiersUpdate(
                name: name,
                maxInboxBytes: parsed[0],
                maxStorageBytes: parsed[1],
                maxDevices: parsed[2],
                maxBlobSize: parsed[3],
                maxFeeds: parsed[4]
            )
        }
    }

    /// Define a new tier via `fauna.admin.tiers.create` (admin.md § 3 → *Defining a
    /// new tier*), then refetch so it appears as an ordinary row. The press asks
    /// `addTierAnswer` — the form is painted from the same answer — so an empty
    /// name or an unparseable cap is refused locally on `error-message` with the
    /// shared reason; the name is trimmed and each cap goes through the shared
    /// `parseCap`. A nest refusal (a taken name is `fauna.admin.conflict`)
    /// is worded through `addTierError`. Returns `true` only when the nest
    /// accepted the create — the form clears on that alone, so a refused name
    /// keeps what was typed.
    public func addTier(name: String, inbox: String, storage: String, devices: String,
                        blobSize: String, feeds: String) async -> Bool {
        if refusePress(addTierAnswer(name: name, caps: [inbox, storage, devices, blobSize, feeds]),
                       errorMessage: &errorMessage) { return false }
        guard let admin else { return false }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            errorMessage = L.admin.settingsPage.addTierErrorEmptyName
            return false
        }
        guard let maxInboxBytes = parseCap(input: inbox),
              let maxStorageBytes = parseCap(input: storage),
              let maxDevices = parseCap(input: devices),
              let maxBlobSize = parseCap(input: blobSize),
              let maxFeeds = parseCap(input: feeds) else {
            errorMessage = L.admin.settingsPage.addTierErrorInvalidCap
            return false
        }
        isBusy = true
        var created = false
        do {
            try await admin.tiersCreate(
                name: trimmed, maxInboxBytes: maxInboxBytes, maxStorageBytes: maxStorageBytes,
                maxDevices: maxDevices, maxBlobSize: maxBlobSize, maxFeeds: maxFeeds)
            errorMessage = nil
            created = true
        } catch {
            errorMessage = L.admin.settingsPage.addTierError(message: DisplayError.message(error) ?? "")
        }
        isBusy = false
        let failure = errorMessage
        await hydrate()
        // `hydrate` clears a stale error on success; a refused create keeps its own.
        if !created { errorMessage = failure }
        return created
    }

    /// Remove one tier via `fauna.admin.tiers.delete` (admin.md § 3 → *Removing a
    /// tier*), then refetch so the row disappears (non-optimistic). No confirm
    /// dialog. A refusal — including the nest's in-use `fauna.admin.conflict`,
    /// which the UniFFI face already words through the shared `tier_delete_refusal`
    /// — goes on `error-message` as `deleteTierError(message:)`; the refusal is
    /// kept across the refetch, as in `addTier`.
    public func deleteTier(name: String) async {
        if refusePress(adminTierDeletePossibility(tier: name), errorMessage: &errorMessage) { return }
        guard let admin else { return }
        isBusy = true
        var deleted = false
        do {
            try await admin.tiersDelete(name: name)
            errorMessage = nil
            deleted = true
        } catch {
            errorMessage = L.admin.settingsPage.deleteTierError(message: DisplayError.message(error) ?? "")
        }
        isBusy = false
        let failure = errorMessage
        await hydrate()
        if !deleted { errorMessage = failure }
    }

    /// Designate/re-point a membership tier via `fauna.admin.membership_tiers.set`
    /// (an upsert), then refetch so the row re-renders from persisted state.
    /// `lapseTier` always rides an explicit selection — the row never relies on
    /// the wire's omit-means-default.
    public func saveMembershipTier(tierName: String, adminTier: String, lapseTier: String) async {
        if refusePress(Self.membershipSaveAnswer(adminTier: adminTier), errorMessage: &errorMessage) {
            return
        }
        guard let admin else { return }
        await runAdminCommand {
            try await admin.membershipTiersSet(tierName: tierName, adminTier: adminTier, lapseTier: lapseTier)
        }
    }

    /// Drop a membership designation via `fauna.admin.membership_tiers.clear`;
    /// the subscription tier itself survives, reverting to undesignated.
    /// `admin-settings-membership-clear-button`'s answer for the row of
    /// `tierName`: the shared `membership_clear_possibility` (admin.md § 2,
    /// *What a row offers*) — blocked, saying nothing is set, while the row
    /// holds no persisted designation.
    public func membershipClearAnswer(tierName: String) -> Possibility {
        adminMembershipClearPossibility(
            hasExisting: membershipTiers.contains { $0.tierName == tierName })
    }

    public func clearMembershipTier(tierName: String) async {
        if refusePress(membershipClearAnswer(tierName: tierName), errorMessage: &errorMessage) { return }
        guard let admin else { return }
        await runAdminCommand { try await admin.membershipTiersClear(tierName: tierName) }
    }
}
