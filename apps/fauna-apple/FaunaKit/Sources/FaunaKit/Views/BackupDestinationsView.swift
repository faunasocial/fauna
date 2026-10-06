import SwiftUI

/// The cross-location **backup-destination management** section of the Backups
/// page (`docs/goal/ui/backups.md` § Manage backup destinations), shared by
/// macOS + iOS (one FaunaKit view). A dumb renderer over
/// the thin FFI free fns via `BackupDestinationsVM`; no enroll/persist logic here.
/// Reference implementation: linux
/// `apps/fauna-linux/src/views/backups/destinations.rs`.
///
/// The add/edit dialog (`backup-destination-add-modal`) and the remove-confirm
/// dialog (`backup-destination-remove-confirm-modal`) are **inline reveals** (a
/// conditionally-shown `VStack` in the same scroll), not `.sheet`s — matching
/// linux's deliberate choice to keep them in the reachable AX tree, and riding the
/// single-`ScrollView` scroll-into-view the apple-bridge `safeTap` now does.
/// Every id-bearing container with children carries
/// `.accessibilityElement(children: .contain)` so a bare container id does not
/// clobber its child ids (memory `apple-section-accessibilityid-clobbers-children`).
/// Element IDs match `tests/e2e-unified/ui.yaml` `backups` page exactly.
///
/// **In-process automation.** Every interactive/readable element pairs its
/// `.accessibilityIdentifier` (the XCUITest carrier) with an `automation*`
/// modifier (`automationActivate` / `automationField` / `automationValue` /
/// `automationText`) — the only surface the in-process driver can see, since
/// SwiftUI has no usable in-process a11y tree (`AutomationRegistry`). Without it
/// the whole section is invisible in-process (`is_visible(add-button)==false` —
/// the original blind-authored 2026-06-15 view shipped bare ids only).
public struct BackupDestinationsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = BackupDestinationsVM()

    public init() {}

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if vm.formVisible {
                AddEditForm(vm: vm)
            }
            auditAlerts
            destinationList
            if vm.removingId != nil {
                removeConfirm
            }
            if let held = vm.orphanedStoreBytes {
                orphanedStoreRow(held: held)
            }
            if vm.reclaiming {
                reclaimConfirm
            }
            if vm.reseedConfirming {
                reseedConfirm
            }
            // `backup-destination-reseed-result` — the running line, then the
            // shared result lines; absent until a first restore runs.
            if let result = vm.reseedResultText {
                automationText(Ids.backupDestinationReseedResult, result)
                    .font(.caption)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }
        }
        .task {
            guard let client else { return }
            vm.configure(api: client.api, deviceId: client.deviceId, custodianStore: client)
            await vm.hydrate()
        }
    }

    // MARK: - Header (title + description + always-present add button)

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.backups.backupDestinationsTitle)
                .font(.title3.bold())
            Text(L.backups.backupDestinationsDesc)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if vm.showSoleClientDestinationWarning {
                automationText(
                    Ids.backupSoleClientDestinationWarning,
                    L.backups.backupSoleClientDestinationWarning)
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            Button(L.backups.backupDestinationAddButton) { vm.openAddForm() }
                .accessibilityIdentifier(Ids.backupDestinationAddButton)
                .automationActivate(Ids.backupDestinationAddButton) { vm.openAddForm() }
        }
    }

    // MARK: - Audit-failure banners (above the destination list)

    /// `backup-audit-alert`, indexed — one per destination in a failing state
    /// (freshness / inclusion / overdue, or a client-device custodian's own
    /// `SelfReported` failure — `ui/backups.md` § Audit-alert surface → *The
    /// client-device arm*), rendering only then. A **flat** list, not nested
    /// inside each `backup-destination-status-row` — the e2e reads it via a
    /// bare `driver.count("backup-audit-alert")`, no row scope. Built as a
    /// text list, not two `ForEach`s keyed on `destinationId` — a destination
    /// could in principle carry both an owner-side and a self-reported
    /// reason, and two `ForEach`s over the same id would collide.
    @ViewBuilder
    private var auditAlerts: some View {
        let ownerSide = vm.destinations.compactMap { vm.alertText(for: $0) }
        let texts = ownerSide + vm.selfReportedAlertTexts()
        if !texts.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(texts.enumerated()), id: \.offset) { _, text in
                    automationText(Ids.backupAuditAlert, text)
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }
        }
    }

    // MARK: - Status-row list

    private var destinationList: some View {
        VStack(alignment: .leading, spacing: 6) {
            if vm.isEmpty {
                Text(L.backups.backupDestinationsEmpty)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(vm.destinations.enumerated()), id: \.element.destinationId) { index, dest in
                    statusRow(dest, index: index)
                }
            }
        }
    }

    /// One `backup-destination-status-row`. Indexed (one per destination); the
    /// row carries the id + `.contain` so the per-row child ids stay queryable.
    /// The status numbers render the LIVE `BackupCoordinator::destination_status()`
    /// read (`vm.lastUploadText` / `vm.backlogText`), uniform with linux. Until a
    /// per-app always-on upload coordinator runs, `last_upload_time` is `nil`
    /// ("never") while `backlog_count` is a live count of un-uploaded segments.
    private func statusRow(_ dest: FfiBackupDestinationView, index: Int) -> some View {
        HStack(alignment: .top, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(vm.label(for: dest))
                    .font(.body)
                automationText(Ids.backupDestinationKindBadge, vm.kindBadgeText(for: dest))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                if vm.isClientDeviceKind(dest) {
                    automationText(Ids.backupDestinationUsage, vm.usageText(for: dest))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                automationText(Ids.backupDestinationLastUploadTime,
                               vm.lastUploadText(for: dest))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                automationText(Ids.backupDestinationBacklogCount,
                               vm.backlogText(for: dest))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                automationText(Ids.backupDestinationLastAuditTime,
                               vm.auditCellText(for: dest))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                // The post-succession review pair (`succession-aftermath.md`
                // § Re-key scope → *Adjudicating what the aftermath carries
                // across*; tui and web are the prior art). Three rules carried
                // with it: ABSENT, not empty, on an ordinary row — after a
                // recovery almost every row is the owner's own, and a
                // permanently rendered mark trains the user past the one that
                // matters; Remove is NOT re-rendered — the row's own
                // `backup-destination-remove-button` already is it; Keep gets no
                // confirm, being non-destructive and re-decidable.
                if dest.unattested {
                    automationText(Ids.backupDestinationUnattestedMark,
                                   L.backups.backupDestinationUnattestedMark)
                        .font(.caption)
                        .foregroundStyle(.orange)
                    Button(L.backups.backupDestinationKeepButton) {
                        Task { await vm.keep(dest.destinationId) }
                    }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.backupDestinationKeepButton)
                    .automationActivate(Ids.backupDestinationKeepButton) {
                        Task { await vm.keep(dest.destinationId) }
                    }
                }
            }
            Spacer(minLength: 8)
            // Re-seed — on this device's own custodian row only (`reseedRows`).
            if vm.reseedOnRow(dest) {
                reseedButton
            }
            Button(L.backups.backupDestinationEditButton) { vm.openEditForm(dest) }
                .buttonStyle(.borderless)
                .accessibilityIdentifier(Ids.backupDestinationEditButton)
                .automationActivate(Ids.backupDestinationEditButton) { vm.openEditForm(dest) }
            Button(L.backups.backupDestinationRemoveButton) { vm.armRemove(dest.destinationId) }
                .buttonStyle(.borderless)
                .foregroundStyle(.red)
                .accessibilityIdentifier(Ids.backupDestinationRemoveButton)
                .automationActivate(Ids.backupDestinationRemoveButton) { vm.armRemove(dest.destinationId) }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.backupDestinationStatusRow)
        // Registry read so the in-process driver can count rows + read each row's
        // display name (`destination_text(index)` → `get_text` on this id); value
        // = the same label the row renders. Read by STABLE id (not the captured
        // value-type `dest`) so a rename — which keeps the row's `ForEach` identity
        // and so never re-fires the `.onAppear` that (re)registers this closure —
        // still reports the fresh name in-process (the children read by id too).
        // (The child `automationText` reads are resolved by the server's per-row
        // scope index — one child per row.)
        .automationValue(Ids.backupDestinationStatusRow,
                         text: { vm.label(forId: dest.destinationId) })
        .automationScope(Ids.backupDestinationStatusRow, index: index)
    }

    // MARK: - Remove-confirm dialog (inline reveal)

    private var removeConfirm: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.backups.backupDestinationRemoveConfirmTitle)
                .font(.headline)
            // `backup-destination-remove-reclaim-checkbox` — client-device rows
            // only (`vm.rowIsAClientDevice`, the shared predicate linux gates
            // on). Removing such a row deliberately KEEPS this device's sealed
            // store — it is the owner's only offline copy — so freeing it is an
            // opt-in offered at the moment the intent forms, never a side
            // effect of the removal.
            if let dest = vm.removingDestination, vm.rowIsAClientDevice(dest) {
                // One registration, not an `automationValue` stacked on an
                // `automationActivate`: both write the same id's entry, so the
                // second would drop the first's half. `automationActivate`
                // carries the value reader itself (the fleet's toggle idiom —
                // `folder-on-demand-toggle`), and SwiftUI ignores
                // `.accessibilityValue` on a Toggle, so the XCUITest side reads
                // the native switch's own value.
                Toggle(L.backups.backupDestinationRemoveReclaimCheckbox, isOn: $vm.removeReclaim)
                    .accessibilityIdentifier(Ids.backupDestinationRemoveReclaimCheckbox)
                    .automationActivate(
                        Ids.backupDestinationRemoveReclaimCheckbox,
                        value: { vm.removeReclaim ? "on" : "off" }
                    ) { vm.removeReclaim.toggle() }
            }
            HStack {
                Button(L.backups.backupDestinationRemoveCancelButton) { vm.cancelRemove() }
                    .accessibilityIdentifier(Ids.backupDestinationRemoveCancelButton)
                    .automationActivate(Ids.backupDestinationRemoveCancelButton) { vm.cancelRemove() }
                Button(L.backups.backupDestinationRemoveConfirmButton, role: .destructive) {
                    Task { await vm.confirmRemove() }
                }
                .accessibilityIdentifier(Ids.backupDestinationRemoveConfirmButton)
                .automationActivate(Ids.backupDestinationRemoveConfirmButton) {
                    Task { await vm.confirmRemove() }
                }
                // Deregistering tears the destination down at the destination
                // nest *and* at the source nest's registry, so it genuinely needs
                // a connection — the gate says so rather than letting the user
                // press it into an error.
                .faunaGate("fauna.backup.destination.remove")
            }
            // The VISIBLE half of that gate's duty. `.help` is a hover tooltip
            // and the hint is only spoken, so on iOS a user sees a dead button
            // and no reason at all — `ui/README.md` § Copy comprehensibility
            // rule 5 asks for one within eyeshot. Un-id'd chrome, placed under
            // the button row deliberately: it reads as a statement about the
            // destructive action, not about the cancel button beside it (the
            // same placement reasoning `WebSettingsView`'s section caption
            // records). Renders nothing while the nest is reachable.
            FaunaOfflineReason("fauna.backup.destination.remove")
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.backupDestinationRemoveConfirmModal)
        .automationValue(Ids.backupDestinationRemoveConfirmModal,
                         text: { L.backups.backupDestinationRemoveConfirmTitle })
    }

    // MARK: - Reclaim this device's copy

    /// `backup-orphaned-store-row` — painted only while the VM's cached verdict
    /// says this device holds a sealed store no destination row claims
    /// (`backups.md` § Manage backup destinations → *Reclaim this device's
    /// copy*). It is what keeps the gesture reachable after the row it used to
    /// hang off was removed; without it those bytes are unrecoverable from the
    /// app for the life of the install.
    ///
    /// Purely a paint of `vm.orphanedStoreBytes` — the verdict is never computed
    /// here (see that property).
    private func orphanedStoreRow(held: UInt64) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            // A plain `Text`, not an `automationText` under the row's own id:
            // the container below already registers that id, and two
            // registrations of one id overwrite each other. The sentence reaches
            // the driver as the container's `text` (`/element/text` reads it
            // first), the same way `backup-destination-status-row` carries its
            // label.
            Text(vm.orphanedStoreText(held: held))
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
            // The restore first, ahead of the reclaim (`backups.md` § Restore
            // after losing the nest): a store no row claims is, after a lost
            // nest, the owner's way back, and freeing it is the other way out.
            if vm.reseedOnOrphanedStore {
                reseedButton
            }
            Button(L.backups.backupDestinationReclaimButton) { vm.armReclaim() }
                .accessibilityIdentifier(Ids.backupDestinationReclaimButton)
                .automationActivate(Ids.backupDestinationReclaimButton) { vm.armReclaim() }
            // Deliberately NO `.faunaGate`: tui, the lead app, names no wire kind
            // for this gesture (`apps/fauna-tui/src/backups.rs`,
            // `Action::ConfirmReclaim => None`). The whole premise of a
            // client-device copy is that it restores with no nest alive
            // anywhere, so the gesture that frees it must stay live exactly when
            // the network is gone.
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.backupOrphanedStoreRow)
        .automationValue(Ids.backupOrphanedStoreRow,
                         text: { vm.orphanedStoreText(held: held) })
        // A scope, so the driver can reach THIS row's restore button apart from
        // the one a custodian row may also paint (`backup-orphaned-store-row[0]`).
        .automationScope(Ids.backupOrphanedStoreRow, index: 0)
    }

    // MARK: - Restore after losing the nest (re-seed)

    /// One `backup-destination-reseed-button`. It opens the confirm; it never
    /// reaches the store directly. Disarmed while a restore runs. Deliberately
    /// NO `.faunaGate`, like the reclaim beside it: the gesture's whole setting
    /// is a nest that was lost, and the ceremony reaches the rebuilt one itself.
    private var reseedButton: some View {
        Button(L.backups.backupDestinationReseedButton) { vm.armReseed() }
            .disabled(vm.reseedRunning)
            .accessibilityIdentifier(Ids.backupDestinationReseedButton)
            .automationActivate(Ids.backupDestinationReseedButton) { vm.armReseed() }
    }

    /// `backup-destination-reseed-confirm-modal` — the plain confirm pair, the
    /// reclaim modal's idiom. The body states the consequence where the decision
    /// is made; ui.yaml scopes no id to it.
    private var reseedConfirm: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.backups.backupReseedConfirmTitle)
                .font(.headline)
            Text(L.backups.backupReseedConfirmBody)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Button(L.backups.backupReseedCancelButton) { vm.cancelReseed() }
                    .accessibilityIdentifier(Ids.backupDestinationReseedCancelButton)
                    .automationActivate(Ids.backupDestinationReseedCancelButton) { vm.cancelReseed() }
                Button(L.backups.backupReseedConfirmButton) {
                    Task { await vm.confirmReseed() }
                }
                .accessibilityIdentifier(Ids.backupDestinationReseedConfirmButton)
                .automationActivate(Ids.backupDestinationReseedConfirmButton) {
                    Task { await vm.confirmReseed() }
                }
            }
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.backupDestinationReseedConfirmModal)
        .automationValue(Ids.backupDestinationReseedConfirmModal,
                         text: { L.backups.backupReseedConfirmTitle })
    }

    /// `backup-reclaim-confirm-modal` — a plain confirm, no re-type. Reclaiming
    /// ends this device's standalone-restore property, which is why the modal
    /// exists at all; the copy is re-buildable from a fresh pull whenever the
    /// device re-enrolls, which is why it is not the type-to-confirm idiom.
    private var reclaimConfirm: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.backups.backupReclaimConfirmTitle)
                .font(.headline)
            Text(L.backups.backupReclaimConfirmBody)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Button(L.backups.backupReclaimCancelButton) { vm.cancelReclaim() }
                    .accessibilityIdentifier(Ids.backupReclaimCancelButton)
                    .automationActivate(Ids.backupReclaimCancelButton) { vm.cancelReclaim() }
                Button(L.backups.backupReclaimConfirmButton, role: .destructive) {
                    Task { await vm.confirmReclaim() }
                }
                .accessibilityIdentifier(Ids.backupReclaimConfirmButton)
                .automationActivate(Ids.backupReclaimConfirmButton) {
                    Task { await vm.confirmReclaim() }
                }
            }
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.backupReclaimConfirmModal)
        .automationValue(Ids.backupReclaimConfirmModal,
                         text: { L.backups.backupReclaimConfirmTitle })
    }
}

