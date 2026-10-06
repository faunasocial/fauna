import SwiftUI
import FaunaKit
#if canImport(UIKit)
import UIKit
#endif

struct SnapshotFileListView: View {
    /// The open snapshot's id. The rows themselves come from the machine's
    /// `detail`, which `SnapshotListView` opened before pushing this screen —
    /// this view no longer runs a read of its own.
    let snapshotId: Int64
    var vm: BackupsMachineVM

    var body: some View {
        // Eager `ScrollView { VStack }`, NOT a lazy `List` (rule 6 —
        // apple-e2e-automation.md § Registration rules): the indexed
        // `snapshot-file-download-button` per row needs every row to
        // .onAppear-register regardless of scroll position, which an iOS
        // `List` never does for off-screen rows (macOS's `List` realizes
        // eagerly and is unaffected — see MacSnapshotFileListView). Mirrors
        // SnapshotListView's identical rule-6 conversion.
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(vm.detailFiles.enumerated()), id: \.element.path) { offset, file in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(file.path)
                                .font(.body)
                                .lineLimit(1)
                            Text(ValueFormat.byteSize(UInt64(max(0, file.sizeBytes))))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        // A REGULAR-FILE gesture only — a directory or symlink row
                        // gets no dead affordance (the shape tui's lead leg
                        // recorded on its ledger row).
                        if file.fileType == "regular" {
                            Button {
                                Task { await downloadFile(file) }
                            } label: {
                                Image(systemName: "arrow.down.circle")
                            }
                            .buttonStyle(.borderless)
                            .accessibilityIdentifier(Ids.snapshotFileDownloadButton)
                            .automationActivate(Ids.snapshotFileDownloadButton) {
                                Task { await downloadFile(file) }
                            }
                        }
                    }
                    .padding(.vertical, 8)
                    if offset < vm.detailFiles.count - 1 {
                        Divider()
                    }
                }
            }
            .padding(.horizontal)
        }
        .accessibilityIdentifier(Ids.snapshotDetailFiles)
        // Read-only anchor for `is_visible("snapshot-detail-files")`. Mirrors
        // macOS MacSnapshotFileListView.
        .automationValue(Ids.snapshotDetailFiles, text: { "" })
        .navigationTitle(L.backups.snapshot(id: String(snapshotId)))
        // `snapshot-check-button` lives on the Backups landing page
        // (`SnapshotListView`'s toolbar, matching macOS `BackupSplitView` and
        // ui.yaml's flat `backups` page element scope) — not here.
    }

    /// Fetch the file's bytes then save them through the shared
    /// `SnapshotFileSaver.save` — under e2e straight into its
    /// `e2eDownloadDir` (no dialog), otherwise via a share sheet.
    private func downloadFile(_ file: SnapshotFileRow) async {
        // No device-id guard here: a missing device id is refused (loudly, onto
        // `error-message`) inside `downloadSnapshotFile` itself, so both apple
        // targets share one refusal instead of each dropping the command
        // silently — convention 11, see the VM's doc comment.
        guard let data = await vm.downloadSnapshotFile(
            deviceId: vm.openSnapshotDeviceId, snapshotId: snapshotId, path: file.path)
        else { return }
        let name = (file.path as NSString).lastPathComponent
        try? SnapshotFileSaver.save(suggestedFileName: name, data: data)
    }
}
