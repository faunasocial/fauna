import SwiftUI

/// The admin **`admin-settings`** page (nav label "Tiers", admin.md § 3 Settings),
/// shared by macOS + iOS (one FaunaKit view, thin per-target mount points).
/// A dumb renderer of `AdminTiersVM`; no business logic
/// here. Element IDs match `tests/e2e-unified/ui.yaml` `admin-settings` exactly.
/// Reference renderers: linux (`apps/fauna-linux/src/views/admin.rs::build_settings_page`
/// / `build_tier_definition_row`), windows (`AdminSettingsPage`), web
/// (`routes/admin/settings/+page.svelte`).
///
/// The page has two sections. **Tier definitions** — each `admin-settings-tier-item`
/// row shows the tier name + an editable **raw-i64** input per cap
/// (`admin-settings-tier-cap-*`) + an `admin-settings-tier-save-button` that PUTs the
/// row via `fauna.admin.tiers.update` and refetches. The inputs hold exact integers
/// (bytes for the byte caps, counts otherwise), 1:1 with the `AdminTier` i64 wire
/// type; a stray/blank edit falls back to the persisted value on Save (no silent
/// zeroing — mirrors linux `parse_cap`). **Membership designations**
/// (monetization.md § Pillar 4) — one `admin-settings-membership-item` row per
/// subscription tier the admin owns, linking it to an admitted/lapsed quota tier;
/// see `MembershipDesignationRow` below.
///
/// `admin-nav-back` is provided by the admin shell rail (macOS) / the navigation
/// stack (iOS), not this page.
public struct AdminTiersView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AdminTiersVM()
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0
    /// (the NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) { self.reloadToken = reloadToken }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.adminSettingsHeading, L.admin.settingsPage.title)
                    .font(.title)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                VStack(alignment: .leading, spacing: 12) {
                    Text(L.admin.settingsPage.tiers)
                        .font(.headline)
                    if vm.tiers.isEmpty {
                        Text(L.admin.settingsPage.noTiers)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(Array(vm.tiers.enumerated()), id: \.element.name) { offset, tier in
                        TierDefinitionRow(tier: tier, reloadToken: vm.snapshotVersion,
                                          isBusy: vm.isBusy) { await vm.saveTier($0) }
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier(Ids.adminSettingsTierItem)
                            // Per-row read anchor (indexed registry entry per
                            // ForEach row) exposing the tier name.
                            .automationValue(Ids.adminSettingsTierItem, text: { tier.name })
                            // Scoped container (goal-doc rule 5): the cap fields +
                            // save button are queried via
                            // `scope="admin-settings-tier-item[N]/..."`.
                            .automationScope(Ids.adminSettingsTierItem, index: offset)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.adminSettingsTiersSection)
                .automationValue(Ids.adminSettingsTiersSection, text: { String(vm.tiers.count) })

                VStack(alignment: .leading, spacing: 12) {
                    Text(L.admin.settingsPage.membershipSection)
                        .font(.headline)
                    if vm.ownMembershipTierNames.isEmpty {
                        // The normal out-of-the-box state (no subscription tiers
                        // minted yet) — an empty-state pointer at the admin's own
                        // Tiers tab, never an error (monetization.md § Pillar 4).
                        Text(L.admin.settingsPage.noMembershipTiers)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(Array(vm.ownMembershipTierNames.enumerated()), id: \.element) { offset, tierName in
                        MembershipDesignationRow(
                            tierName: tierName,
                            existing: vm.membershipTiers.first { $0.tierName == tierName },
                            ownTierNames: vm.ownMembershipTierNames,
                            quotaTierNames: vm.tierNames,
                            reloadToken: vm.snapshotVersion,
                            isBusy: vm.isBusy,
                            onSave: { await vm.saveMembershipTier(tierName: $0, adminTier: $1, lapseTier: $2) },
                            onClear: { await vm.clearMembershipTier(tierName: $0) }
                        )
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.adminSettingsMembershipItem)
                        // Per-row read anchor exposing the designated subscription tier name.
                        .automationValue(Ids.adminSettingsMembershipItem, text: { tierName })
                        .automationScope(Ids.adminSettingsMembershipItem, index: offset)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.adminSettingsMembershipSection)
                .automationValue(Ids.adminSettingsMembershipSection, text: { String(vm.ownMembershipTierNames.count) })
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
    }
}

// MARK: - Tier definition row (one editable `admin-settings-tier-item`)

/// One tier row: the name + five raw-i64 cap inputs + a Save button. Integer
/// fields edit as `String` buffers seeded from the persisted caps (re-seeded on
/// every `reloadToken` bump = each refetch), parsed on Save with a fall-back to
/// the persisted value (`parseCap`, mirroring linux `parse_cap` — a stray edit
/// never silently zeroes a cap). Save gathers the buffers into an `FfiAdminTier`
/// and hands it up; the VM PUTs + refetches.
private struct TierDefinitionRow: View {
    let tier: FfiAdminTier
    let reloadToken: Int
    let isBusy: Bool
    let onSave: (FfiAdminTier) async -> Void

    @State private var inbox = ""
    @State private var storage = ""
    @State private var devices = ""
    @State private var blobSize = ""
    @State private var feeds = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(tier.name)
                .font(.headline)

            EditableFieldRow(L.admin.settingsPage.capInboxBytes, "admin-settings-tier-cap-inbox", $inbox,
                             labelColor: .secondary, maxWidth: 200)
            EditableFieldRow(L.admin.settingsPage.capStorageBytes, "admin-settings-tier-cap-storage", $storage,
                             labelColor: .secondary, maxWidth: 200)
            EditableFieldRow(L.admin.settingsPage.capDevices, "admin-settings-tier-cap-devices", $devices,
                             labelColor: .secondary, maxWidth: 200)
            EditableFieldRow(L.admin.settingsPage.capBlobSize, "admin-settings-tier-cap-blob-size", $blobSize,
                             labelColor: .secondary, maxWidth: 200)
            EditableFieldRow(L.admin.settingsPage.capFeeds, "admin-settings-tier-cap-feeds", $feeds,
                             labelColor: .secondary, maxWidth: 200)

            Button(L.admin.settingsPage.saveTier) {
                save()
            }
            .disabled(isBusy)
            .accessibilityIdentifier(Ids.adminSettingsTierSaveButton)
            .automationActivate(Ids.adminSettingsTierSaveButton,
                                isEnabled: { !isBusy }) {
                save()
            }
            .faunaGate("fauna.admin.tiers.update")
        }
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .task(id: reloadToken) { seed() }
    }

    /// The Save action — shared by the `Button` and `automationActivate` so the
    /// two can never diverge.
    private func save() {
        Task { await onSave(gather()) }
    }

    private func seed() {
        inbox = String(tier.maxInboxBytes)
        storage = String(tier.maxStorageBytes)
        devices = String(tier.maxDevices)
        blobSize = String(tier.maxBlobSize)
        feeds = String(tier.maxFeeds)
    }

    private func gather() -> FfiAdminTier {
        // Shared `fauna_core::format::parse_cap` (UniFFI `parseCap`): a stray edit
        // (empty / unparseable / negative / fractional / out-of-range) falls back
        // to the persisted cap, so it never silently zeroes — one validator for
        // every app (value-formatting.md § Tier cap validation), replacing the
        // local `max(0, Int64(...))` (which on a negative edit zeroed the cap,
        // contradicting that intent; `parseCap` conforms apple to the shared
        // fall-back-to-prev contract linux/windows already use, #4).
        FfiAdminTier(
            name: tier.name,
            maxInboxBytes: parseCap(input: inbox) ?? tier.maxInboxBytes,
            maxStorageBytes: parseCap(input: storage) ?? tier.maxStorageBytes,
            maxDevices: parseCap(input: devices) ?? tier.maxDevices,
            maxBlobSize: parseCap(input: blobSize) ?? tier.maxBlobSize,
            maxFeeds: parseCap(input: feeds) ?? tier.maxFeeds
        )
    }
}

