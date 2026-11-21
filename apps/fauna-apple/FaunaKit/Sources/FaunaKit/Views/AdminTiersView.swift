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
/// type; the Save is painted from the shared `tier_caps_possibility` — blocked with
/// its reason while any cap does not parse through `parse_cap`, never sent with the
/// persisted cap in its place (admin.md § Errors & edge cases). **Membership designations**
/// (monetization.md § Pillar 4) — one `admin-settings-membership-item` row per
/// subscription tier the admin owns, linking it to an admitted/lapsed quota tier;
/// see `MembershipDesignationRow` below.
///
/// `admin-nav-back` is provided by the admin shell rail (macOS) / the navigation
/// stack (iOS), not this page.
public struct AdminTiersView: View {
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
                                          isBusy: vm.isBusy,
                                          onSave: { await vm.saveTier(name: $0, caps: $1) },
                                          onDelete: { await vm.deleteTier(name: $0) })
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

                // The add-a-tier form (admin.md § 3 → *Defining a new tier*), a sibling
                // of the tier rows so it shows on a fresh box with no tiers.
                // It waits for the `tiers.list` read (admin.md § Errors & edge
                // cases, *Controls wait for their read*).
                FaunaPossibilityReason(vm.addTierPossibility)
                TierAddForm(isBusy: vm.isBusy,
                            answer: { vm.addTierAnswer(name: $0, caps: $1) }) { name, inbox, storage, devices, blobSize, feeds in
                    await vm.addTier(name: name, inbox: inbox, storage: storage, devices: devices,
                                     blobSize: blobSize, feeds: feeds)
                }
                .faunaPossible(vm.addTierPossibility)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.adminSettingsTierAddSection)
                .automationValue(Ids.adminSettingsTierAddSection)
                // Scoped container: the five cap inputs are queried via
                // `scope="admin-settings-tier-add-section/..."`, as under a row.
                .automationScope(Ids.adminSettingsTierAddSection)

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
                            clearAnswer: vm.membershipClearAnswer(tierName: tierName),
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
        .adminReadOnEntryAndReconnect(vm, reloadToken: reloadToken)
    }
}

// MARK: - Tier definition row (one editable `admin-settings-tier-item`)

/// One tier row: the name + five raw-i64 cap inputs + a Save button. Integer
/// fields edit as `String` buffers seeded from the persisted caps (re-seeded on
/// every `reloadToken` bump = each refetch). The Save is painted from the shared
/// `tier_caps_possibility` over the five drafts and hands them up as typed; the
/// VM asks the same answer, parses through `parseCap`, PUTs + refetches.
private struct TierDefinitionRow: View {
    let tier: FfiAdminTier
    let reloadToken: Int
    let isBusy: Bool
    let onSave: (_ name: String, _ caps: [String]) async -> Void
    let onDelete: (_ name: String) async -> Void

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
            // Live only while every cap parses (admin.md § Errors & edge cases);
            // `AdminTiersVM.saveTier` asks the same answer at the press.
            .faunaPossible(AdminTiersVM.tierCapsAnswer(caps))
            FaunaPossibilityReason(AdminTiersVM.tierCapsAnswer(caps))

            // Delete (admin.md § 3 → *Removing a tier*): absent on `free`, the
            // one tier the nest never removes — the shared
            // `adminTierDeletePossibility` answer, asked again in `delete()`.
            // No confirm.
            Button(L.admin.settingsPage.deleteTier, role: .destructive) {
                delete()
            }
            .disabled(isBusy)
            .accessibilityIdentifier(Ids.adminSettingsTierDeleteButton)
            .automationActivate(Ids.adminSettingsTierDeleteButton,
                                isEnabled: { !isBusy }) {
                delete()
            }
            .faunaGate("fauna.admin.tiers.delete")
            .faunaPossible(adminTierDeletePossibility(tier: tier.name))
        }
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .task(id: reloadToken) { seed() }
    }

    /// The Save action — shared by the `Button` and `automationActivate` so the
    /// two can never diverge.
    private func save() {
        Task { await onSave(tier.name, caps) }
    }

    /// The five cap drafts, in the order the shared predicate takes them.
    private var caps: [String] { [inbox, storage, devices, blobSize, feeds] }

    /// The Delete action — shared by the `Button` and `automationActivate`.
    private func delete() {
        guard adminTierDeletePossibility(tier: tier.name).isPossible else { return }
        Task { await onDelete(tier.name) }
    }

    private func seed() {
        inbox = String(tier.maxInboxBytes)
        storage = String(tier.maxStorageBytes)
        devices = String(tier.maxDevices)
        blobSize = String(tier.maxBlobSize)
        feeds = String(tier.maxFeeds)
    }
}

