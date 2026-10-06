package com.fauna.app.testing

import android.util.Base64
import com.fauna.app.ui.components.documentAttachments
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.ConversationsManager
import uniffi.fauna_conversations.typedAddressDisplay

/**
 * The TestAgent's conversations witnesses over a REAL shared [ConversationsManager] with
 * the mock rail backends installed — the same manager the debug app drives under E2E.
 * `conversations_inject_inbound` hands its payload whole to the shared parser
 * (`inject_inbound_from_test_json`) instead of re-parsing it in Kotlin, and
 * `conversations_evict_attachment` drops an attachment's bytes through
 * `evict_thread_attachments_for_test`, refusing a no-op (convention 11)
 * (`docs/goal/ui/conversations.md` § Participants vs. reply recipients,
 * § Attachments → *Retention*).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentConversationsTest {

    private fun manager() = ConversationsManager().apply { installMockBackendsForTest() }

    private fun onlyThreadId(m: ConversationsManager): String {
        val threads = m.snapshot().threads
        assertEquals("the inject must land exactly one thread", 1, threads.size)
        return threads.single().threadId
    }

    /**
     * The whole `recipients` list reaches the thread — the hand parse this replaced read
     * only the singular `recipient` and dropped the list, so a mail thread could never
     * carry more than one other person to reply-all to.
     */
    @Test
    fun injectHandsTheWholePayloadToTheSharedParser() {
        val m = manager()
        val cmd = JSONObject()
            .put("id", "cmd_1")
            .put("action", "conversations_inject_inbound")
            .put("rail", "Smtp")
            .put("sender", "bob@example.com")
            .put("recipients", JSONArray().put("me@self-nest.test").put("carol@example.com"))
            .put("subject", "lunch")
            .put("body", "noon?")
            .put("message_id", "m-1")

        assertNull(TestAgent.injectInbound(m, cmd))

        val detail = m.threadDetail(onlyThreadId(m))
        assertNotNull(detail)
        val shown = detail!!.participants.map { typedAddressDisplay(it) }
        assertTrue("every recipient is a participant: $shown", shown.any { "carol@example.com" in it })
        assertTrue("the sender is a participant: $shown", shown.any { "bob@example.com" in it })
    }

    /**
     * Evicting a resident attachment drops its bytes (the residency peek flips) and acks;
     * evicting again finds nothing resident and is a FAILED command, never a silent ack.
     */
    @Test
    fun evictDropsTheBytesAndRefusesANoOp() {
        val m = manager()
        val png = byteArrayOf(0x89.toByte(), 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3)
        val inject = JSONObject()
            .put("rail", "FaunaMls")
            .put("sender", "petra@self-nest.test")
            .put("body", "a picture")
            .put("message_id", "m-pic")
            .put(
                "attachments",
                JSONArray().put(
                    JSONObject()
                        .put("filename", "pic.png")
                        .put("mime_type", "image/png")
                        .put("data_base64", Base64.encodeToString(png, Base64.NO_WRAP)),
                ),
            )
        assertNull(TestAgent.injectInbound(m, inject))
        val threadId = onlyThreadId(m)
        val hash = documentAttachments(m.threadDetail(threadId)!!.messages.single().document).single().blobHash
        assertTrue("the injected bytes start resident", m.attachmentResident(hash))

        val evict = JSONObject().put("thread_id", threadId).put("filename", "pic.png")
        assertNull(TestAgent.evictAttachment(m, evict))
        assertFalse("the evict dropped the bytes", m.attachmentResident(hash))

        val again = TestAgent.evictAttachment(m, evict)
        assertNotNull("a second evict finds nothing resident and must refuse", again)
        assertTrue(again!!, "pic.png" in again && threadId in again)
    }

    /** No compose field on screen is a refusal (`null`), never an empty run list. */
    @Test
    fun composeTextRunsWithNoFieldIsNull() {
        assertNull(TestAgent.composeTextRunsJson(null))
    }
}
