import SwiftUI
import FaunaKit

struct MacSnapshotFileListView: View {
    /// The open snapshot's row, straight off the machine. This used to be a
    /// SwiftData `Snapshot` bridge model plus a separately-threaded `deviceId`
    /// (the bridge model does not carry one) — both are gone: the machine's own
    /// row is the single source, so there is no second shape to keep in sync and
    /// no way for the download's device id to arrive nil because a hop dropped it.
    let snapshot: SnapshotRow
    /// The SAME already-`configure`d instance the page holds.
    var vm: BackupsMachineVM
    @State private var searchText = ""

    var body: some View {
        let filtered = searchText.isEmpty ? vm.detailFiles : vm.detailFiles.filter {
            $0.path.localizedCaseInsensitiveContains(searchText)
        }

        // Plain `List` (not the rule-6 `ScrollView{VStack}` conversion the iOS
        // twin needs) — macOS realizes `List` rows eagerly regardless of scroll
        // position, so the indexed `snapshot-file-download-button` per row still
        // `.onAppear`-registers off-screen (apple-e2e-automation.md rule 6: "macOS
        // realizes eagerly and masks it").
        List(filtered, id: \.path) { file in
            HStack {
                Image(systemName: file.fileType == "dir" ? "folder" : "doc")
                    .foregroundStyle(.secondary)
                VStack(alignment: .leading) {
                    Text(file.path)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(ValueFormat.byteSize(UInt64(max(0, file.sizeBytes))))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                // Without an explicit width, an unconstrained Text's IDEAL size is
                // its full untruncated content width, so a long path can grow this
                // VStack (and the row) past the available column width instead of
                // truncating — parking snapshot-file-download-button off the
                // visible window edge (geo-parked, HIDDEN despite being genuinely
                // rendered). Claiming the flexible leading space forces the Text
                // to truncate into whatever width it's actually given.
                .frame(maxWidth: .infinity, alignment: .leading)
                Spacer(minLength: 8)
                // The affordance is a REGULAR-FILE gesture: a directory or symlink
                // row gets no dead download button (the shape tui's lead leg
                // recorded on its ledger row so a following one would not have to
                // rediscover it).
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
        }
        .accessibilityIdentifier(Ids.snapshotDetailFiles)
        // Read-only anchor for `is_visible("snapshot-detail-files")`.
        .automationValue(Ids.snapshotDetailFiles, text: { "" })
        .searchable(text: $searchText)
        .navigationTitle(L.backups.snapshot(id: String(snapshot.id)))
    }

    /// Fetch the file's bytes then save them through the shared
    /// `SnapshotFileSaver.save` — under e2e straight into its
    /// `e2eDownloadDir` (no dialog), otherwise via `NSSavePanel`.
    private func downloadFile(_ file: SnapshotFileRow) async {
        // No `guard let deviceId` here: a missing device id is refused (loudly,
        // onto `error-message`) inside `downloadSnapshotFile` itself, so both
        // apple targets share one refusal instead of each dropping the command
        // silently — convention 11, see the VM's doc comment.
        guard let data = await vm.downloadSnapshotFile(
            deviceId: snapshot.deviceId, snapshotId: snapshot.id, path: file.path)
        else { return }
        let name = (file.path as NSString).lastPathComponent
        try? SnapshotFileSaver.save(suggestedFileName: name, data: data)
    }
}
