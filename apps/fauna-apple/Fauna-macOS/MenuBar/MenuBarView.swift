import SwiftUI
import FaunaKit

struct MenuBarView: View {
    let controller: MenuBarController

    @State private var folders: [FolderResponse] = []
    @State private var loadedFolders = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header
            HStack {
                Text("Fauna")
                    .font(.headline)
                Spacer()
                if let client = controller.client {
                    Circle()
                        .fill(client.connectionState == .connected ? .green : .red)
                        .frame(width: 8, height: 8)
                    Text(client.connectionState == .connected ? L.common.connected : L.common.disconnected)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding()

            Divider()

            // Unread messages
            if controller.unreadCount > 0 {
                HStack {
                    Image(systemName: "envelope.badge")
                    Text(L.common.unreadCount(count: "\(controller.unreadCount)"))
                    Spacer()
                }
                .padding(.horizontal)
                .padding(.vertical, 8)

                Divider()
            }

            // Sync status
            syncStatusSection

            Divider()

            // Actions
            VStack(spacing: 0) {
                MenuBarButton(title: "\(L.conversations.compose.title)…", shortcut: "N") {
                    controller.openMainWindow()
                }

                MenuBarButton(title: "\(L.common.`open`) Fauna", shortcut: "O") {
                    controller.openMainWindow()
                }
            }

            Divider()

            MenuBarButton(title: "\(L.common.quit) Fauna", shortcut: "Q") {
                NSApp.terminate(nil)
            }
        }
        .frame(width: 320)
        .task {
            await loadFolders()
        }
    }

    // MARK: - Sync Status

    private var syncStatusSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: syncStatusIcon)
                    .foregroundStyle(syncStatusColor)
                    .font(.caption)
                Text(L.status.sync.menuStatus(status: syncStatusLabel))
                    .font(.caption)
                    .fontWeight(.medium)
                Spacer()
            }

            if !folders.isEmpty {
                ForEach(folders) { folder in
                    HStack(spacing: 4) {
                        // One folder glyph, no type (`ui/folders.md` § Modes — a folder has no type).
                        Image(systemName: "folder")
                            .font(.system(size: 9))
                            .foregroundStyle(.secondary)
                        Text(folder.name)
                            .font(.caption2)
                        Spacer()
                        Text(folderStatusLabel(folder))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
    }

    // MARK: - Helpers

    /// Localized display label for the raw `controller.syncStatus` state string.
    /// The raw string stays the state key (compared as `== "Active"` etc.); only the
    /// user-facing label is localized here.
    private var syncStatusLabel: String {
        switch controller.syncStatus {
        case "Active": L.common.active
        case "Stopped": L.status.sync.stopped
        default: L.common.unknown
        }
    }

    private var syncStatusIcon: String {
        switch controller.syncStatus {
        case "Active": "arrow.triangle.2.circlepath"
        case "Stopped": "stop.circle"
        default: "questionmark.circle"
        }
    }

    private var syncStatusColor: Color {
        switch controller.syncStatus {
        case "Active": .green
        case "Stopped": .red
        default: .secondary
        }
    }

    private func folderStatusLabel(_ folder: FolderResponse) -> String {
        if let lastAt = folder.cachedLastSnapshotAt {
            "last: \(lastAt)"
        } else {
            "idle"
        }
    }

    private func loadFolders() async {
        guard !loadedFolders, let api = controller.client?.api else { return }
        loadedFolders = true
        do {
            folders = try await api.listFolders()
        } catch {
            // Silently fail in menu bar — not critical
        }
    }
}

private struct MenuBarButton: View {
    let title: String
    let shortcut: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack {
                Text(title)
                Spacer()
                Text("⌘\(shortcut)")
                    .foregroundStyle(.secondary)
                    .font(.caption)
            }
            .padding(.horizontal)
            .padding(.vertical, 6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}
