import SwiftUI
import FaunaKit

/// Full restore of a folder snapshot into a user-chosen folder, via the shared
/// **client-side walk** (`backup-restore.md` § 4 + § Restoring Files). Each file's
/// manifest + chunks are fetched, decrypted under the owner `BackupKey`, verified,
/// and written locally by the in-process sync engine host — so this works on
/// **sealed** snapshots, which the retired server-side ZIP route refuses.
struct MacRestoreView: View {
    let snapshotId: Int
    @Environment(FaunaClient.self) private var client: FaunaClient?

    @State private var destinationURL: URL?
    @State private var restoreState: RestoreState = .idle

    enum RestoreState: Equatable {
        case idle
        case restoring
        case done(filesRestored: UInt64)
        case error(String)
    }

    var body: some View {
        Form {
            Section(L.backups.chooseDestination) {
                HStack {
                    if let url = destinationURL {
                        Image(systemName: "folder.fill")
                            .foregroundStyle(.blue)
                        Text(url.path)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    } else {
                        Text(L.backups.noDestination)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button(L.backups.chooseDestination) {
                        chooseDestination()
                    }
                    .controlSize(.small)
                }
            }

            Section(L.backups.restore) {
                switch restoreState {
                case .idle:
                    HStack {
                        Button(L.backups.startRestore) {
                            Task { await startRestore() }
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(destinationURL == nil || client?.syncHost == nil)

                        Spacer()

                        Text(L.backups.restoreIntoDirectoryDesc)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                case .restoring:
                    HStack {
                        ProgressView()
                            .controlSize(.small)
                        Text(L.backups.restoreProgressRunning)
                            .font(.headline)
                    }

                case .done(let filesRestored):
                    HStack(spacing: 8) {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(.green)
                            .font(.title2)
                        VStack(alignment: .leading) {
                            Text(L.backups.restoreComplete)
                                .font(.headline)
                            Text(L.backups.restoreFilesWritten(
                                count: String(filesRestored),
                                path: destinationURL?.path ?? ""
                            ))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        }
                    }

                    Button(L.backups.revealInFinder) {
                        if let url = destinationURL {
                            NSWorkspace.shared.activateFileViewerSelecting([url])
                        }
                    }
                    .controlSize(.small)

                case .error(let message):
                    VStack(alignment: .leading, spacing: 8) {
                        HStack(spacing: 8) {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .foregroundStyle(.red)
                                .font(.title2)
                            Text(L.backups.restoreFailed)
                                .font(.headline)
                        }
                        Text(message)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }

                    Button(L.common.retry) {
                        restoreState = .idle
                    }
                    .controlSize(.small)
                }
            }

            Section {
                VStack(alignment: .leading, spacing: 4) {
                    Text(L.backups.aboutRestore)
                        .font(.caption)
                        .fontWeight(.medium)
                    Text(L.backups.restoreAboutDetail(id: String(snapshotId)))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
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
        destinationURL = url
    }

    private func startRestore() async {
        guard let client, let destination = destinationURL else { return }

        restoreState = .restoring
        do {
            let summary = try await client.restoreSnapshot(
                snapshotId: snapshotId,
                outputDir: destination.path
            )
            restoreState = .done(filesRestored: summary.filesRestored)
        } catch {
            restoreState = .error(error.localizedDescription)
        }
    }
}
