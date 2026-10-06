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
    let threadId: ThreadId

    var body: some View {
        Group {
            if let detail = vm.detail(threadId) {
                // The list's page-error banner sits under this push, so the
                // pushed screen carries it (a refused add is stamped while the
                // thread is open).
                ThreadDetailView(detail: detail, showsPageError: true)
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
