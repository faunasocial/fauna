package com.fauna.app.testing

import com.fauna.app.core.AppState
import com.fauna.app.core.NotificationHelper
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.conversations.ConversationsManagerHost
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.TypedAddress

/**
 * `compose.file`[attachment-button] — the harness door the cross-app
 * conversation-attachment witnesses reach android's attach seam through
 * (`drivers/http_bridge.py::set_input_files` sends `{"file", "target"}` on every
 * bridge driver, android's included). The OS `GetContent()` picker cannot be
 * driven by an in-process agent, so the arm stands in for the picker's result
 * callback and must call exactly what that callback calls: `addAttachment` on
 * the open thread (`ConversationDetailScreen`), `addNewThreadAttachment` on the
 * new-thread composer (`NewThreadComposeScreen`).
 *
 * Which composer is "open" is read off the SHARED manager's snapshot — the same
 * precedence linux's agent (`main.rs`) and apple's
 * `ConversationsVM.attachComposerFile` use: an active `newThreadCompose` first,
 * else `selectedThreadId` (which `ConversationListScreen.onOpenThread` sets
 * before pushing `conversation/{threadId}`). These pins drive a REAL
 * [ConversationsManagerHost] (UniFFI over host JNA, `FaunaRobolectricTestRunner`),
 * so each asserts on the manager's own snapshot, never on the arm's say-so.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentConversationAttachTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private fun storage() = mock(SecureStorage::class.java)

    /** `messages.error` off the state protocol — what `error_text()` reads on
     *  android (see [TestAgentRefusalSurfaceTest]'s twin for why `null`). */
    private fun errorFromStateProtocol(appState: AppState): String? {
        val state = TestAgent.serializeState(appState, storage(), null, appState.messages)
        if (state.isNull("messages")) return null
        val messages = state.getJSONObject("messages")
        return if (messages.isNull("error")) null else messages.getString("error")
    }

    /** The exact command `set_input_files("attachment-button", path)` sends. */
    private fun attachPatch(path: String, appState: AppState) = runBlocking {
        val cmd = JSONObject().apply {
            put("action", "patch")
            put(
                "state",
                JSONObject().put(
                    "compose",
                    JSONObject().put("file", path).put("target", "attachment-button"),
                ),
            )
        }
        TestAgent.processCommand("patch", cmd, appState, storage(), null)
    }

    private fun wireRealManagerHost(): ConversationsManagerHost =
        ConversationsManagerHost(NotificationHelper(RuntimeEnvironment.getApplication()))
            .also { TestAgent.setConversationsManagerHostForTest(it) }

    @After
    fun unwireManagerHost() {
        // `TestAgent` is a process-global `object` — never leak a host.
        TestAgent.setConversationsManagerHostForTest(null)
    }

    private fun stagedFile(name: String, body: String = "attached for real"): String =
        tmp.newFile(name).apply { writeText(body) }.absolutePath

    /** A thread the list tap opened: create one, then `selectThread` it the way
     *  `ConversationListScreen.onOpenThread` does before navigating. */
    private fun openThread(host: ConversationsManagerHost): String {
        val before = host.manager.snapshot().threads.map { it.threadId }.toSet()
        host.manager.createMlsGroup(listOf(TypedAddress.Fauna("bob@self-nest.test", ByteArray(32))))
        val tid = host.manager.snapshot().threads.map { it.threadId }.single { it !in before }
        host.manager.selectThread(tid)
        return tid
    }

    /** The load-bearing case: the arm MOVES the open thread's compose draft. */
    @Test
    fun anAttachOnAnOpenThreadStagesOntoThatThreadsDraft() {
        val host = wireRealManagerHost()
        val tid = openThread(host)
        val appState = AppState()

        attachPatch(stagedFile("note.txt"), appState)

        assertNull("an honoured attach writes no refusal", errorFromStateProtocol(appState))
        val staged = host.manager.threadDetail(tid)!!.compose.attachments
        assertEquals("exactly one attachment staged on the open thread", 1, staged.size)
        assertEquals("note.txt", staged.single().filename)
        assertEquals(
            "the MIME comes from the shared catalog every native app resolves through",
            "text/plain",
            staged.single().mimeType,
        )
        assertEquals("attached for real".length.toULong(), staged.single().sizeBytes)
    }

    /** The new-thread composer outranks a stale selection — the precedence both
     *  linux and apple use, and the one a user standing on the new-thread screen
     *  after having opened a thread earlier actually needs. */
    @Test
    fun anActiveNewThreadComposerOutranksTheSelectedThread() {
        val host = wireRealManagerHost()
        val tid = openThread(host)
        host.manager.startNewConversation()
        val appState = AppState()

        attachPatch(stagedFile("draft.txt"), appState)

        assertNull("an honoured attach writes no refusal", errorFromStateProtocol(appState))
        val newThread = host.manager.snapshot().newThreadCompose
        assertNotNull("the new-thread composer is still open", newThread)
        assertEquals(
            "the attach must land on the new-thread draft",
            listOf("draft.txt"),
            newThread!!.attachments.map { it.filename },
        )
        assertTrue(
            "and must NOT land on the previously selected thread",
            host.manager.threadDetail(tid)!!.compose.attachments.isEmpty(),
        )
    }

    /** Neither composer open → a named refusal, never a silent no-op (e2e rule 11). */
    @Test
    fun anAttachWithNoOpenComposerIsRefusedByName() {
        wireRealManagerHost()
        val appState = AppState()

        attachPatch(stagedFile("orphan.txt"), appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull("an attach with nowhere to go must be loud", shown)
        assertTrue(
            "the reason must say no conversation composer is open: $shown",
            shown!!.contains("no conversation composer is open"),
        )
    }

    @Test
    fun anUnreadablePathIsRefusedAndStagesNothing() {
        val host = wireRealManagerHost()
        val tid = openThread(host)
        val appState = AppState()
        val missing = tmp.root.resolve("does-not-exist.png").absolutePath

        attachPatch(missing, appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)
        assertTrue("the reason must name the path: $shown", shown!!.contains(missing))
        assertTrue(
            "a refused attach must leave the draft untouched",
            host.manager.threadDetail(tid)!!.compose.attachments.isEmpty(),
        )
    }

    @Test
    fun anAttachBeforeTheHostIsWiredNamesTheHost() {
        val appState = AppState()

        attachPatch(stagedFile("early.txt"), appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)
        assertTrue(
            "the reason must name the unwired host: $shown",
            shown!!.contains("ConversationsManagerHost"),
        )
    }
}
