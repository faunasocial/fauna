import SwiftUI

/// The owner-only **immediate-delete confirmation modal**
/// (`docs/goal/ui/backups.md` § User actions + Architectural rule 4), shared by
/// macOS + iOS (one FaunaKit view). An **inline reveal** (a
/// conditionally-shown `VStack` in the page's scroll, not a `.sheet`) matching
/// `BackupDestinationsView` — it stays in the reachable AX tree and rides the
/// apple-bridge `safeTap` scroll-into-view.
///
/// **The behavioural invariant (rule 4):** this is NEVER a one-click affordance. The
/// per-row `snapshot-immediate-delete-button` only OPENS it; the
/// `immediate-delete-confirm-button` enables ONLY when BOTH the re-typed snapshot id
/// (`immediate-delete-confirm-input`) AND the exact acknowledge phrase
/// (`immediate-delete-acknowledge-input`) match — `vm.immediateDeleteEnabled`. On
/// confirm the dispatch reaches the nest, which re-checks both + the ≥3-active hard
/// floor; on rejection the error surfaces on the page and the modal stays open.
/// Reference implementations: windows `BackupsViewModel` / `BackupsPage`, web
/// `routes/backups/+page.svelte`.
///
/// Every id-bearing container carries `.accessibilityElement(children: .contain)` so
/// a bare container id does not clobber its child ids (memory
/// `apple-section-accessibilityid-clobbers-children`); every interactive/readable
/// element pairs its `.accessibilityIdentifier` with an `automation*` modifier (the
/// only surface the in-process driver sees). Element IDs match `tests/e2e-unified/`
/// `ui.yaml`'s `backups` page exactly.
public struct SnapshotImmediateDeleteModal: View {
    @Bindable private var vm: BackupsMachineVM

    public init(vm: BackupsMachineVM) {
        self._vm = Bindable(wrappedValue: vm)
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.backups.immediateDeleteModalTitle(id: String(vm.immediateDeleteSnapshotId ?? 0)))
                .font(.headline)
            Text(L.backups.immediateDeleteWarning)
                .font(.caption)
                .foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)

            // immediate-delete-confirm-input — re-type the snapshot id.
            TextField(L.backups.immediateDeleteConfirmIdPlaceholder,
                      text: $vm.immediateDeleteConfirmId)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.immediateDeleteConfirmInput)
                .automationField(Ids.immediateDeleteConfirmInput,
                                 text: $vm.immediateDeleteConfirmId)

            // The exact acknowledge phrase, displayed for the user to retype.
            Text(L.backups.immediateDeleteAcknowledgePrompt)
                .font(.caption)
            Text(vm.immediateDeleteAck)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .foregroundStyle(.secondary)

            // immediate-delete-acknowledge-input — type the exact phrase above.
            TextField(L.backups.immediateDeleteAcknowledgePlaceholder,
                      text: $vm.immediateDeleteAcknowledge)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.immediateDeleteAcknowledgeInput)
                .automationField(Ids.immediateDeleteAcknowledgeInput,
                                 text: $vm.immediateDeleteAcknowledge)

            HStack {
                Button(L.backups.immediateDeleteCancelButton) { vm.cancelImmediateDelete() }
                    .accessibilityIdentifier(Ids.immediateDeleteCancelButton)
                    .automationActivate(Ids.immediateDeleteCancelButton) {
                        vm.cancelImmediateDelete()
                    }
                Button(L.backups.immediateDeleteConfirmButton, role: .destructive) {
                    Task { await vm.confirmImmediateDelete() }
                }
                .disabled(!vm.immediateDeleteEnabled)
                .accessibilityIdentifier(Ids.immediateDeleteConfirmButton)
                // `isEnabled` backs the e2e `is_enabled("immediate-delete-confirm-button")`
                // read — the friction-bar invariant the test asserts.
                .automationActivate(Ids.immediateDeleteConfirmButton,
                                    isEnabled: { vm.immediateDeleteEnabled }) {
                    Task { await vm.confirmImmediateDelete() }
                }
                // The hard delete only the nest can perform — unlike the soft
                // `snapshot-delete-button` that armed this modal, which is
                // `OfflineQueued` and stays live. The friction bar's own
                // predicate is untouched: the registry entry reports
                // `gate && vm.immediateDeleteEnabled`, one answer for both.
                .faunaGate("fauna.filesync.snapshot.delete_immediate")
            }
        }
        .padding(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.immediateDeleteConfirmModal)
        .automationValue(Ids.immediateDeleteConfirmModal, text: {
            L.backups.immediateDeleteModalTitle(id: String(vm.immediateDeleteSnapshotId ?? 0))
        })
    }
}
