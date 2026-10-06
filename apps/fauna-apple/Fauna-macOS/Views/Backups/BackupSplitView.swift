import SwiftUI
import FaunaKit

struct BackupSplitView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = BackupsMachineVM()
    @State private var showStatsPopover = false

    init() {}

    /// Layout-probe seam: host this view over a pre-populated VM so the headless
    /// `NSHostingView` pane measurement in `BackupSplitViewLayoutTests` can assert
    /// the three-pane packing in the *populated* state (an empty folder List and
    /// a closed detail both under-measure, which is precisely why the pane
    /// overflow this test pins was invisible to every non-e2e check).
    init(vmForTest: BackupsMachineVM) {
        _vm = State(initialValue: vmForTest)
    }

    var body: some View {
        VStack(spacing: 0) {
            HSplitView {
                folderSidebar
                    .frame(minWidth: 220, maxWidth: 300)

                snapshotContent

                if let row = openRow {
                    MacSnapshotDetailView(vm: vm, snapshot: row)
                        .frame(minWidth: 300)
                } else {
                    ContentUnavailableView(L.backups.noSnapshots,
                        systemImage: "clock.arrow.circlepath",
                        description: Text(L.backups.noSnapshotsDesc))
                        .frame(minWidth: 300)
                }
            }
            .frame(maxHeight: .infinity)

            Divider()

            // Cross-location backup-destination management (shared FaunaKit —
            // `backups.md` § Manage backup destinations); same view iOS uses. Lives
            // beneath the snapshot/restore surfaces, mirroring the linux page.
            // ⚠ Fixed height (not `maxHeight`) + `layoutPriority` so the greedy
            // `HSplitView` above (`maxHeight: .infinity`) can't squeeze this section
            // to ~0 — a zero-height container never lays out its content, so
            // `BackupDestinationsView`'s `automation*` `.onAppear` registrations
            // would never fire and the in-process driver sees an empty registry
            // (`is_visible("backup-destination-add-button") == false`,
            // `test_backup_destination_crud[macos]`).
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    // Message-kind (mail/calendar) restore surfaces (shared FaunaKit
                    // — `backups.md` §§ Restore from backup destination / Restore
                    // history / Restore divergence); the same view iOS uses. Above the
                    // destination-management section, matching the goal-doc order. The
                    // divergence-details modal is an inline reveal reached by the
                    // apple-bridge `safeTap` scroll-into-view within this ScrollView.
                    RestoreSectionView()
                    Divider()
                    BackupDestinationsView()
                }
                .padding()
            }
            .frame(height: 420)
            .layoutPriority(1)
        }
        .accessibilityElement(children: .contain)
        .pageTitle(L.backups.title)
        .task {
            if let client {
                await vm.configure(api: client.api, deviceIdHex: client.deviceId)
            }
        }
    }

    /// The row the machine's open detail belongs to. The machine is the single
    /// source of "which snapshot is open" — this page carries no `selectedSnapshot`
    /// of its own, so a detail whose row a re-read drops closes the pane by
    /// construction (§ Snapshot-list shape — the detail is keyed to the row
    /// clicked, and one no longer listed is closed, not rendered).
    private var openRow: SnapshotRow? {
        guard let id = vm.openSnapshotId else { return nil }
        return vm.snapshot?.snapshots.first { $0.id == id }
    }

    // MARK: - Sidebar

    private var folderSidebar: some View {
        List(vm.snapshot?.folders ?? [], id: \.name, selection: Binding(
            get: { vm.selectedFolder },
            set: { newValue in selectFolder(newValue) }
        )) { folder in
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(folder.name)
                        .font(.headline)
                    Spacer()
                }
                // NOTE: no `last-backed-up` here. It is ONE non-indexed element for
                // the SELECTED set (§ Snapshot-list shape, *`last-backed-up`*
                // ruling) and lives in `snapshotContent` below; rendering it per
                // sidebar row was macOS's ui.yaml scope deviation, retired on this
                // leg. The unselected sets' own `lastSnapshotAt` still renders — as
                // an untagged caption, which is what it always was semantically.
                if let at = folder.lastSnapshotAt {
                    Text(Date(epochSeconds: at).relativeFormatted)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                HStack {
                    Text(L.backups.snapshotCount(count: String(folder.snapshotCount)))
                    Spacer()
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            .padding(.vertical, 2)
            .tag(folder.name)
        }
        .frame(minWidth: 220)
        .accessibilityIdentifier(Ids.backupFolderSelector)
        // Driver `select("backup-folder-selector", <name>)` runs the same
        // selection path the List's set-binding does (extracted into
        // `selectFolder`). `value` reads the machine's current selection.
        // One Entry (read + select).
        .automationSelect(Ids.backupFolderSelector,
                          value: { vm.selectedFolder }) { name in
            selectFolder(name)
        }
    }

    // MARK: - Content

    private var snapshotContent: some View {
        VStack(spacing: 0) {
            HStack {
                Button {
                    Task { await vm.createSnapshot() }
                } label: {
                    Image(systemName: "plus")
                }
                .help(L.backups.createSnapshot)
                .disabled(vm.mutationsDisabled)
                .accessibilityIdentifier(Ids.snapshotCreateButton)
                .automationActivate(Ids.snapshotCreateButton,
                                    isEnabled: { !vm.mutationsDisabled }) {
                    Task { await vm.createSnapshot() }
                }

                // The tagged button RUNS the check (§ Snapshot-list shape, *Check*
                // ruling) — it no longer opens a sheet whose own untagged button
                // was the real actuator. The verdict lands in `CheckResultView`
                // below, never on `error-message`.
                Button {
                    Task { await vm.check() }
                } label: {
                    Image(systemName: "checkmark.shield")
                }
                .help(L.backups.checkButton)
                .disabled(vm.mutationsDisabled)
                .accessibilityIdentifier(Ids.snapshotCheckButton)
                .automationActivate(Ids.snapshotCheckButton,
                                    isEnabled: { !vm.mutationsDisabled }) {
                    Task { await vm.check() }
                }

                // Prune = apply this set's OWN resting retention policy, preview
                // first (§ Snapshot-list shape, *Prune* ruling). The button starts
                // the dry run; `PrunePreviewView` offers execute/cancel.
                Button {
                    Task { await vm.prunePreview() }
                } label: {
                    Image(systemName: "scissors")
                }
                .help(L.backups.pruneButton)
                .disabled(vm.mutationsDisabled)
                .accessibilityIdentifier(Ids.snapshotPruneButton)
                .automationActivate(Ids.snapshotPruneButton,
                                    isEnabled: { !vm.mutationsDisabled }) {
                    Task { await vm.prunePreview() }
                }
                // Preview and execute are one kind with a `dry_run` flag, and
                // it is `OnlineOnly` for the honest reason that a dry run with
                // no nest would have nothing to dry-run. Create/delete/undelete
                // beside it stay live — the table makes them queued or safe.
                .faunaGate("fauna.filesync.snapshot.prune_set_policy")

                Button {
                    showStatsPopover.toggle()
                } label: {
                    Image(systemName: "chart.bar")
                }
                .help(L.backups.statistics)
                .popover(isPresented: $showStatsPopover) {
                    if let folder = vm.selectedFolder {
                        BackupStatsPopover(vm: vm, folder: folder)
                    }
                }

                Spacer()

                // The single-flight indicator: every mutating control above is
                // disabled while an op runs, so the page names which one rather
                // than leaving dead buttons unexplained.
                if let busyOpText = vm.busyOpText {
                    Text(busyOpText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal)
            .padding(.vertical, 6)

            // ONE non-indexed `last-backed-up` for the selected set.
            automationText(Ids.lastBackedUp, vm.lastBackedUpText)
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal)
                .padding(.bottom, 4)

            if let result = vm.snapshot?.checkResult {
                CheckResultView(result: result)
            }
            if let preview = vm.snapshot?.prunePreview {
                PrunePreviewView(preview: preview, busy: vm.busy,
                                 onExecute: { Task { await vm.pruneExecute() } },
                                 onCancel: { vm.cancelPrunePreview() })
            }

            Divider()

            deviceFilterBar
            Divider()
            MacSnapshotTimelineView(vm: vm)
        }
        // Bounded like `folderSidebar` (min 220 / max 300) — the detail pane
        // to the right is the one genuinely flexible/filling column. Two
        // uncapped `minWidth`-only panes side by side left `HSplitView`'s
        // initial-layout pass free to size this middle pane past its actual
        // need, starving the detail pane below its own `minWidth: 300` and
        // pushing `snapshot-detail-files`/`snapshot-file-download-button` past
        // the window's right edge regardless of row content — live-traced
        // during e2e verification; a prior per-row `Text` width fix changed
        // nothing because the overflow is container-level, not row-level.
        .frame(minWidth: 300, maxWidth: 450)
    }

    // MARK: - Device Filter

    private var deviceFilterBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                Button {
                    vm.selectedDeviceId = nil
                } label: {
                    Text(L.backups.allDevices)
                        .font(.caption)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(vm.selectedDeviceId == nil ? Color.accentColor : Color.secondary.opacity(0.2))
                        .foregroundStyle(vm.selectedDeviceId == nil ? .white : .primary)
                        .clipShape(Capsule())
                }
                .buttonStyle(.plain)

                ForEach(vm.devices) { device in
                    Button {
                        vm.selectedDeviceId = device.deviceId
                    } label: {
                        Text(device.label)
                            .font(.caption)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 4)
                            .background(vm.selectedDeviceId == device.deviceId ? Color.accentColor : Color.secondary.opacity(0.2))
                            .foregroundStyle(vm.selectedDeviceId == device.deviceId ? .white : .primary)
                            .clipShape(Capsule())
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
        }
    }

    // MARK: - Helpers

    /// Shared by the folder List's selection binding and the
    /// `backup-folder-selector` `automationSelect`, so a driver `select` runs
    /// the identical production selection path.
    ///
    /// The equality guard inside `vm.selectFolder` is load-bearing — see its doc
    /// comment (the re-entry shape linux's leg recorded on the ledger row).
    private func selectFolder(_ name: String?) {
        guard let name else { return }
        Task { await vm.selectFolder(name) }
    }
}
