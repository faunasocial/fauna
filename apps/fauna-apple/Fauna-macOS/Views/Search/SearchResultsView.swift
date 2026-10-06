import SwiftUI
import FaunaKit

struct SearchResultsView: View {
    let vm: SearchVM

    @Environment(MacAppState.self) private var appState
    @Environment(FeedVM.self) private var feedVM
    @Environment(ConversationsVM.self) private var conversationsVM

    /// Route a `search-result-item` activation by its typed `SearchNav`
    /// target — the three-part contract (`ui/search.md` § Where logic lives →
    /// Result navigation (deep link)). Mirrors
    /// linux's `open_search_result` (`views/search.rs`); switches the
    /// destination page FIRST (synchronously) in every arm, then lets any
    /// resolve round trip (Post/Contact/File) fill in behind it.
    ///
    /// `vm.cancel()` FIRST — `ContentView.body`'s detail pane shows
    /// `SearchResultsView` whenever `searchVM.hasSearched`, taking priority
    /// over `appState.selectedSidebar` entirely; without dismissing search
    /// first, switching the sidebar changes what WOULD render underneath
    /// but never actually shows it (measured: the sidebar switch is a no-op
    /// on screen, and no destination page mounts to consume the pending
    /// deep-link state). Mirrors the existing `search-cancel-button` action.
    private func openResult(_ result: SearchResultRow) {
        guard let nav = result.navigation else { return }
        vm.cancel()
        switch nav {
        case .post(let postId):
            appState.selectedSidebar = .feed
            feedVM.pendingPostOpen = postId
        case .mail(let threadId, let messageId):
            // The message-highlight half now rides selectThreadAndMessage —
            // the paint/scroll is DmMessageBubble/ThreadDetailView's job
            // (`ui/conversations.md` § The selected message).
            appState.selectedSidebar = .conversations
            conversationsVM.selectThreadAndMessage(threadId, messageId)
        case .draft(let threadId):
            appState.selectedSidebar = .conversations
            if let threadId {
                conversationsVM.selectThread(threadId)
            } else {
                conversationsVM.startNewConversation()
            }
        case .contact(let uidHash):
            appState.selectedSidebar = .contacts
            appState.pendingContactUidHash = uidHash
        case .file(let folderId, let pathHash):
            appState.selectedSidebar = .media
            MediaFileLocate.shared.stage(folderId: folderId, pathHash: pathHash)
        }
    }

    var body: some View {
        VStack(alignment: .leading) {
            SearchResultsListView(vm: vm, onTapResult: openResult)
        }
        .accessibilityIdentifier(Ids.searchResultsView)
        .automationValue(Ids.searchResultsView, text: { "" })
        .pageTitle(L.searchPage.title)
    }
}