// MARK: - Membership designation row (one editable `admin-settings-membership-item`)

/// One `admin-settings-membership-item` row: a link between one of the admin's own
/// subscription tiers (`tierName`, the row's identity) and an admitted/lapsed quota
/// tier (monetization.md § Pillar 4 — a link editor, never a third tier list).
/// `tier-select` is display + fix-up (a real cycle button per the approved shape,
/// letting an admin correct a mis-set row without deleting it) — Save reads the
/// *current* selection, never the row's original identity (mirrors linux's
/// `dropdown_tier(&tier_select)` at save time and android's `selectedTierName`).
/// Clear always targets this row's own identity, `tierName`. Reference renderers:
/// linux (`views/admin.rs::build_membership_row`), android (`MembershipRow`).
private struct MembershipDesignationRow: View {
    let tierName: String
    let existing: FfiAdminMembershipTier?
    let ownTierNames: [String]
    let quotaTierNames: [String]
    let reloadToken: Int
    let isBusy: Bool
    let onSave: (_ tierName: String, _ adminTier: String, _ lapseTier: String) async -> Void
    let onClear: (_ tierName: String) async -> Void

    @State private var selectedTierName = ""
    @State private var adminTier = ""
    @State private var lapseTier = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            tierCycleButton(id: "admin-settings-membership-tier-select", names: ownTierNames,
                            current: { selectedTierName }) { selectedTierName = $0 }

