import SwiftUI
import FaunaKit

/// Whole-snapshot restore of a folder snapshot into a user-chosen folder, via the
/// shared **client-side walk** (`backup-restore.md` § 4 + § Restoring Files). Each
/// file's manifest + chunks are fetched, decrypted under the owner `BackupKey`,
/// verified, and written locally by the in-process sync engine host — so this
/// works on **sealed** snapshots, which the retired server-side ZIP route refuses.
///
/// Sits inside the open snapshot detail beside `snapshot-detail-files`
/// (`ui/backups.md` § Whole-snapshot restore): a path field the folder picker
/// fills, the restore button, the progress line while the walk runs, and the
/// result line after — a partial result lists the files not written and is a
/// result, never an error.
struct MacRestoreView: View {
    let snapshotId: Int
    @Environment(FaunaClient.self) private var client: FaunaClient?

    @State private var destinationText: String = ""
    @State private var restoreState: RestoreState = .idle

    enum RestoreState: Equatable {
        case idle
        case restoring(path: String)
        /// `path` is the resolved destination *Reveal in Finder* opens; `nil`
        /// when the typed text never resolved to one.
        case finished(line: String, path: String?)
    }

    private var isRestoring: Bool {
        if case .restoring = restoreState { return true }
        return false
    }

    private var canRestore: Bool {
        !destinationText.trimmingCharacters(in: .whitespaces).isEmpty
            && !isRestoring && client?.syncHost != nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                TextField(L.backups.snapshotRestoreDestinationPlaceholder, text: $destinationText)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.snapshotRestoreDestinationInput)
                    .automationField(Ids.snapshotRestoreDestinationInput, text: $destinationText)

                Button(L.backups.chooseDestination) {
                    chooseDestination()
                }
                .controlSize(.small)
                .disabled(isRestoring)

                Button(L.backups.snapshotRestoreButton) {
                    Task { await startRestore() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!canRestore)
                .accessibilityIdentifier(Ids.snapshotRestoreButton)
                .automationActivate(Ids.snapshotRestoreButton, isEnabled: { canRestore }) {
                    Task { await startRestore() }
                }
            }

            switch restoreState {
            case .idle:
                EmptyView()
            case .restoring(let path):
                HStack(spacing: 8) {
                    ProgressView()
                        .controlSize(.small)
                    automationText(
                        Ids.snapshotRestoreProgress,
                        L.backups.snapshotRestoreProgress(id: String(snapshotId), path: path)
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            case .finished(let line, let path):
                HStack(spacing: 8) {
                    automationText(Ids.snapshotRestoreResult, line)
                        .font(.caption)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer()
                    if let path {
                        Button(L.backups.revealInFinder) {
                            let url = URL(fileURLWithPath: path)
                            NSWorkspace.shared.activateFileViewerSelecting([url])
                        }
                        .controlSize(.small)
                    }
                }
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }

    // MARK: - Actions

    private func chooseDestination() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = L.backups.restore
        panel.message = L.backups.restoreDirectoryPanelMessage

        guard panel.runModal() == .OK, let url = panel.url else { return }
        destinationText = url.path
    }

    private func startRestore() async {
        guard let client else { return }
        // The shared resolver (`ui/backups.md` § Where logic lives → *Whole-snapshot
        // restore*): an absolute path, or `~/` naming the home directory; `nil`
        // for anything else.
        guard let destination = resolveSnapshotRestoreDestination(typed: destinationText) else {
            restoreState = .finished(
                line: L.backups.snapshotRestoreDestinationNotAbsolute(path: destinationText),
                path: nil)
            return
        }

        restoreState = .restoring(path: destination)
        do {
            let summary = try await client.restoreSnapshot(
                snapshotId: snapshotId,
                outputDir: destination
            )
            restoreState = .finished(
                line: SnapshotRestoreText.line(summary, path: destination), path: destination)
        } catch {
            restoreState = .finished(
                line: L.backups.snapshotRestoreFailed(message: error.localizedDescription),
                path: destination)
        }
    }
}
