import SwiftUI
import FaunaKit

struct QuickSwitcherView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(MacAppState.self) private var appState
    @Environment(ConversationsVM.self) private var conversationsVM: ConversationsVM?
    @Environment(FaunaClient.self) private var client: FaunaClient?

    /// File search reads the same cross-set media list the Media page renders
    /// (`fauna.media.list` through the shared `MediaMachine`). It used to read a
    /// SwiftData `SyncFile` mirror that the bespoke Swift engine maintained; that
    /// mirror retired with the engine (the shared engine's per-set `SyncDb` is the
    /// only per-file store now), and the control plane is the right source for a
    /// *search* anyway — it spans every readable set, not just folders this device
    /// happens to sync.
    @State private var mediaVM = MediaMachineVM()

    @State private var query = ""
    @State private var selectedIndex = 0
    @State private var debouncedQuery = ""
    @FocusState private var isSearchFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            // Search field
            HStack {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(.secondary)
                TextField(L.navigation.quickSwitcherPlaceholder, text: $query)
                    .textFieldStyle(.plain)
                    .focused($isSearchFocused)
                    .onSubmit { openSelected() }
                    .onChange(of: query) { _, _ in
                        selectedIndex = 0
                    }
            }
            .padding(12)

            Divider()

            if results.isEmpty && !debouncedQuery.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "magnifyingglass")
                        .font(.title2)
                        .foregroundStyle(.tertiary)
                    Text(L.navigation.noMatches)
                        .foregroundStyle(.secondary)
                        .font(.subheadline)
                }
                .frame(maxWidth: .infinity, maxHeight: 120)
            } else if !results.isEmpty {
                // Results
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            ForEach(Array(results.enumerated()), id: \.offset) { index, result in
                                QuickSwitcherRow(result: result, isSelected: index == selectedIndex)
                                    .id(index)
                                    .onTapGesture {
                                        selectedIndex = index
                                        openSelected()
                                    }
                            }
                        }
                    }
                    .frame(maxHeight: 300)
                    .onChange(of: selectedIndex) { _, newIndex in
                        proxy.scrollTo(newIndex, anchor: .center)
                    }
                }
            } else {
                // Empty state — show sidebar shortcuts
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(sidebarShortcuts.enumerated()), id: \.offset) { index, item in
                        HStack {
                            Image(systemName: item.systemImage)
                                .frame(width: 20)
                                .foregroundStyle(.secondary)
                            Text(item.label)
                            Spacer()
                            Text("\u{2318}\(index + 1)")
                                .font(.caption)
                                .foregroundStyle(.tertiary)
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 6)
                        .onTapGesture {
                            appState.selectedSidebar = item
                            dismiss()
                        }
                    }
                }
            }

            if !results.isEmpty {
                Divider()
                HStack(spacing: 16) {
                    Label(L.navigation.navigate, systemImage: "arrow.up.arrow.down")
                    Label(L.common.`open`, systemImage: "return")
                    Label(L.common.dismiss, systemImage: "escape")
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(8)
            }
        }
        .frame(width: 500)
        .background(.regularMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .shadow(radius: 20)
        .onAppear { isSearchFocused = true }
        .task {
            // Load the media list once per palette open, so a file added since the
            // last open is findable. `configure` is idempotent — it builds the
            // machine on first call and refreshes on later ones.
            if let client {
                await mediaVM.configure(
                    api: client.api, deviceId: client.deviceId,
                    predecessors: client.resolvedMediaPredecessors())
            }
        }
        .onKeyPress(.upArrow) {
            selectedIndex = max(0, selectedIndex - 1)
            return .handled
        }
        .onKeyPress(.downArrow) {
            selectedIndex = min(results.count - 1, selectedIndex + 1)
            return .handled
        }
        .onKeyPress(.escape) {
            dismiss()
            return .handled
        }
        .task(id: query) {
            // Debounce: wait 150ms before updating results
            try? await Task.sleep(for: .milliseconds(150))
            debouncedQuery = query
        }
    }

    private var sidebarShortcuts: [SidebarItem] {
        // Gate the Admin shortcut on `am-i-admin` — non-admins must not reach the
        // admin shell via the command palette either (admin.md § Navigation model:
        // "non-admins do not see admin entries"), matching the gated sidebar row.
        appState.isAdmin ? SidebarItem.allCases : SidebarItem.standardCases
    }

    // MARK: - Search

    private var results: [QuickSwitcherResult] {
        guard !debouncedQuery.isEmpty else { return [] }
        let q = debouncedQuery.lowercased()

        var scored: [(QuickSwitcherResult, Double)] = []

        // Conversation threads (unified page) — boost recent and exact matches.
        // Group threads (flavor: mlsGroup) live here too — no separate Groups
        // section since the conversations-page convergence.
        for thread in conversationsVM?.manager.snapshot().threads ?? [] {
            let score = matchScore(q, in: thread.label, thread.snippet)
            if score > 0 {
                let recencyBoost = recencyWeight(Date(timeIntervalSince1970: Double(thread.lastActivityMs) / 1000))
                scored.append((.thread(thread), score * recencyBoost))
            }
        }

        // Files — boost recent. `updatedAt` is unix **seconds** here (the media
        // wire's unit), not the millis the conversations thread above carries.
        for file in mediaVM.snapshot?.items ?? [] {
            let score = matchScore(q, in: file.path, file.folder)
            if score > 0 {
                let recencyBoost = recencyWeight(Date(timeIntervalSince1970: Double(file.updatedAt)))
                scored.append((.file(file), score * recencyBoost))
            }
        }

        // Sidebar sections — always available as fuzzy match (Admin gated on
        // `am-i-admin` via `sidebarShortcuts`).
        for item in sidebarShortcuts {
            if item.label.lowercased().contains(q) {
                scored.append((.sidebar(item), 0.5))
            }
        }

        // Sort by score descending, take top 15
        return scored.sorted { $0.1 > $1.1 }.prefix(15).map(\.0)
    }

    /// Score a query against multiple text fields. Returns 0 for no match.
    private func matchScore(_ query: String, in fields: String...) -> Double {
        var best = 0.0
        for field in fields {
            let lower = field.lowercased()
            if lower == query {
                best = max(best, 3.0) // Exact match
            } else if lower.hasPrefix(query) {
                best = max(best, 2.0) // Prefix match
            } else if lower.contains(query) {
                best = max(best, 1.0) // Substring match
            }
        }
        return best
    }

    /// Boost recent items: 2x for today, 1.5x within a week, 1.0x otherwise.
    private func recencyWeight(_ date: Date) -> Double {
        let age = Date.now.timeIntervalSince(date)
        if age < 86400 { return 2.0 }      // Today
        if age < 604800 { return 1.5 }     // This week
        if age < 2592000 { return 1.2 }    // This month
        return 1.0
    }

    private func openSelected() {
        guard selectedIndex < results.count else { return }
        let result = results[selectedIndex]

        switch result {
        case .thread(let thread):
            appState.selectedSidebar = .conversations
            conversationsVM?.manager.selectThread(id: thread.threadId)
        case .file:
            appState.selectedSidebar = .media
        case .sidebar(let item):
            appState.selectedSidebar = item
        }

        dismiss()
    }
}

