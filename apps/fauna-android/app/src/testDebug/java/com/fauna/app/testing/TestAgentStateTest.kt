package com.fauna.app.testing

import com.fauna.app.data.db.Contact
import com.fauna.app.data.db.Knock
import com.fauna.app.data.db.SyncFile
import com.fauna.app.data.db.SyncFileState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Pure-mapping coverage for [TestAgent.contactsToJson] / [TestAgent.knocksToJson] /
 * [TestAgent.syncFilesToJson] — the `data.{contacts,knocks,sync.files}` e2e
 * state-protocol sections (`tests/e2e-unified/client_capabilities.py`
 * EXPECTED_FIELDS). android's `TestAgent.kt` previously serialized these as
 * always-empty placeholders despite Room already holding real rows
 * (`ContactDao`/`KnockDao`/`SyncFileDao`) — this locks the field-name contract
 * so a future rename doesn't silently drift from `EXPECTED_FIELDS` or linux's
 * `main.rs` shape. Robolectric-run: plain JVM unit tests hit `org.json`'s
 * `Stub!`-throwing android.jar shim (the established convention for
 * JSON-touching unit tests in this codebase, e.g. `FileSecretBackendTest`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentStateTest {

    @Test
    fun contactsToJsonMapsAllFields() {
        val row = Contact(peerId = "aa".repeat(32), status = "confirmed", handle = "alice", nodeUrl = "https://n.example")
        val json = TestAgent.contactsToJson(listOf(row))
        assertEquals(1, json.length())
        val obj = json.getJSONObject(0)
        assertEquals("aa".repeat(32), obj.getString("peer_id"))
        assertEquals("confirmed", obj.getString("status"))
        assertEquals("alice", obj.getString("handle"))
        assertEquals("https://n.example", obj.getString("node_url"))
    }

    @Test
    fun contactsToJsonHandlesNullOptionalFields() {
        val row = Contact(peerId = "bb".repeat(32), status = "pending", handle = null, nodeUrl = null)
        val obj = TestAgent.contactsToJson(listOf(row)).getJSONObject(0)
        assertTrue("handle should be JSON null, not absent", obj.isNull("handle"))
        assertTrue("node_url should be JSON null, not absent", obj.isNull("node_url"))
    }

    @Test
    fun contactsToJsonEmptyListYieldsEmptyArray() {
        assertEquals(0, TestAgent.contactsToJson(emptyList()).length())
    }

    @Test
    fun knocksToJsonMapsAllFields() {
        val row = Knock(id = 1, sender = "bob", senderNode = "https://bob.example", summary = "hi", createdAt = 1_700_000_000_000L)
        val obj = TestAgent.knocksToJson(listOf(row)).getJSONObject(0)
        assertEquals("bob", obj.getString("sender"))
        assertEquals("https://bob.example", obj.getString("sender_node"))
        assertEquals("hi", obj.getString("summary"))
        assertEquals(1_700_000_000_000L, obj.getLong("timestamp"))
    }

    @Test
    fun syncFilesToJsonMapsAllFields() {
        val row = SyncFile(
            path = "/docs/a.txt",
            folder = "set-1",
            sizeBytes = 1024L,
            manifestHash = "cc".repeat(32),
            state = SyncFileState.SYNCED,
            updatedAt = 1_700_000_000_000L,
        )
        val obj = TestAgent.syncFilesToJson(listOf(row)).getJSONObject(0)
        assertEquals("/docs/a.txt", obj.getString("path"))
        assertEquals("set-1", obj.getString("folder"))
        assertEquals(1024L, obj.getLong("size_bytes"))
        assertEquals("synced", obj.getString("state"))
    }

    @Test
    fun syncFilesToJsonLowercasesEveryState() {
        SyncFileState.entries.forEach { state ->
            val row = SyncFile(
                path = "/x",
                folder = "set",
                sizeBytes = 0L,
                manifestHash = "dd".repeat(32),
                state = state,
                updatedAt = 0L,
            )
            val obj = TestAgent.syncFilesToJson(listOf(row)).getJSONObject(0)
            assertEquals(state.name.lowercase(), obj.getString("state"))
        }
    }
}
