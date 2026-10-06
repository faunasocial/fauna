import SwiftUI
import FaunaKit

struct MacSnapshotDetailView: View {
    var vm: BackupsMachineVM
    /// The open snapshot's row, off the machine. Replaces the `SnapshotResponse`
    /// this pane used to hold plus the SwiftData `Snapshot` bridge model it built
    /// for the Files tab — one shape now, and it carries the device id the
    /// download walk resolves against.
    let snapshot: SnapshotRow

    var body: some View {
        TabView {
            Tab(L.common.files, systemImage: "doc.on.doc") {
                MacSnapshotFileListView(snapshot: snapshot, vm: vm)
            }

            Tab(L.backups.diff.title, systemImage: "arrow.left.arrow.right") {
                MacSnapshotDiffView(vm: vm, snapshotId: snapshot.id)
            }

            Tab(L.backups.restore, systemImage: "arrow.uturn.backward") {
                MacRestoreView(snapshotId: Int(snapshot.id))
            }

            Tab(L.fileSync.info, systemImage: "info.circle") {
                snapshotInfoView
            }
        }
        .navigationTitle(L.backups.snapshot(id: String(snapshot.id)))
    }

    // MARK: - Info Tab

    private var snapshotInfoView: some View {
        Form {
            Section {
                LabeledContent(L.backups.detail.snapshotId, value: "\(snapshot.id)")
                LabeledContent(L.backups.detail.created,
                               value: ValueFormat.absoluteDate(epochMs: snapshot.createdAt * 1000, withTime: true))
                LabeledContent(L.backups.detail.deviceId, value: snapshot.deviceId ?? L.common.unknown)
                LabeledContent(L.backups.detail.tags, value: tagsString)
                LabeledContent(L.backups.detail.fileCount, value: "\(snapshot.fileCount)")
                LabeledContent(L.backups.detail.totalSize,
                               value: ValueFormat.byteSize(UInt64(max(0, snapshot.totalBytes))))
                // The lifecycle state, spelled out on the detail as well as the
                // row — this is where a user reads the deadline they can still
                // act on. `Active` renders nothing (the row's own rule).
                if let stateText = SnapshotRowText.state(snapshot.state) {
                    LabeledContent(L.common.status, value: stateText)
                }
            }
        }
        .formStyle(.grouped)
    }

    // MARK: - Helpers

    /// Manual snapshots are untagged by ratification (a tag is a retention
    /// shield), so this is normally "none" — it still renders because an
    /// automatic/imported row can carry tags.
    private var tagsString: String {
        snapshot.tags.isEmpty ? L.backups.detail.none : snapshot.tags.joined(separator: ", ")
    }
}