            HStack(alignment: .firstTextBaseline) {
                Text(L.admin.settingsPage.membershipAdmitsAt)
                    .foregroundStyle(.secondary)
                tierCycleButton(id: "admin-settings-membership-admin-tier-select", names: quotaTierNames,
                                current: { adminTier }) { adminTier = $0 }
            }
            HStack(alignment: .firstTextBaseline) {
                Text(L.admin.settingsPage.membershipLapsesTo)
                    .foregroundStyle(.secondary)
                tierCycleButton(id: "admin-settings-membership-lapse-tier-select", names: quotaTierNames,
                                current: { lapseTier }) { lapseTier = $0 }
            }

            HStack(spacing: 8) {
                Button(L.admin.settingsPage.membershipSave) {
                    save()
                }
                .disabled(isBusy)
                .accessibilityIdentifier(Ids.adminSettingsMembershipSaveButton)
                .automationActivate(Ids.adminSettingsMembershipSaveButton,
                                    isEnabled: { !isBusy }) {
                    save()
                }
                .faunaGate("fauna.admin.membership_tiers.set")

                Button(L.admin.settingsPage.membershipClear, role: .destructive) {
                    clear()
                }
                .disabled(isBusy || existing == nil)
                .accessibilityIdentifier(Ids.adminSettingsMembershipClearButton)
                .automationActivate(Ids.adminSettingsMembershipClearButton,
                                    isEnabled: { !isBusy && existing != nil }) {
                    clear()
                }
                .faunaGate("fauna.admin.membership_tiers.clear")
            }
        }
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .task(id: reloadToken) { seed() }
    }

    private func seed() {
        selectedTierName = tierName
        adminTier = existing?.adminTier ?? quotaTierNames.first ?? ""
        lapseTier = existing?.lapseTier ?? defaultLapseTier()
    }

    /// The Save action — shared by the `Button` and `automationActivate`. A blank
    /// admin-tier means nothing has been picked yet (no quota tiers exist): a
    /// no-op rather than a request the nest would refuse with
    /// `fauna.admin.invalid_params` (mirrors linux's / android's early-return).
    private func save() {
        guard !adminTier.isEmpty else { return }
        Task { await onSave(selectedTierName, adminTier, lapseTier) }
    }

    /// The Clear action — shared by the `Button` and `automationActivate`.
    private func clear() {
        Task { await onClear(tierName) }
    }
}
