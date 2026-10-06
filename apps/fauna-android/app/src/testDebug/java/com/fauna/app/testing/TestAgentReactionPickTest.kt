package com.fauna.app.testing

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import com.fauna.app.ui.screen.conversations.MoreReactionPicker
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.ConversationsManager
import uniffi.fauna_conversations.Rail
import uniffi.fauna_conversations.TypedAddress

/**
 * `type_text`[dm-reaction-more-button] — the seam `react_with_custom_emoji`
 * reaches android's fuller reaction picker through. The picker is the native
 * emoji2 `EmojiPickerView`, whose cells carry no test id, so
 * `drivers/android.py` routes the typed emoji to the agent, which picks it
 * through the open sheet's own pick handler (`ui/conversations.md` § Reactions
 * & message delete → *Rendering / picker glue*; linux's `GtkEmojiChooser` arm
 * is the same shape).
 *
 * These pins compose the REAL [MoreReactionPicker] over a REAL
 * `ConversationsManager` (UniFFI over host JNA, `FaunaRobolectricTestRunner`)
 * and assert on the manager's own reaction fold — the data the
 * `dm-reaction-pill` renders — never on the arm's say-so.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentReactionPickTest {

    @get:Rule
    val compose = createComposeRule()

    private fun storage() = mock(SecureStorage::class.java)

    /** `messages.error` off the state protocol — what `error_text()` reads. */
    private fun errorFromStateProtocol(appState: AppState): String? {
        val state = TestAgent.serializeState(appState, storage(), null, appState.messages)
        if (state.isNull("messages")) return null
        val messages = state.getJSONObject("messages")
        return if (messages.isNull("error")) null else messages.getString("error")
    }

    /** The exact command `drivers/android.py`'s `type_text` sends for this id. */
    private fun typeAtMoreButton(text: String, appState: AppState) = runBlocking {
        val cmd = JSONObject().apply {
            put("action", "patch")
            put(
                "state",
                JSONObject().put(
                    "type_text",
                    JSONObject().put("target", "dm-reaction-more-button").put("text", text),
                ),
            )
        }
        TestAgent.processCommand("patch", cmd, appState, storage(), null)
    }

    /** A manager whose FaunaMls rail knows who the user is (reactions attribute
     *  themselves off it), holding one own message on a FaunaMls thread. */
    private class Fixture {
        val manager = ConversationsManager().apply {
            installMockBackendsForTest()
            installMockBackendKnowingSelfForTest(
                Rail.FAUNA_MLS,
                TypedAddress.Fauna("me@self-nest.test", ByteArray(32) { 1 }),
            )
        }
        val threadId: String = manager.createMlsGroup(
            listOf(TypedAddress.Fauna("bob@self-nest.test", ByteArray(32) { 2 })),
        )
        val messageId: String

        init {
            manager.setComposeBody(threadId, "react to me")
            runBlocking { manager.send(threadId) }
            messageId = manager.threadDetail(threadId)!!.messages.single { it.isOwn }.messageId
        }

        fun reactions() =
            manager.threadDetail(threadId)!!.messages.single { it.messageId == messageId }.reactions

        /** The screen's one toggle path, run to completion so a test reads a
         *  settled fold (production launches it on the screen's scope). */
        val toggle: (String, String) -> Unit = { msgId, emoji ->
            runBlocking { manager.toggleReaction(threadId, msgId, emoji) }
        }
    }

    /** Compose the sheet open on the fixture's message, as the ⋯ → more click leaves it. */
    private fun openSheet(f: Fixture) {
        compose.setContent {
            var target by remember { mutableStateOf<String?>(f.messageId) }
            MoreReactionPicker(
                targetMessageId = target,
                onToggleReaction = f.toggle,
                onClose = { target = null },
            )
        }
        compose.waitForIdle()
    }

    @After
    fun unregister() {
        // `TestAgent` is a process-global `object` — never leak a handler.
        TestAgent.moreReactionPick = null
    }

    /** The load-bearing case: a typed 🦊 with the sheet open lands as a pill on
     *  that message and closes the sheet, as a human pick does. */
    @Test
    fun aTypedEmojiWithTheSheetOpenReactsAndClosesTheSheet() {
        val f = Fixture()
        openSheet(f)
        assertNotNull("the open sheet registers its pick handler", TestAgent.moreReactionPick)
        val appState = AppState()

        typeAtMoreButton("🦊", appState)
        compose.waitForIdle()

        assertNull("an honoured pick writes no refusal", errorFromStateProtocol(appState))
        val pills = f.reactions()
        assertEquals("one pill, the picked emoji", listOf("🦊"), pills.map { it.emoji })
        assertTrue("attributed to me", pills.single().reactedByMe)
        assertNull("the pick closed the sheet", TestAgent.moreReactionPick)
    }

    /** No sheet open → a named refusal, never a silent no-op (convention 11):
     *  the click that opens the picker stays part of the journey. */
    @Test
    fun aTypedEmojiWithNoSheetOpenIsRefusedByName() {
        val f = Fixture()
        val appState = AppState()

        typeAtMoreButton("🦊", appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull("a pick with no open picker must be loud", shown)
        assertTrue("the reason must say the picker is not open: $shown", shown!!.contains("not open"))
        assertTrue("nothing reacted", f.reactions().isEmpty())
    }

    /** A second pick after the first closed the sheet is refused too — the
     *  handler is unregistered on dismiss, never left dangling. */
    @Test
    fun theSheetUnregistersWhenAPickClosesIt() {
        val f = Fixture()
        openSheet(f)
        typeAtMoreButton("🦊", AppState())
        compose.waitForIdle()
        val appState = AppState()

        typeAtMoreButton("🦊", appState)

        assertNotNull(errorFromStateProtocol(appState))
        assertEquals("still exactly the first pick", listOf("🦊"), f.reactions().map { it.emoji })
    }
}
