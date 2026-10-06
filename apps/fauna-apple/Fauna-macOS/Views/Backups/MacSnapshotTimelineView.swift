import SwiftUI
import FaunaKit

struct MacSnapshotTimelineView: View {
    var vm: BackupsMachineVM
    @State private var pendingDelete: SnapshotRow?

    var body: some View {
        // Wire order (newest-first) — apps do NOT re-sort
        // (§ Snapshot-list shape, *Row content contract*). The client-side
        // `sorted { createdAt > }` this replaced was a no-op over that order.
        let rows = vm.rows

        VStack(spacing: 0) {
        List(Array(rows.enumerated()), id: \.element.id, selection: Binding(
            get: { vm.openSnapshotId },
            set: { newValue in openSnapshot(newValue) }
        )) { offset, snapshot in
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(Date(epochSeconds: snapshot.createdAt), style: .date)
                        .font(.headline)
                    HStack {
                        Text(Date(epochSeconds: snapshot.createdAt), style: .time)
                        Text(L.backups.fileCount(count: String(snapshot.fileCount)))
                        Text(ValueFormat.byteSize(UInt64(max(0, snapshot.totalBytes))))
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    // A non-Active lifecycle state renders ON the row, with the
                    // deadline the user can still act on — that deadline is why
                    // the wire gained the lifecycle fields at all.
                    if let stateText = SnapshotRowText.state(snapshot.state) {
                        Text(stateText)
                            .font(.caption2)
                            .foregroundStyle(.orange)
                    }
                    // Integrity is ABSENT until a check runs this session:
                    // `Unknown` paints nothing rather than the word "unknown",
                    // which on this page would read as a finding.
                    if let integrityText = SnapshotRowText.integrity(snapshot.integrity) {
                        Text(integrityText)
                            .font(.caption2)
                            .foregroundStyle(snapshot.integrity == .implicated ? .red : .secondary)
                    }
                }

                Spacer()

                if vm.selectedDeviceId == nil, let deviceId = snapshot.deviceId {
                    Text(deviceLabel(for: deviceId))
                        .font(.caption2)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color.secondary.opacity(0.15))
                        .clipShape(Capsule())
                }

                Button(role: .destructive) {
                    pendingDelete = snapshot
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .help(L.backups.deleteSnapshot)
                .accessibilityIdentifier(Ids.snapshotDeleteButton)
                // Per-row indexed Entry (modifier inside the ForEach row). Same
                // action the Button fires: arm the delete-confirm dialog.
                .automationActivate(Ids.snapshotDeleteButton) {
                    pendingDelete = snapshot
                }

                // Owner-only immediate delete — OPENS the friction-bar modal,
                // never a one-click delete (backups.md rule 4). Sibling of the
                // soft-delete trash, per row.
                Button {
                    vm.openImmediateDelete(snapshotId: snapshot.id)
                } label: {
                    Image(systemName: "xmark.bin")
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .foregroundStyle(.red)
                .help(L.backups.immediateDeleteButton)
                .accessibilityIdentifier(Ids.snapshotImmediateDeleteButton)
                // Per-row indexed Entry; same action the Button fires (open the
                // immediate-delete modal for this snapshot).
                .automationActivate(Ids.snapshotImmediateDeleteButton) {
                    vm.openImmediateDelete(snapshotId: snapshot.id)
                }

                // Undelete — presence IS the state (§ Snapshot-list shape →
                // *Soft-deleted rows*): renders ONLY on a SoftDeleted row, same
                // shape as `snapshot-prune-execute-button`. Single-flight via
                // vm.busy, mirroring the create/check/prune buttons above.
                if case .softDeleted = snapshot.state {
                    Button {
                        Task { await vm.undeleteSnapshot(id: snapshot.id) }
                    } label: {
                        Image(systemName: "arrow.uturn.backward")
                    }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .disabled(vm.busy)
                    .help(L.backups.snapshotUndeleteButton)
                    .accessibilityIdentifier(Ids.snapshotUndeleteButton)
                    .automationActivate(Ids.snapshotUndeleteButton,
                                        isEnabled: { !vm.busy }) {
                        Task { await vm.undeleteSnapshot(id: snapshot.id) }
                    }
                }
            }
            .padding(.vertical, 2)
            .tag(snapshot.id)
            // `.contain` (not a bare id on the row) so the per-row
            // `snapshot-delete-button` keeps its own identifier instead of being
            // overwritten by the row's — mirrors `device-card` in DevicesContent
            // (memory: apple-section-accessibilityid-clobbers-children).
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.snapshotItem)
            // Driver `click("snapshot-item", index)` opens this row's detail (same
            // as the List selection binding). The `value` closure exposes the
            // snapshot id so the immediate-delete e2e can read the row's id to
            // re-type (the apple twin of web's data-snapshot-id; iOS exposes the
            // same on its `snapshot-item`). `text` is the row's rendered line
            // (the shared `SnapshotRowText.line`), what `get_text` answers on
            // every app. Per-row indexed Entry.
            .automationActivate(Ids.snapshotItem,
                                text: { SnapshotRowText.line(snapshot) },
                                value: { String(snapshot.id) }) {
                openSnapshot(snapshot.id)
            }
            // Scoped container (goal-doc rule 5): supports future per-row
            // singleton children under `scope="snapshot-item[N]/..."`.
            .automationScope(Ids.snapshotItem, index: offset)
        }
        // minHeight (not just minWidth): without it the surrounding VStack's
        // fixed-height chrome (toolbar/last-backed-up/device filter) plus the
        // page's fixed-height destinations section below squeeze this List to
        // ~2 visible rows in the default 1280x800 window — a real, if
        // scrollable, pre-existing squeeze the undelete-button e2e
        // exposed: SwiftUI `List` virtualizes, so a row scrolled off-screen has
        // no automation registry entry at all, not merely an unhit one (unlike
        // a plain `ScrollView`, where `safeTap` already scrolls to reach an
        // off-screen but MOUNTED control). ~5 rows' worth (row height ~41 incl.
        // padding) keeps the largest seeded set's oldest row realized without
        // scrolling — the lifecycle journey's five snapshots, whose
        // non-`Active` rows sit at the old end of the newest-first list (4 rows
        // left the fifth unrendered).
        .frame(minWidth: 280, minHeight: 215)
        .accessibilityIdentifier(Ids.snapshotList)
        // Read-only anchor for `is_visible("snapshot-list")` (the List itself).
        .automationValue(Ids.snapshotList, text: { "" })
        .overlay {
            if rows.isEmpty && !vm.busy {
                ContentUnavailableView(L.backups.noSnapshots,
                    systemImage: "clock.arrow.circlepath",
                    description: Text(L.backups.noSnapshotsDesc))
            }
        }
        .snapshotDeleteConfirmation(pendingDelete: $pendingDelete) { snapshot in
            Task { await vm.deleteSnapshot(id: snapshot.id) }
        }

        // Owner-only immediate-delete modal (inline reveal, shared FaunaKit view),
        // shown when a row's immediate-delete button has opened it.
        if vm.immediateDeleteSnapshotId != nil {
            SnapshotImmediateDeleteModal(vm: vm)
                .padding()
        }
        // Page error surface — the modal's confirm writes the nest's hard-floor
        // rejection here (`error-message`), keeping the modal open for a retry.
        if let error = vm.errorMessage {
            ErrorBanner(message: error)
                .padding(.horizontal)
                .padding(.bottom, 8)
        }
        }
    }

    // MARK: - Helpers

    private func openSnapshot(_ id: Int64?) {
        guard let id else {
            vm.closeSnapshotDetail()
            return
        }
        Task { await vm.openSnapshot(id: id) }
    }

    private func deviceLabel(for deviceId: String) -> String {
        vm.devices.first { $0.deviceId == deviceId }?.label ?? shortId(hex: deviceId)
    }
}
