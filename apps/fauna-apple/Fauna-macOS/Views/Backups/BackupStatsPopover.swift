import SwiftUI
import FaunaKit

struct BackupStatsPopover: View {
    var vm: BackupsMachineVM
    let folder: String

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let stats = vm.stats {
                Form {
                    Section(L.backups.repoStats.title) {
                        LabeledContent(L.common.snapshots, value: "\(stats.snapshotCount ?? 0)")
                        LabeledContent(L.backups.repoStats.totalFiles, value: "\(stats.totalFiles ?? 0)")
                        LabeledContent(L.backups.repoStats.rawSize, value: ValueFormat.byteSize(stats.rawSizeBytes ?? 0))
                        LabeledContent(L.backups.repoStats.storedSize, value: ValueFormat.byteSize(stats.storedSizeBytes ?? 0))
                        LabeledContent(L.backups.repoStats.dedupRatio, value: String(format: "%.2fx", stats.dedupRatio ?? 1.0))
                        LabeledContent(L.backups.repoStats.storageBackend, value: stats.storageBackend ?? "local")
                    }

                    Section(L.backups.repoStats.encryption) {
                        LabeledContent(L.settings.encryptionPage.title, value: L.backups.repoStats.encryptionAlgo)
                        LabeledContent(L.backups.repoStats.compression, value: L.backups.repoStats.compression)
                        LabeledContent(L.backups.repoStats.chunkSizes, value: L.backups.repoStats.chunkSizes)
                    }
                }
                .formStyle(.grouped)
            } else if vm.busy {
                HStack {
                    Spacer()
                    ProgressView(L.backups.repoStats.loadingStats)
                    Spacer()
                }
                .padding(40)
            } else if let error = vm.errorMessage {
                VStack {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.title2)
                        .foregroundStyle(.secondary)
                    ErrorBanner(message: error)
                }
                .padding(40)
            }
        }
        .frame(minWidth: 320, minHeight: 200)
        .task {
            await vm.loadStats(folder: folder)
        }
    }
}