enum QuickSwitcherResult {
    case thread(ThreadSummary)
    case file(MediaItemSummary)
    case sidebar(SidebarItem)
}

private struct QuickSwitcherRow: View {
    let result: QuickSwitcherResult
    let isSelected: Bool

    var body: some View {
        HStack {
            Image(systemName: icon)
                .frame(width: 20)
                .foregroundStyle(.secondary)
            VStack(alignment: .leading) {
                Text(title)
                    .lineLimit(1)
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer()
            Text(category)
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(isSelected ? Color.accentColor.opacity(0.1) : .clear)
    }

    private var icon: String {
        switch result {
        case .thread(let t): t.flavor == .mlsGroup ? "person.3" : "bubble.left.and.bubble.right"
        case .file: "doc"
        case .sidebar(let item): item.systemImage
        }
    }

    private var title: String {
        switch result {
        case .thread(let t): t.label
        case .file(let f): f.path
        case .sidebar(let item): item.label
        }
    }

    private var subtitle: String {
        switch result {
        case .thread(let t):
            t.flavor == .mlsGroup ? L.navigation.members(count: "\(t.participantCount)") : String(t.snippet.prefix(48))
        case .file(let f): f.folder
        case .sidebar: L.navigation.section
        }
    }

    private var category: String {
        switch result {
        case .thread(let t): t.flavor == .mlsGroup ? L.navigation.categoryGroup : L.navigation.categoryConversation
        case .file: L.navigation.categoryFile
        case .sidebar: L.navigation.categoryAction
        }
    }
}
