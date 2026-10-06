import SwiftUI

/// The **message-kind (mail/calendar) restore** section of the Backups page
/// (`docs/goal/ui/backups.md` §§ Restore from backup destination / Restore history
/// / Restore divergence), shared by macOS + iOS (one FaunaKit view). Renders the
/// 13 `restore-*` ui.yaml ids over `RestoreVM`; a dumb renderer, no WS-RPC logic
/// here. Reference: android `LocalRestoreSection` + `RestoreHistorySection` (all
/// 13, 2026-06-15) and linux `apps/fauna-linux/src/views/backups/restore.rs`.
///
/// Like both references the wired action is the **local single-snapshot restore**
/// (pick a snapshot, re-type its id, dispatch `restore_message_kind` once). The
/// `restore-source-select` is a decorative disabled-at-zero-destinations affordance
/// and the two `restore-kind-checkbox`es are decorative parity (the dispatch
/// ignores them) — the cross-location pair-restore they'd drive is blocked on
/// backup-Plan 4. The restore endpoint requires the bridge not be serving the
/// actor; the user arranges that via the existing `mail-settings-enabled-toggle`
/// (progress ends "Done — restart the bridge.") — not client-driven sequencing.
///
/// Rendering mirrors the proven cross-platform `BackupDestinationsView`: eager
/// `VStack` + `ForEach` rows (never a lazy `List`, so every indexed row's
/// `automation*` registration fires), each id-bearing container with children
/// carrying `.accessibilityElement(children: .contain)` (memory
/// `apple-section-accessibilityid-clobbers-children`), and the divergence-details
/// modal as an **inline reveal** (not a `.sheet`) so it stays in the reachable AX
/// tree. Every interactive/readable element pairs its `.accessibilityIdentifier`
/// with an `automation*` modifier (the only surface the in-process driver sees).
public struct RestoreSectionView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = RestoreVM()

    public init() {}

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            LocalRestoreAction(vm: vm)
            historySection
            if vm.modalRows != nil {
                divergenceModal
            }
        }
        .task {
            guard let client else { return }
            vm.configure(api: client.api)
            await vm.hydrate()
        }
    }

    // MARK: - Restore history (read surface)

    /// The collapsible-in-spec restore-history section. Rendered always-expanded
    /// (matching linux) so every row's registration lays out reliably; the section
    /// is only meaningfully populated after a restore. Each row carries its own
    /// scope so `restore-history-item[i]` → `restore-divergence-banner` resolves.
    private var historySection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.backups.restoreSectionTitle)
                .font(.headline)
            VStack(alignment: .leading, spacing: 6) {
                if vm.history.isEmpty {
                    Text(L.backups.restoreNoSnapshots)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(Array(vm.history.enumerated()), id: \.element.id) { index, row in
                        historyRow(index: index, row: row)
                    }
                }
            }
            .accessibilityIdentifier(Ids.restoreHistoryList)
            .automationValue(Ids.restoreHistoryList, text: { "" })
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.restoreHistorySection)
        .automationValue(Ids.restoreHistorySection, text: { L.backups.restoreSectionTitle })
    }

    /// One `restore-history-item`. Carries the row text (read via `get_text`) + a
    /// scope (so the banner child resolves under `restore-history-item[i]`) + the
    /// `.contain` guard so the row id doesn't clobber the banner child id. The
    /// divergence banner appears reactively once the per-row divergence read lands
    /// (the body re-reads `vm.hasDivergence`), matching android/linux.
    private func historyRow(index: Int, row: FfiRestoreHistoryRow) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(vm.historyRowText(row))
                .font(.body)
                .frame(maxWidth: .infinity, alignment: .leading)
            if vm.hasDivergence(row.snapshotId) {
                Button {
                    vm.openDivergenceModal(snapshotId: row.snapshotId)
                } label: {
                    Label(vm.divergenceBannerText(row.snapshotId),
                          systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.orange)
                .accessibilityIdentifier(Ids.restoreDivergenceBanner)
                // Both readable (banner count) AND clickable (opens the modal),
                // one Entry — resolved under this row's scope.
                .automationActivate(Ids.restoreDivergenceBanner,
                                    value: { vm.divergenceBannerText(row.snapshotId) }) {
                    vm.openDivergenceModal(snapshotId: row.snapshotId)
                }
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.restoreHistoryItem)
        .automationValue(Ids.restoreHistoryItem, text: { vm.historyRowText(row) })
        .automationScope(Ids.restoreHistoryItem, index: index)
    }

    // MARK: - Divergence details modal (inline reveal, forensic / close-only)

    private var divergenceModal: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.backups.restoreDivergenceModalTitle)
                .font(.headline)
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array((vm.modalRows ?? []).enumerated()), id: \.element.id) { _, row in
                    automationText(Ids.restoreDivergenceDetailsItem, vm.divergenceDetailText(row))
                        .font(.caption)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            Text(L.backups.restoreDivergenceFooter)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button(L.backups.restoreDivergenceClose) { vm.closeDivergenceModal() }
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.restoreDivergenceDetailsModal)
        .automationValue(Ids.restoreDivergenceDetailsModal,
                         text: { L.backups.restoreDivergenceModalTitle })
    }
}

