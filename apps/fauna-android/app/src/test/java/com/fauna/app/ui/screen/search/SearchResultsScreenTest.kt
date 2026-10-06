package com.fauna.app.ui.screen.search

import androidx.navigation.NavController
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.verify
import org.mockito.Mockito.verifyNoInteractions
import uniffi.fauna_client_search.SearchNav
import uniffi.fauna_conversations.ConversationsManager

/**
 * Pure-Kotlin unit tests for [openSearchResult] — the android twin of tui's
 * `open_result` (`apps/fauna-tui/src/search.rs`, the lead-app reference;
 * `docs/goal/ui/search.md` § User actions + § Where logic lives → *Result
 * navigation (deep link)*). No Compose/Robolectric needed: the function takes
 * plain mocked collaborators and returns a plain `() -> Unit`, so this runs as
 * a fast JVM test with no native library — the [com.fauna.app.core.PostDetailMappingTest]
 * style applied to a render-lift's routing logic.
 */
class SearchResultsScreenTest {

    private fun navController(): NavController = mock(NavController::class.java)
    private fun conversationsManager(): ConversationsManager = mock(ConversationsManager::class.java)

    @Test
    fun postNavigatesToTheFeedPostRouteByItsOwnId() {
        val nav = navController()
        val conv = conversationsManager()

        val onOpen = openSearchResult(SearchNav.Post(postId = "p1"), nav, conv)
        checkNotNull(onOpen)
        onOpen.invoke()

        // Post ids match the wire contract directly (content_id IS the post
        // id, ratified) — no id-space resolve, straight into the existing
        // feed/post/{postId} route PostDetailScreen already reads.
        verify(nav).navigate("feed/post/p1?source=")
        verifyNoInteractions(conv)
    }

    @Test
    fun postIdIsUrlEncodedIntoTheRoute() {
        val nav = navController()
        val conv = conversationsManager()

        val onOpen = openSearchResult(SearchNav.Post(postId = "a b/c"), nav, conv)
        checkNotNull(onOpen)
        onOpen.invoke()

        verify(nav).navigate("feed/post/a+b%2Fc?source=")
    }

    @Test
    fun draftWithAThreadSelectsThatThreadThenNavigatesToIt() {
        val nav = navController()
        val conv = conversationsManager()

        val onOpen = openSearchResult(SearchNav.Draft(threadId = "t1"), nav, conv)
        checkNotNull(onOpen)
        onOpen.invoke()

        // Exactly mirrors ConversationListScreen's onOpenThread: select on the
        // shared manager, then push the detail route.
        verify(conv).selectThread("t1")
        verify(nav).navigate("conversation/t1")
    }

    @Test
    fun threadlessDraftStartsTheNewThreadComposerInstead() {
        val nav = navController()
        val conv = conversationsManager()

        val onOpen = openSearchResult(SearchNav.Draft(threadId = null), nav, conv)
        checkNotNull(onOpen)
        onOpen.invoke()

        // thread_id == null is the single-slot new-thread compose — mirrors
        // ConversationListScreen's onNewConversation, never a raw selectThread(null).
        verify(conv).startNewConversation()
        verify(nav).navigate("conversation_compose")
    }

    @Test
    fun mailSelectsTheThreadAndMessageTogetherThenNavigatesToTheThread() {
        val nav = navController()
        val conv = conversationsManager()

        val onOpen = openSearchResult(SearchNav.Mail(threadId = "t2", messageId = "m1"), nav, conv)
        checkNotNull(onOpen)
        onOpen.invoke()

        // The whole of SearchNav::Mail's contract in one write — thread AND
        // message — via the already-exported selectThreadAndMessage, not the
        // thread-only selectThread a Draft/Post uses.
        verify(conv).selectThreadAndMessage("t2", "m1")
        verify(nav).navigate("conversation/t2")
    }

    @Test
    fun contactNavigatesToTheContactCardRouteByItsUidHash() {
        val nav = navController()
        val conv = conversationsManager()

        // uid_hash and card_id are different id spaces of the same width —
        // this must carry the RAW uid_hash into the deep-link route (never a
        // cast into card_id's space); ContactsVM resolves it to a card_id via
        // the shared locate on entry.
        val onOpen = openSearchResult(SearchNav.Contact(uidHash = "deadbeef"), nav, conv)
        checkNotNull(onOpen)
        onOpen.invoke()

        verify(nav).navigate("contacts/card/deadbeef")
        verifyNoInteractions(conv)
    }

    @Test
    fun contactUidHashIsUrlEncodedIntoTheRoute() {
        val nav = navController()
        val conv = conversationsManager()

        val onOpen = openSearchResult(SearchNav.Contact(uidHash = "a b/c"), nav, conv)
        checkNotNull(onOpen)
        onOpen.invoke()

        verify(nav).navigate("contacts/card/a+b%2Fc")
    }

    @Test
    fun fileNavigatesToTheMediaFileRouteByItsFolderIdAndPathHash() {
        val nav = navController()
        val conv = conversationsManager()

        // folderId/pathHash name a durable (folder, path) pair — MediaVM
        // resolves it to a MediaItemSummary via the shared MediaMachine
        // locate on entry, never a client-side scan of a rendered field.
        val onOpen = openSearchResult(SearchNav.File(folderId = 1L, pathHash = "ph1"), nav, conv)
        checkNotNull(onOpen)
        onOpen.invoke()

        verify(nav).navigate("media/file/1/ph1")
        verifyNoInteractions(conv)
    }

    @Test
    fun filePathHashIsUrlEncodedIntoTheRoute() {
        val nav = navController()
        val conv = conversationsManager()

        val onOpen = openSearchResult(SearchNav.File(folderId = 42L, pathHash = "a b/c"), nav, conv)
        checkNotNull(onOpen)
        onOpen.invoke()

        verify(nav).navigate("media/file/42/a+b%2Fc")
    }
}
