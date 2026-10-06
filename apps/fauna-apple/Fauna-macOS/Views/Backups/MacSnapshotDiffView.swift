import SwiftUI
import FaunaKit

struct MacSnapshotDiffView: View {
    var vm: BackupsMachineVM
    let snapshotId: Int64
    @State private var comparisonId: Int64?

    /// The other rows of the SELECTED set, off the machine's list — the same
    /// rows the timeline renders, so the picker can never offer a snapshot the
    /// page no longer lists.
    private var otherSnapshots: [SnapshotRow] {
        (vm.snapshot?.snapshots ?? []).filter { $0.id != snapshotId }
    }

    var body: some View {
        VStack(spacing: 0) {
            comparisonPicker
            Divider()
            diffContent
        }
    }

    // MARK: - Picker

    private var comparisonPicker: some View {
        HStack {
            Picker(L.backups.diff.compareWith, selection: $comparisonId) {
                Text(L.backups.diff.selectSnapshot).tag(nil as Int64?)
                ForEach(otherSnapshots, id: \.id) { snap in
                    Text("#\(snap.id) - \(ValueFormat.absoluteDate(epochMs: snap.createdAt * 1000, withTime: true))")
                        .tag(snap.id as Int64?)
                }
            }
            .frame(maxWidth: 350)

            Button(L.backups.diff.compare) {
                guard let comparisonId else { return }
                Task {
                    await vm.loadDiff(a: Int(snapshotId), b: Int(comparisonId))
                }
            }
            .disabled(comparisonId == nil)
        }
        .padding()
    }

    // MARK: - Diff Content

    @ViewBuilder
    private var diffContent: some View {
        if let diff = vm.diffResult {
            VStack(spacing: 0) {
                summaryBar(diff: diff)
                Divider()
                diffList(diff: diff)
            }
        } else {
            ContentUnavailableView(L.backups.diff.noComparison,
                systemImage: "arrow.left.arrow.right",
                description: Text(L.backups.diff.noComparisonDesc))
        }
    }

    private func summaryBar(diff: SnapshotDiffResponse) -> some View {
        HStack(spacing: 16) {
            Label(L.backups.diff.addedCount(count: String(diff.summary.addedCount)), systemImage: "plus.circle.fill")
                .foregroundStyle(.green)
            Label(L.backups.diff.removedCount(count: String(diff.summary.removedCount)), systemImage: "minus.circle.fill")
                .foregroundStyle(.red)
            Label(L.backups.diff.modifiedCount(count: String(diff.summary.modifiedCount)), systemImage: "pencil.circle.fill")
                .foregroundStyle(.orange)
            Spacer()
            Text(L.backups.diff.net(size: ValueFormat.byteSize(diff.summary.netBytes)))
                .foregroundStyle(.secondary)
        }
        .font(.caption)
        .padding(.horizontal)
        .padding(.vertical, 8)
        .background(.bar)
    }

    private func diffList(diff: SnapshotDiffResponse) -> some View {
        List {
            if !diff.added.isEmpty {
                Section {
                    ForEach(diff.added) { entry in
                        HStack {
                            Image(systemName: "plus.circle.fill")
                                .foregroundStyle(.green)
                            Text(entry.path)
                                .lineLimit(1)
                            Spacer()
                            Text(ValueFormat.byteSize(entry.sizeBytes))
                                .foregroundStyle(.secondary)
                        }
                    }
                } header: {
                    Text(L.backups.diff.added)
                }
            }

            if !diff.removed.isEmpty {
                Section {
                    ForEach(diff.removed) { entry in
                        HStack {
                            Image(systemName: "minus.circle.fill")
                                .foregroundStyle(.red)
                            Text(entry.path)
                                .lineLimit(1)
                            Spacer()
                            Text(ValueFormat.byteSize(entry.sizeBytes))
                                .foregroundStyle(.secondary)
                        }
                    }
                } header: {
                    Text(L.backups.diff.removed)
                }
            }

            if !diff.modified.isEmpty {
                Section {
                    ForEach(diff.modified) { entry in
                        HStack {
                            Image(systemName: "pencil.circle.fill")
                                .foregroundStyle(.orange)
                            Text(entry.path)
                                .lineLimit(1)
                            Spacer()
                            Text("\(ValueFormat.byteSize(entry.oldSize)) -> \(ValueFormat.byteSize(entry.newSize))")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                } header: {
                    Text(L.backups.diff.modified)
                }
            }
        }
    }
}