// MARK: - Local restore action card

/// The local-restore action (`restore-source-select`, `restore-snapshot-select`,
/// `restore-kinds-checkboxes` + the two `restore-kind-checkbox`es,
/// `restore-confirm-input`, `restore-confirm-button`, `restore-progress`). A
/// separate struct so the two-way bindings (`confirmText`, the checkboxes, the
/// snapshot selection) can `@Bindable`-bind the VM.
private struct LocalRestoreAction: View {
    @Bindable var vm: RestoreVM

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.backups.restoreLocalTitle)
                .font(.headline)

            sourceSelect
            snapshotSelect
            kindsCheckboxes

            TextField(L.backups.restoreConfirmPlaceholder, text: $vm.confirmText)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.restoreConfirmInput)
                .automationField(Ids.restoreConfirmInput, text: $vm.confirmText)

            Button(L.backups.restoreConfirmButton) { Task { await vm.restore() } }
                .disabled(!vm.confirmEnabled)
                .accessibilityIdentifier(Ids.restoreConfirmButton)
                .automationActivate(Ids.restoreConfirmButton,
                                    isEnabled: { vm.confirmEnabled }) {
                    Task { await vm.restore() }
                }
                // Typing the confirm phrase is local; the restore itself is
                // `fauna.filesync.snapshot.restore_message_kind`, so only the
                // commit gates.
                .faunaGate("fauna.filesync.snapshot.restore_message_kind")

            automationText(Ids.restoreProgress, vm.progress.text)
                .font(.caption)
                .foregroundStyle(.secondary)

            if vm.configAbsent {
                automationText(Ids.restoreWarning, L.backups.restoreWarningConfigAbsent)
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let error = vm.errorMessage {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// `restore-source-select` — the cross-location destination picker, a decorative
    /// disabled-at-zero-destinations affordance in v1 (destination chunk-pull blocked
    /// on backup-Plan 4). Shows the empty-destinations placeholder when disabled;
    /// `isEnabled` tracks whether ≥1 destination is configured (android's shape).
    private var sourceSelect: some View {
        HStack {
            Text(vm.hasDestinations ? "" : L.backups.backupDestinationsEmpty)
                .foregroundStyle(.secondary)
            Spacer()
            Image(systemName: "chevron.down")
                .font(.caption)
                .foregroundStyle(.tertiary)
        }
        .padding(8)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .stroke(Color.secondary.opacity(0.3)))
        .opacity(vm.hasDestinations ? 1 : 0.5)
        .accessibilityIdentifier(Ids.restoreSourceSelect)
        .automationValue(Ids.restoreSourceSelect,
                         text: { vm.hasDestinations ? "" : L.backups.backupDestinationsEmpty },
                         isEnabled: { vm.hasDestinations })
    }

    /// `restore-snapshot-select` — the local-snapshot picker, populated from
    /// `fauna.filesync.snapshot.list`. Auto-selects the first snapshot on load
    /// (linux's single-snapshot behaviour); disabled when empty.
    private var snapshotSelect: some View {
        Picker(L.backups.restoreLocalTitle, selection: $vm.selectedSnapshotId) {
            ForEach(vm.snapshots, id: \.id) { s in
                Text(vm.snapshotLabel(s)).tag(Optional(s.id))
            }
        }
        .labelsHidden()
        .disabled(vm.snapshots.isEmpty)
        .accessibilityIdentifier(Ids.restoreSnapshotSelect)
        .automationSelect(Ids.restoreSnapshotSelect,
                          value: { vm.selectedSnapshotLabel },
                          isEnabled: { !vm.snapshots.isEmpty }) { wire in
            if let s = vm.snapshots.first(where: {
                String($0.id) == wire || vm.snapshotLabel($0) == wire
            }) {
                vm.selectedSnapshotId = s.id
            }
        }
    }

    /// `restore-kinds-checkboxes` container + the two `restore-kind-checkbox`es
    /// (mail, calendar), both checked by default. Decorative parity (the local
    /// dispatch ignores them). Custom checkbox buttons (uniform on both platforms;
    /// the automation value reads "on"/"off").
    private var kindsCheckboxes: some View {
        HStack(spacing: 16) {
            kindCheckbox(label: L.backups.restoreKindsMail, isOn: $vm.mailChecked)
            kindCheckbox(label: L.backups.restoreKindsCalendar, isOn: $vm.calendarChecked)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.restoreKindsCheckboxes)
        .automationValue(Ids.restoreKindsCheckboxes, text: { "" })
    }

    private func kindCheckbox(label: String, isOn: Binding<Bool>) -> some View {
        Button {
            isOn.wrappedValue.toggle()
        } label: {
            HStack(spacing: 6) {
                Image(systemName: isOn.wrappedValue ? "checkmark.square.fill" : "square")
                Text(label)
            }
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(Ids.restoreKindCheckbox)
        .automationActivate(Ids.restoreKindCheckbox,
                            value: { isOn.wrappedValue ? "on" : "off" }) {
            isOn.wrappedValue.toggle()
        }
    }
}
