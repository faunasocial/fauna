import Testing
@testable import FaunaKit

// Loading is not empty (`docs/goal/ui/README.md` § List pages: loading is not
// empty; `docs/goal/ui/conversations.md` → *Loading is not empty*): the
// conversations list paints "No conversations yet." only once the shared
// `ConversationsSnapshot.loaded` bit is true and the list is still empty.
// Mirrors tui's `the_empty_state_paints_only_once_the_room_list_has_loaded`.
// Reds under: painting off `threads.isEmpty` alone (the unloaded half);
// never painting it (the loaded half).

@MainActor
@Test func theEmptyStatePaintsOnlyOnceTheRoomListHasLoaded() {
    let vm = ConversationsVM()
    var snapshot = vm.snapshot
    #expect(snapshot.threads.isEmpty, "sanity: a fresh manager holds no threads")
    #expect(!snapshot.loaded, "sanity: a fresh manager has run no receive sweep")

    #expect(!vm.showsEmptyState, "an unread room list is loading, never 'no conversations'")
    #expect(!ConversationsVM.showsEmptyState(snapshot))

    snapshot.loaded = true
    #expect(ConversationsVM.showsEmptyState(snapshot), "a loaded, empty room list paints its empty state")

    let alice = tryParseTypedAddress(raw: "alice@example.com")!
    _ = vm.manager.createMlsGroup(participants: [alice])
    var withThread = vm.snapshot
    withThread.loaded = true
    #expect(!withThread.threads.isEmpty, "sanity: the created thread is listed")
    #expect(!ConversationsVM.showsEmptyState(withThread), "a loaded list with a thread paints no empty state")
}
