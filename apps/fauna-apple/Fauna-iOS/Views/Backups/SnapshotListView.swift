import SwiftUI
import FaunaKit

struct SnapshotListView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = BackupsMachineVM()
    /// The row whose delete confirm is armed. iOS shipped NO confirm before this
    /// leg — `snapshot-delete-button` deleted on the first tap, alone among the
    /// apps (`ui/backups.md` § Snapshot-list shape, *Delete* ruling: "windows and
    /// iOS ship no confirm today and gain one"). Untagged, exactly like macOS's:
    /// the confirm is client glue and ui.yaml scopes it no id.
    @State private var pendingDelete: SnapshotRow?

    var body: some View {
        NavigationStack {
            VStack {
                Picker(L.backups.folder, selection: Binding(
                    get: { vm.selectedFolder ?? "" },
                    set: { selectFolder($0) }
                )) {
                    ForEach(folderNames, id: \.self) { Text($0) }
                }
                .pickerStyle(.segmented)
                .padding(.horizontal)
                .accessibilityIdentifier(Ids.backupFolderSelector)
                // Driver `select("backup-folder-selector", <name>)` runs the same
                // selection path the Picker's binding does; `value` reads the
                // machine's current selection. One Entry (read + select).
                .automationSelect(Ids.backupFolderSelector,
                                  value: { vm.selectedFolder }) { name in
                    selectFolder(name)
                }

                // ONE non-indexed `last-backed-up` for the SELECTED set, rendered
                // unconditionally — an empty set reads "never" rather than dropping
                // the element (§ Snapshot-list shape, *`last-backed-up`* ruling).
                automationText(Ids.lastBackedUp, vm.lastBackedUpText)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let busyOpText = vm.busyOpText {
                    Text(busyOpText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if let result = vm.snapshot?.checkResult {
                    CheckResultView(result: result)
                }
                if let preview = vm.snapshot?.prunePreview {
                    PrunePreviewView(preview: preview, busy: vm.busy,
                                     onExecute: { Task { await vm.pruneExecute() } },
                                     onCancel: { vm.cancelPrunePreview() })
                }

                // Message-kind (mail/calendar) restore surfaces (shared FaunaKit —
                // `backups.md` §§ Restore …); same view macOS uses. In a bounded
                // ScrollView (symmetric with the macOS bottom band) so it lays out
                // reliably above the greedy snapshot List AND the apple-bridge
                // `safeTap` can scroll to reach the divergence banner / inline modal.
                ScrollView {
                    RestoreSectionView()
                        .padding(.horizontal)
                }
                .frame(maxHeight: 400)

                // Cross-location backup-destination management (shared FaunaKit —
                // `backups.md` § Manage backup destinations); same view macOS uses.
                BackupDestinationsView()
                    .padding(.horizontal)

                // Eager `ScrollView { VStack }`, NOT a lazy `List` (rule 6 —
                // apple-e2e-automation.md § Registration rules): a lazy `List` only
                // realizes on-screen (+ small buffer) rows, AND `.swipeActions`
                // content is never laid out (no real geometry) until a user
                // actually swipes, so the automation registry can never observe
                // `snapshot-delete-button`/`snapshot-immediate-delete-button` —
                // confirmed 2026-07-18 (`test_snapshot_delete_button_indexed`).
                // The row's `NavigationLink` also never pushed when driven via
                // the automation registry (same class as iOS `FeedListView`'s
                // `selectedPost` — a tap-driven `NavigationLink` label doesn't
                // fire from an in-process activate closure), so `snapshot-item`
                // carried only a read-only Entry and `driver.click("snapshot-item")`
                // could never open the detail screen
                // (`test_snapshot_detail_files_visible`). Fixed by driving the
                // machine's own `open_snapshot` gesture + `.navigationDestination
                // (item:)` below (mirrors `FeedListView`'s `post-card`) and the two
                // swipe actions to always-visible trailing icon buttons (mirrors
                // `FeedListView`'s `feed-delete-button`). Cost: rows lose `List`
                // inset styling + the swipe gesture (accepted rule-6 tradeoff).
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(vm.rows.enumerated()), id: \.element.id) { offset, snapshot in
                            HStack {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(L.backups.snapshot(id: String(snapshot.id)))
                                        .font(.headline)
                                    HStack {
                                        Text(L.backups.fileCount(count: String(snapshot.fileCount)))
                                        Text("·")
                                        Text(ValueFormat.byteSize(UInt64(max(0, snapshot.totalBytes))))
                                    }
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    Text(Date(epochSeconds: snapshot.createdAt).relativeFormatted)
                                        .font(.caption2)
                                        .foregroundStyle(.secondary)
                                    // A non-Active lifecycle state renders ON the row,
                                    // with the deadline the user can still act on.
                                    if let stateText = SnapshotRowText.state(snapshot.state) {
                                        Text(stateText)
                                            .font(.caption2)
                                            .foregroundStyle(.orange)
                                    }
                                    // Absent until a check runs this session.
                                    if let integrityText = SnapshotRowText.integrity(snapshot.integrity) {
                                        Text(integrityText)
                                            .font(.caption2)
                                            .foregroundStyle(snapshot.integrity == .implicated ? .red : .secondary)
                                    }
                                }
                                Spacer()
                                // Owner-only immediate delete — OPENS the friction-bar
                                // modal, never a one-click delete (backups.md rule 4).
                                Button {
                                    vm.openImmediateDelete(snapshotId: snapshot.id)
                                } label: {
                                    Image(systemName: "bolt.trianglebadge.exclamationmark")
                                        .foregroundStyle(.red)
                                }
                                .buttonStyle(.borderless)
                                .accessibilityIdentifier(Ids.snapshotImmediateDeleteButton)
                                .automationActivate(Ids.snapshotImmediateDeleteButton) {
                                    vm.openImmediateDelete(snapshotId: snapshot.id)
                                }
                                // Arms the confirm — it does NOT delete (this leg's
                                // change; see `pendingDelete`).
                                Button {
                                    pendingDelete = snapshot
                                } label: {
                                    Image(systemName: "trash")
                                        .foregroundStyle(.secondary)
                                }
                                .buttonStyle(.borderless)
                                .accessibilityIdentifier(Ids.snapshotDeleteButton)
                                .automationActivate(Ids.snapshotDeleteButton) {
                                    pendingDelete = snapshot
                                }
                                // Undelete — presence IS the state (§ Snapshot-list
                                // shape → *Soft-deleted rows*): renders ONLY on a
                                // SoftDeleted row, same shape as
                                // `snapshot-prune-execute-button`. Single-flight via
                                // vm.busy.
                                if case .softDeleted = snapshot.state {
                                    Button {
                                        Task { await vm.undeleteSnapshot(id: snapshot.id) }
                                    } label: {
                                        Image(systemName: "arrow.uturn.backward")
                                    }
                                    .buttonStyle(.borderless)
                                    .disabled(vm.busy)
                                    .accessibilityIdentifier(Ids.snapshotUndeleteButton)
                                    .automationActivate(Ids.snapshotUndeleteButton,
                                                        isEnabled: { !vm.busy }) {
                                        Task { await vm.undeleteSnapshot(id: snapshot.id) }
                                    }
                                }
                            }
                            .contentShape(Rectangle())
                            .padding(.vertical, 8)
                            // Whole-row tap opens detail (not a wrapping `Button`,
                            // so the trailing icon buttons keep working — mirrors
                            // `FeedListView`'s `post-card`).
                            .onTapGesture { Task { await vm.openSnapshot(id: snapshot.id) } }
                            .accessibilityIdentifier(Ids.snapshotItem)
                            .automationActivate(Ids.snapshotItem,
                                                text: { SnapshotRowText.line(snapshot) },
                                                value: { String(snapshot.id) }) {
                                Task { await vm.openSnapshot(id: snapshot.id) }
                            }
                            // Scoped container (goal-doc rule 5): supports future
                            // per-row singleton children under
                            // `scope="snapshot-item[N]/..."`.
                            .automationScope(Ids.snapshotItem, index: offset)
                            if offset < vm.rows.count - 1 {
                                Divider()
                            }
                        }
                    }
                    .padding(.horizontal)
                }
                .accessibilityIdentifier(Ids.snapshotList)
                // Read-only anchor for `is_visible("snapshot-list")` (the
                // ScrollView itself). Mirrors macOS MacSnapshotTimelineView.
                .automationValue(Ids.snapshotList, text: { "" })
                .navigationDestination(item: Binding(
                    get: { vm.openSnapshotId.map(OpenSnapshot.init(id:)) },
                    set: { if $0 == nil { vm.closeSnapshotDetail() } }
                )) { open in
                    SnapshotFileListView(snapshotId: open.id, vm: vm)
                }

                if vm.rows.isEmpty {
                    ContentUnavailableView(L.backups.noSnapshots, systemImage: "clock.arrow.circlepath",
                                           description: Text(L.backups.noSnapshotsDesc))
                }

                // Owner-only immediate-delete modal (inline reveal, shared FaunaKit
                // view), shown when a row's immediate-delete button opened it.
                if vm.immediateDeleteSnapshotId != nil {
                    SnapshotImmediateDeleteModal(vm: vm)
                        .padding(.horizontal)
                }
            }
            .safeAreaInset(edge: .bottom) {
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                        .padding()
                }
            }
            .pageTitle(L.backups.title)
            .toolbar {
                ToolbarItemGroup(placement: .primaryAction) {
                    Button(L.common.create) {
                        Task { await vm.createSnapshot() }
                    }
                    .disabled(vm.mutationsDisabled)
                    .accessibilityIdentifier(Ids.snapshotCreateButton)
                    .automationActivate(Ids.snapshotCreateButton,
                                        isEnabled: { !vm.mutationsDisabled }) {
                        Task { await vm.createSnapshot() }
                    }
                    // The tagged button RUNS the check (§ Snapshot-list shape,
                    // *Check* ruling) — it no longer opens a sheet whose own
                    // untagged button was the real actuator.
                    Button(L.common.check) {
                        Task { await vm.check() }
                    }
                    .disabled(vm.mutationsDisabled)
                    .accessibilityIdentifier(Ids.snapshotCheckButton)
                    .automationActivate(Ids.snapshotCheckButton,
                                        isEnabled: { !vm.mutationsDisabled }) {
                        Task { await vm.check() }
                    }
                    // Prune previews THIS SET'S OWN resting policy; execute is
                    // offered from the preview (§ *Prune* ruling).
                    Button(L.common.prune) {
                        Task { await vm.prunePreview() }
                    }
                    .disabled(vm.mutationsDisabled)
                    .accessibilityIdentifier(Ids.snapshotPruneButton)
                    .automationActivate(Ids.snapshotPruneButton,
                                        isEnabled: { !vm.mutationsDisabled }) {
                        Task { await vm.prunePreview() }
                    }
                    // Preview and execute are one kind with a `dry_run` flag,
                    // and it is `OnlineOnly` for the honest reason that a dry
                    // run with no nest would have nothing to dry-run.
                    // Create/delete/undelete beside it stay live — the table
                    // makes them queued or safe.
                    .faunaGate("fauna.filesync.snapshot.prune_set_policy")
                }
            }
            .snapshotDeleteConfirmation(pendingDelete: $pendingDelete) { snapshot in
                Task { await vm.deleteSnapshot(id: snapshot.id) }
            }
            // Keyed on the session's client, not one-shot: this page survives an
            // account switch on iOS — More → Backups (`moreSelectedView` is never
            // cleared) and Settings → Backups (a closure-destination link the
            // `selectedSettingsPage` reset does not pop) — so the nil-client phase
            // drops the outgoing account's snapshots (`BackupsMachineVM.reset()`) and
            // the incoming client rebuilds (`account-scoping.md` § The scoping
            // taxonomy, the "reused shell" case).
            .task(id: SessionKey(client)) {
                if let client {
                    await vm.configure(api: client.api, deviceIdHex: client.deviceId)
                } else {
                    vm.reset()
                }
            }
        }
    }

    /// The actor's real folders, from the machine's owner-scoped
    /// `fauna.folders.list` read — never a hardcoded universe, and never the
    /// `fauna.sync.backup_status` read the ratified selector source replaced.
    private var folderNames: [String] {
        (vm.snapshot?.folders ?? []).map(\.name)
    }

    /// See `BackupsMachineVM.selectFolder` — the equality guard inside it is
    /// what stops the picker's set-binding from dispatching a selection for the
    /// value the machine just handed back.
    private func selectFolder(_ name: String) {
        Task { await vm.selectFolder(name) }
    }
}

/// `Identifiable` wrapper so `.navigationDestination(item:)` can key on the
/// machine's open-detail id (an `Int64` is not `Identifiable`).
private struct OpenSnapshot: Identifiable, Hashable {
    let id: Int64
}