// MARK: - Add/edit dialog (inline reveal)

/// The shared add/edit dialog — one form serves both (the VM's `editingId`
/// distinguishes them via `formTitle`). A separate struct so the two text inputs
/// can `@Bindable`-bind the VM's `urlText` / `nameText`.
private struct AddEditForm: View {
    @Bindable var vm: BackupDestinationsVM

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(vm.formTitle)
                .font(.headline)
            // Add-only: disabled (not hidden) in edit mode — the kind is not
            // an editable property (mirrors linux/tui). Prefilled from the
            // row being edited; never read back while editing.
            Picker(L.backups.backupDestinationKindSelectLabel, selection: $vm.kindInput) {
                ForEach(vm.kindOptions, id: \.tag) {
                    Text($0.label).tag($0.tag).accessibilityIdentifier($0.tag)
                }
            }
            .disabled(vm.editingId != nil)
            .accessibilityIdentifier(Ids.backupDestinationKindSelect)
            .automationSelect(
                Ids.backupDestinationKindSelect,
                value: { vm.kindInput },
                isEnabled: { vm.editingId == nil }
            ) { vm.kindInput = $0 }
            // The swap, not a disable — a custodian has no address at all.
            if !vm.isCustodianKind {
                TextField(L.backups.backupDestinationUrlPlaceholder, text: $vm.urlText)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.backupDestinationUrlInput)
                    .automationField(Ids.backupDestinationUrlInput, text: $vm.urlText)
            }
            TextField(L.backups.backupDestinationNamePlaceholder, text: $vm.nameText)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.backupDestinationNameInput)
                .automationField(Ids.backupDestinationNameInput, text: $vm.nameText)
            if vm.isCustodianKind {
                TextField(L.backups.backupDestinationCapacityPlaceholder, text: $vm.capacityText)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.backupDestinationCapacityInput)
                    .automationField(Ids.backupDestinationCapacityInput, text: $vm.capacityText)
                Text(L.backups.backupDestinationCustodianExposure)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button(L.backups.backupDestinationAddCancel) { vm.cancelForm() }
                    .accessibilityIdentifier(Ids.backupDestinationAddCancelButton)
                    .automationActivate(Ids.backupDestinationAddCancelButton) { vm.cancelForm() }
                Button(L.backups.backupDestinationAddConfirm) { Task { await vm.submitForm() } }
                    .accessibilityIdentifier(Ids.backupDestinationAddConfirmButton)
                    .automationActivate(Ids.backupDestinationAddConfirmButton) {
                        Task { await vm.submitForm() }
                    }
                    // Enrolling resolves the destination's identity over the
                    // network and registers both the writer grant (at the
                    // destination) and the registry row (at the source nest) —
                    // `fauna.account.state.put` is the only offline-safe part of the
                    // sequence, and it is not the part that can succeed alone.
                    .faunaGate("fauna.backup.destination.register")
            }
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.backupDestinationAddModal)
        .automationValue(Ids.backupDestinationAddModal, text: { vm.formTitle })
    }
}