// MARK: - Add-a-tier form (`admin-settings-tier-add-section`)

/// The add-a-tier form: name input + the five raw-i64 cap inputs (blank until the
/// admin types) + `admin-settings-tier-add-button`. The button is painted from
/// `answer` — `AdminTiersVM.addTierAnswer`, the page read folded with the shared
/// `tier_create_possibility` over the drafts — and the nest call lives in
/// `AdminTiersVM.addTier`, which asks the same answer; the buffers clear only
/// when it reports success.
private struct TierAddForm: View {
    let isBusy: Bool
    let answer: (_ name: String, _ caps: [String]) -> Possibility
    let onAdd: (_ name: String, _ inbox: String, _ storage: String, _ devices: String,
                _ blobSize: String, _ feeds: String) async -> Bool

    @State private var name = ""
    @State private var inbox = ""
    @State private var storage = ""
    @State private var devices = ""
    @State private var blobSize = ""
    @State private var feeds = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.admin.settingsPage.addTierSection)
                .font(.headline)

            EditableFieldRow(L.admin.settingsPage.addTierName, "admin-settings-tier-add-name-input", $name,
                             labelColor: .secondary, maxWidth: 200)
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

            Button(L.admin.settingsPage.addTier) {
                add()
            }
            .disabled(isBusy)
            .accessibilityIdentifier(Ids.adminSettingsTierAddButton)
            .automationActivate(Ids.adminSettingsTierAddButton,
                                isEnabled: { !isBusy }) {
                add()
            }
            .faunaGate("fauna.admin.tiers.create")
            .faunaPossible(answer(name, [inbox, storage, devices, blobSize, feeds]))
            FaunaPossibilityReason(adminTierCreatePossibility(
                name: name, caps: [inbox, storage, devices, blobSize, feeds]))
        }
        .padding(.vertical, 8)
    }

    /// The Add action — shared by the `Button` and `automationActivate`. Clears
    /// the buffers only once the nest accepted the create.
    private func add() {
        Task {
            if await onAdd(name, inbox, storage, devices, blobSize, feeds) {
                name = ""; inbox = ""; storage = ""; devices = ""; blobSize = ""; feeds = ""
            }
        }
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
    /// The shared `membership_clear_possibility` for this row; the clear asks
    /// it again (`AdminTiersVM.clearMembershipTier`).
    let clearAnswer: Possibility
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
                // Live only with an admitted-at tier picked (admin.md § Errors &
                // edge cases); `AdminTiersVM.saveMembershipTier` asks again.
                .faunaPossible(AdminTiersVM.membershipSaveAnswer(adminTier: adminTier))

                Button(L.admin.settingsPage.membershipClear, role: .destructive) {
                    clear()
                }
                .disabled(isBusy)
                .accessibilityIdentifier(Ids.adminSettingsMembershipClearButton)
                .automationActivate(Ids.adminSettingsMembershipClearButton,
                                    isEnabled: { !isBusy }) {
                    clear()
                }
                .faunaGate("fauna.admin.membership_tiers.clear")
                .faunaPossible(clearAnswer)
            }
            FaunaPossibilityReason(AdminTiersVM.membershipSaveAnswer(adminTier: adminTier))
            FaunaPossibilityReason(clearAnswer)
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
    /// admin-tier (nothing picked — no quota tiers exist) is refused by the VM
    /// with the shared `membership_save_possibility` reason, never sent.
    private func save() {
        Task { await onSave(selectedTierName, adminTier, lapseTier) }
    }

    /// The Clear action — shared by the `Button` and `automationActivate`.
    private func clear() {
        Task { await onClear(tierName) }
    }
}
