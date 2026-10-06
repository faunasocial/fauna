import SwiftUI
import FaunaKit

struct SearchResultsView: View {
    let vm: SearchVM

    @Environment(AppState.self) private var appState
    @Environment(FeedVM.self) private var feedVM
    @Environment(ConversationsVM.self) private var conversationsVM

    /// Route a `search-result-item` activation by its typed `SearchNav`
    /// target — the three-part contract (`ui/search.md` § Where logic lives →
    /// Result navigation (deep link)). Mirrors macOS
    /// `SearchResultsView` and linux's `open_search_result`; switches the
    /// destination tab FIRST (synchronously) in every arm, then lets any
    /// resolve round trip (Post/Contact/File) fill in behind it.
    ///
    /// `vm.cancel()` + dismiss the search bar FIRST — `ContentView.body`'s
    /// `.overlay` shows `SearchResultsView` whenever `searchVM.hasSearched`,
    /// covering the tab content underneath regardless of `selectedTab`;
    /// without dismissing search first, switching tabs changes what's
    /// underneath but the overlay still hides it. Mirrors the existing
    /// `search-cancel-button` action (`cancelSearch()`).
    private func openResult(_ result: SearchResultRow) {
        guard let nav = result.navigation else { return }
        vm.cancel()
        appState.showSearchBar = false
        switch nav {
        case .post(let postId):
            appState.selectedTab = "feed"
            feedVM.pendingPostOpen = postId
        case .mail(let threadId, let messageId):
            // The message-highlight half now rides selectThreadAndMessage —
            // the paint/scroll is DmMessageBubble/ThreadDetailView's job
            // (`ui/conversations.md` § The selected message).
            appState.selectedTab = "conversations"
            conversationsVM.selectThreadAndMessage(threadId, messageId)
        case .draft(let threadId):
            appState.selectedTab = "conversations"
            if let threadId {
                conversationsVM.selectThread(threadId)
            } else {
                conversationsVM.startNewConversation()
            }
        case .contact(let uidHash):
            appState.selectedTab = "contacts"
            appState.pendingContactUidHash = uidHash
        case .file(let folderId, let pathHash):
            MediaFileLocate.shared.stage(folderId: folderId, pathHash: pathHash)
            appState.moreSelectedView = "media"
            appState.selectedTab = "more"
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            // `search-type-filter` — one shared mapping applies the token to
            // BOTH search backends (search.md § State & data shape); re-fires
            // the LIVE buffer under the new token (a no-op while the buffer is
            // empty, mirroring tui/linux/web — the picker then redraws back to
            // the still-committed value on the next snapshot read).
            Picker("Type", selection: Binding(
                get: { vm.typeFilter },
                set: { token in Task { await vm.search(newTypeFilter: token) } }
            )) {
                ForEach(FaunaFFISwift.searchTypeFilterOptions(), id: \.self) { token in
                    Text(renderLocalizedText(FaunaFFISwift.searchTypeFilterLabel(token: token))).tag(token)
                }
            }
            .pickerStyle(.segmented)
            .accessibilityIdentifier(Ids.searchTypeFilter)
            .automationSelect(
                Ids.searchTypeFilter,
                value: { vm.typeFilter }
            ) { token in Task { await vm.search(newTypeFilter: token) } }
            .padding(.horizontal)
            .padding(.vertical, 8)

            SearchResultsListView(vm: vm, onTapResult: openResult)
        }
        .accessibilityIdentifier(Ids.searchResultsView)
        // Presence anchor (mirrors macOS). Env-gated no-op.
        .automationValue(Ids.searchResultsView, text: { "" })
    }
}
