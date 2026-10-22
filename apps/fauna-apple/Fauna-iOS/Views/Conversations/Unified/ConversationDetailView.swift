import SwiftUI
import FaunaKit

/// Thread detail screen (iOS) — the mobile-collapsed counterpart of the detail
/// pane in macOS's two-pane `MacConversationsView`. A pushed `NavigationStack`
/// screen (the list's `NavigationLink` pushes it); the body is the shared
/// `FaunaKit.ThreadDetailView` (thread header with participant chips +
/// add/rename, message bubbles with subject dividers, the inline
/// `dm-compose-bar`, and the rename / add-participant `.sheet` overlays). This
/// wrapper just registers the open thread with the shared `ConversationsVM` so
/// the selection round-trips and the unread count clears, and gives the screen
/// its nav-bar title.
struct ConversationDetailView: View {
    @Environment(ConversationsVM.self) private var vm
    #if !FAUNA_KIDS
    // The feed is excised from the kids build, which has no page for a room-search post hit to open.
    @Environment(FeedVM.self) private var feedVM
    @Environment(AppState.self) private var appState
    #endif
    let threadId: ThreadId

    var body: some View {
        Group {
            if let detail = vm.detail(threadId) {
                // The list's page-error banner sits under this push, so the
                // pushed screen carries it (a refused add is stamped while the
                // thread is open).
                #if FAUNA_KIDS
                ThreadDetailView(detail: detail, showsPageError: true)
                #else
                ThreadDetailView(detail: detail, showsPageError: true, onOpenRoomPost: { postId, home in
                    // A room-search post hit opens on the feed tab through the same
                    // pending-open door a Search hit uses, carrying the room's home
                    // so a foreign-homed post resolves through `resolve_post_from`.
                    appState.selectedTab = "feed"
                    feedVM.pendingPostOpenHome = home
                    feedVM.pendingPostOpen = postId
                })
                #endif
            } else {
                // Thread vanished out from under us (e.g. reset) — rare.
                ContentUnavailableView(
                    L.conversations.list.title,
                    systemImage: "bubble.left.and.bubble.right"
                )
            }
        }
        .navigationTitle(vm.detail(threadId)?.label ?? "")
        .navigationBarTitleDisplayMode(.inline)
        // Selecting the thread is what reads it (the shared manager decides
        // that); going back to the list must close it — a single-pane shell's
        // one duty (conversations.md § State & data shape → When a thread is read).
        .onAppear { vm.selectThread(threadId) }
        .onDisappear { vm.clearSelection() }
    }
}
