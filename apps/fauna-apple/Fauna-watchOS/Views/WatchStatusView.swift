import SwiftUI
import FaunaKit

struct WatchStatusView: View {
    let appState: WatchAppState
    var apiClient: APIClient?

    @State private var quota: QuotaResponse?

    var body: some View {
        List {
            if let handle = appState.handle {
                Section {
                    Text(handle)
                        .font(.headline)
                }
            }

            if let actorId = appState.actorId {
                Section("Identity") {
                    Text(shortId(hex: actorId))
                        .font(.caption2.monospaced())
                }
            }

            if let nodeUrl = appState.nodeUrl {
                Section("Node") {
                    Text(nodeUrl)
                        .font(.caption2)
                }
            }

            Section("Connection") {
                HStack {
                    Circle()
                        .fill(appState.isConnected ? .green : .red)
                        .frame(width: 8, height: 8)
                    Text(appState.isConnected ? "Connected" : "Disconnected")
                        .font(.caption)
                }
            }

            if let quota {
                Section("Storage") {
                    ProgressView(
                        value: Double(quota.storage.usedBytes),
                        total: Double(quota.storage.maxBytes)
                    )
                    Text("\(ValueFormat.byteSize(quota.storage.usedBytes)) of \(ValueFormat.byteSize(quota.storage.maxBytes))")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .task {
            if let api = apiClient {
                quota = try? await api.fetchQuota()
            }
        }
    }
}
