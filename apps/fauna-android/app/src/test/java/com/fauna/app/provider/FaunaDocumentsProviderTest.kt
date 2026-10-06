package com.fauna.app.provider

import android.content.Context
import android.content.pm.ProviderInfo
import android.provider.DocumentsContract
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.testing.FaunaRobolectricTestRunner
import java.io.File
import java.io.FileNotFoundException
import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

/**
 * The provider contract of the android on-demand binding
 * (`on-demand-files.md` § Android SAF DocumentsProvider binding), driven
 * against a fake [OnDemandSource] — the Kotlin translation layer headlessly:
 * the read path, and the write half's mapping onto the host (flags, the close
 * listener's one `closedWrite` per open, create / delete / rename / move, the
 * sweep re-driven by a pull). The host's own behaviour (hydrate-on-open,
 * close → ingest → demote, record-first delete and rename, the start sweep) is
 * proven against a real nest by `conformance_file_provider_client.rs`; the
 * device leg is the emulator instrumented test.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class FaunaDocumentsProviderTest {

    private val actor = "ab".repeat(32)
    private val setId = "local%3A7@$actor"
    private lateinit var context: Context
    private lateinit var authority: String
    private lateinit var bodies: File
    private lateinit var host: FakeHost
    private lateinit var source: FakeSource
    private lateinit var provider: FaunaDocumentsProvider

    @Before
    fun setUp() {
        context = ApplicationProvider.getApplicationContext()
        authority = "${context.packageName}.documents"
        bodies = File(context.cacheDir, "fake-bodies").apply { mkdirs() }
        host = FakeHost(bodies)
        source = FakeSource(actor, listOf(OnDemandSet(setId, "Documents")), mapOf(setId to host))
        FaunaDocumentsProvider.sourceOverride = source
        provider = Robolectric.buildContentProvider(FaunaDocumentsProvider::class.java).create(
            ProviderInfo().apply {
                this.authority = this@FaunaDocumentsProviderTest.authority
                exported = true
                grantUriPermissions = true
                readPermission = android.Manifest.permission.MANAGE_DOCUMENTS
                writePermission = android.Manifest.permission.MANAGE_DOCUMENTS
            },
        ).get()
    }

    @After
    fun tearDown() {
        FaunaDocumentsProvider.sourceOverride = null
        bodies.deleteRecursively()
    }

    private fun rootsCount(): Int =
        context.contentResolver.query(DocumentsContract.buildRootsUri(authority), null, android.os.Bundle(), null)!!
            .use { it.count }

    private fun children(parent: String): List<Map<String, Any?>> =
        context.contentResolver.query(
            DocumentsContract.buildChildDocumentsUri(authority, parent), null, android.os.Bundle(), null,
        )!!.use { c ->
            generateSequence { if (c.moveToNext()) c else null }.map { row ->
                (0 until row.columnCount).associate { i ->
                    row.getColumnName(i) to when (row.getType(i)) {
                        android.database.Cursor.FIELD_TYPE_INTEGER -> row.getLong(i)
                        android.database.Cursor.FIELD_TYPE_NULL -> null
                        else -> row.getString(i)
                    }
                }
            }.toList()
        }

    private fun readDocument(id: String): ByteArray =
        provider.openDocument(id, "r", null).use { pfd ->
            java.io.FileInputStream(pfd.fileDescriptor).readBytes()
        }

    @Test
    fun signedOutServesNoRootAndNoDocument() {
        source.actorHex = null
        assertEquals(0, rootsCount())
        try {
            provider.queryDocument("$setId/a.txt", null)
            fail("a signed-out provider must not resolve a document")
        } catch (_: FileNotFoundException) {
        }
    }

    @Test
    fun oneRootForTheActiveAccountWhoseChildrenAreTheDesiredSets() {
        context.contentResolver.query(DocumentsContract.buildRootsUri(authority), null, android.os.Bundle(), null)!!.use { c ->
            assertEquals(1, c.count)
            c.moveToFirst()
            assertEquals(actor, c.getString(c.getColumnIndexOrThrow(DocumentsContract.Root.COLUMN_ROOT_ID)))
            assertEquals(
                FaunaDocumentsProvider.ROOT_DOCUMENT_ID,
                c.getString(c.getColumnIndexOrThrow(DocumentsContract.Root.COLUMN_DOCUMENT_ID)),
            )
        }
        val sets = children(FaunaDocumentsProvider.ROOT_DOCUMENT_ID)
        assertEquals(listOf(setId), sets.map { it[DocumentsContract.Document.COLUMN_DOCUMENT_ID] })
        assertEquals("Documents", sets[0][DocumentsContract.Document.COLUMN_DISPLAY_NAME])
        assertEquals(DocumentsContract.Document.MIME_TYPE_DIR, sets[0][DocumentsContract.Document.COLUMN_MIME_TYPE])
        assertTrue("the root query re-reads the set list", source.refreshedSets > 0)
    }

    @Test
    fun aSetsChildrenCarryTheHostsRowsUnderActorScopedIds() {
        host.items[""] = listOf(
            OnDemandItem("report.pdf", "report.pdf", 1234, 1_700_000_000, isDir = false),
            OnDemandItem("photos", "photos", 0, 0, isDir = true),
        )
        host.items["photos"] = listOf(OnDemandItem("photos/cat.jpg", "cat.jpg", 99, 1_700_000_100, isDir = false))

        val top = children(setId).associateBy { it[DocumentsContract.Document.COLUMN_DOCUMENT_ID] }
        val file = top.getValue("$setId/report.pdf")
        assertEquals("report.pdf", file[DocumentsContract.Document.COLUMN_DISPLAY_NAME])
        assertEquals(1234L, file[DocumentsContract.Document.COLUMN_SIZE])
        assertEquals(1_700_000_000_000L, file[DocumentsContract.Document.COLUMN_LAST_MODIFIED])
        assertEquals(
            DocumentsContract.Document.MIME_TYPE_DIR,
            top.getValue("$setId/photos")[DocumentsContract.Document.COLUMN_MIME_TYPE],
        )
        val nested = children("$setId/photos")
        assertEquals(listOf("$setId/photos/cat.jpg"), nested.map { it[DocumentsContract.Document.COLUMN_DOCUMENT_ID] })
        assertTrue(provider.isChildDocument(setId, "$setId/photos/cat.jpg"))
        assertTrue(provider.isChildDocument(FaunaDocumentsProvider.ROOT_DOCUMENT_ID, setId))
        assertFalse(provider.isChildDocument("$setId/photos", "$setId/report.pdf"))
    }

    @Test
    fun documentsAdvertiseWriteDeleteRenameMoveAndNothingAdvertisesAThumbnail() {
        host.items[""] = listOf(
            OnDemandItem("a.txt", "a.txt", 1, 1, isDir = false),
            OnDemandItem("d", "d", 0, 0, isDir = true),
        )
        val set = children(FaunaDocumentsProvider.ROOT_DOCUMENT_ID).single()
        assertEquals(
            DocumentsContract.Document.FLAG_DIR_SUPPORTS_CREATE.toLong(),
            set[DocumentsContract.Document.COLUMN_FLAGS],
        )
        val rows = children(setId).associateBy { it[DocumentsContract.Document.COLUMN_DOCUMENT_ID] }
        val file = (rows.getValue("$setId/a.txt")[DocumentsContract.Document.COLUMN_FLAGS] as Long).toInt()
        val dir = (rows.getValue("$setId/d")[DocumentsContract.Document.COLUMN_FLAGS] as Long).toInt()
        val shared = DocumentsContract.Document.FLAG_SUPPORTS_DELETE or
            DocumentsContract.Document.FLAG_SUPPORTS_RENAME or
            DocumentsContract.Document.FLAG_SUPPORTS_MOVE
        assertEquals(DocumentsContract.Document.FLAG_SUPPORTS_WRITE or shared, file)
        assertEquals(DocumentsContract.Document.FLAG_DIR_SUPPORTS_CREATE or shared, dir)
        assertEquals(0, (file or dir) and DocumentsContract.Document.FLAG_SUPPORTS_THUMBNAIL)
    }

    @Test
    fun aReadersSetAdvertisesNoWriteAndRefusesEveryWriteBeforeTheHost() {
        source.sets = listOf(OnDemandSet(setId, "Documents (alice)", readOnly = true))
        host.items[""] = listOf(
            OnDemandItem("a.txt", "a.txt", 1, 1, isDir = false),
            OnDemandItem("d", "d", 0, 0, isDir = true),
        )
        host.remote["a.txt"] = "alpha".toByteArray()

        val set = children(FaunaDocumentsProvider.ROOT_DOCUMENT_ID).single()
        assertEquals("Documents (alice)", set[DocumentsContract.Document.COLUMN_DISPLAY_NAME])
        assertEquals(0L, set[DocumentsContract.Document.COLUMN_FLAGS])
        children(setId).forEach { row ->
            assertEquals("${row[DocumentsContract.Document.COLUMN_DOCUMENT_ID]}", 0L, row[DocumentsContract.Document.COLUMN_FLAGS])
        }
        provider.queryDocument("$setId/a.txt", null).use { c ->
            c.moveToFirst()
            assertEquals(0, c.getInt(c.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_FLAGS)))
        }

        // Reading still opens the real body.
        assertEquals("alpha", String(readDocument("$setId/a.txt")))

        try {
            provider.openDocument("$setId/a.txt", "rw", null)
            fail("a reader's document must not open for write")
        } catch (_: FileNotFoundException) {
        }
        val refused = listOf<() -> Unit>(
            { provider.createDocument(setId, "text/plain", "new.txt") },
            { provider.deleteDocument("$setId/a.txt") },
            { provider.renameDocument("$setId/a.txt", "b.txt") },
            { provider.moveDocument("$setId/a.txt", setId, "$setId/d") },
        )
        refused.forEach { write ->
            try {
                write()
                fail("a reader's set must refuse every write")
            } catch (_: UnsupportedOperationException) {
            }
        }
        assertTrue(host.created.isEmpty())
        assertTrue(host.renamed.isEmpty())
        assertTrue(host.closes.isEmpty())
        assertEquals(2, host.items.getValue("").size)
    }

    @Test
    fun aWrittenDescriptorsCloseCallsClosedWriteOnceWithTheOpensBase() {
        host.remote["notes/a.txt"] = "old".toByteArray()
        provider.openDocument("$setId/notes/a.txt", "wt", null).use { pfd ->
            java.io.FileOutputStream(pfd.fileDescriptor).write("new bytes".toByteArray())
        }
        host.awaitCloses(1)
        assertEquals(listOf("notes/a.txt" to "base:notes/a.txt"), host.closes.map { it.first to String(it.second) })
        assertArrayEquals("new bytes".toByteArray(), File(host.keptRoot, "notes/a.txt").readBytes())
        Thread.sleep(100)
        assertEquals("exactly one closedWrite per open", 1, host.closes.size)
    }

    @Test
    fun anErrorCloseStillClosesTheWriteExactlyOnce() {
        // Robolectric's in-process descriptor does not carry a remote
        // closeWithError to the listener, so the listener the provider hands
        // `ParcelFileDescriptor.open` is driven directly with the error a
        // crashed writer's close delivers. The device leg drives it end to end.
        val listener = provider.writtenCloseListener(setId, "a.txt", host, "base:a.txt".toByteArray())
        listener.onClose(java.io.IOException("the writer crashed"))
        host.awaitCloses(1)
        Thread.sleep(100)
        assertEquals(listOf("a.txt" to "base:a.txt"), host.closes.map { it.first to String(it.second) })
    }

    @Test
    fun anUnfetchableWriteOpenFailsAndOpensNoWriter() {
        host.offline = true
        try {
            provider.openDocument("$setId/a.txt", "w", null)
            fail("a placeholder that cannot be fetched cannot be opened for write")
        } catch (_: FileNotFoundException) {
        }
        assertTrue(host.closes.isEmpty())
    }

    @Test
    fun createGoesThroughTheHostAndTakesTheNextFreeName() {
        host.items[""] = listOf(OnDemandItem("a.txt", "a.txt", 1, 1, isDir = false))
        assertEquals("$setId/b.txt", provider.createDocument(setId, "text/plain", "b.txt"))
        assertEquals("$setId/a (1).txt", provider.createDocument(setId, "text/plain", "a.txt"))
        host.items["d"] = emptyList()
        assertEquals("$setId/d/n.txt", provider.createDocument("$setId/d", "text/plain", "n.txt"))
        assertEquals(listOf("b.txt", "a (1).txt", "d/n.txt"), host.created)
    }

    @Test
    fun anEmptyDirectoryAndAnIgnoredNameAreNotCreated() {
        try {
            provider.createDocument(setId, DocumentsContract.Document.MIME_TYPE_DIR, "new folder")
            fail("an empty directory has no row and cannot be created")
        } catch (_: UnsupportedOperationException) {
        }
        try {
            provider.createDocument(setId, "text/plain", ".hidden")
            fail("an ignored name is refused")
        } catch (_: IllegalArgumentException) {
        }
        try {
            provider.createDocument(FaunaDocumentsProvider.ROOT_DOCUMENT_ID, "text/plain", "x.txt")
            fail("nothing is created beside the sets")
        } catch (_: UnsupportedOperationException) {
        }
    }

    @Test
    fun anUnrecordableDeleteIsRefusedAndTheDocumentStillLists() {
        host.items[""] = listOf(OnDemandItem("a.txt", "a.txt", 1, 1, isDir = false))
        host.offline = true
        try {
            provider.deleteDocument("$setId/a.txt")
            fail("a delete the nest cannot record must reach the caller as a failure")
        } catch (_: IllegalStateException) {
        }
        assertEquals(listOf("$setId/a.txt"), children(setId).map { it[DocumentsContract.Document.COLUMN_DOCUMENT_ID] })

        host.offline = false
        provider.deleteDocument("$setId/a.txt")
        assertTrue(children(setId).isEmpty())
        try {
            provider.deleteDocument(setId)
            fail("a set is never deleted through the provider")
        } catch (_: UnsupportedOperationException) {
        }
    }

    @Test
    fun renameAndMoveGoThroughTheHostsRenameAndReturnTheNewId() {
        host.items[""] = listOf(OnDemandItem("a.txt", "a.txt", 1, 1, isDir = false))
        assertEquals("$setId/b.txt", provider.renameDocument("$setId/a.txt", "b.txt"))
        assertEquals("$setId/d/b.txt", provider.moveDocument("$setId/b.txt", setId, "$setId/d"))
        assertEquals("$setId/e", provider.renameDocument("$setId/d", "e"))
        assertEquals(listOf("a.txt" to "b.txt", "b.txt" to "d/b.txt", "d" to "e"), host.renamed)
        try {
            provider.moveDocument("$setId/e", setId, "local%3A9@$actor")
            fail("a move across sets is refused")
        } catch (_: UnsupportedOperationException) {
        }
    }

    @Test
    fun anUnrecordableRenameIsRefused() {
        host.offline = true
        try {
            provider.renameDocument("$setId/a.txt", "b.txt")
            fail("a rename the nest cannot record must reach the caller as a failure")
        } catch (_: IllegalStateException) {
        }
    }

    @Test
    fun aBrowserReQueryReDrivesTheKeptRootSweep() {
        children(setId)
        val deadline = System.currentTimeMillis() + 5_000
        while (host.sweeps == 0 && System.currentTimeMillis() < deadline) Thread.sleep(20)
        assertTrue("a pull sweeps the kept root, so a write closed offline uploads", host.sweeps > 0)
    }

    @Test
    fun openReturnsTheBodyTheHostHydrated() {
        host.remote["notes/a.txt"] = "the real bytes".toByteArray()
        assertArrayEquals("the real bytes".toByteArray(), readDocument("$setId/notes/a.txt"))
        assertEquals(listOf("notes/a.txt"), host.opened)
    }

    @Test
    fun anUnfetchableBodyFailsTheOpenAndNeverServesOtherBytes() {
        host.offline = true
        try {
            readDocument("$setId/a.txt")
            fail("an unfetchable placeholder must fail the open")
        } catch (_: FileNotFoundException) {
        }
    }

    @Test
    fun aDeletedCacheBodyIsFetchedAgainOnTheNextOpen() {
        host.remote["a.txt"] = "v1".toByteArray()
        readDocument("$setId/a.txt")
        File(bodies, "a.txt").delete()
        assertArrayEquals("v1".toByteArray(), readDocument("$setId/a.txt"))
        assertEquals("every open asks the host — the provider caches no body", 2, host.opened.size)
    }

    @Test
    fun anIdScopedToAnotherAccountIsNotFound() {
        val other = "local%3A7@${"cd".repeat(32)}"
        try {
            provider.queryDocument("$other/a.txt", null)
            fail("another account's id must not resolve")
        } catch (_: FileNotFoundException) {
        }
    }

    @Test
    fun teardownReannouncesTheRoots() {
        rootsCount()
        source.actorHex = null
        source.fireTeardown()
        assertTrue(
            shadowOf(context.contentResolver).notifiedUris.any {
                it.uri == DocumentsContract.buildRootsUri(authority)
            },
        )
        assertEquals(0, rootsCount())
    }

    private class FakeSource(
        var actorHex: String?,
        var sets: List<OnDemandSet>,
        val hosts: Map<String, OnDemandSetHost>,
    ) : OnDemandSource {
        var refreshedSets = 0
        private val listeners = mutableListOf<() -> Unit>()
        override fun activeActorHex() = actorHex
        override suspend fun sets(refresh: Boolean): List<OnDemandSet> {
            if (refresh) refreshedSets++
            return if (actorHex == null) emptyList() else sets
        }
        override suspend fun host(scopedId: String) = if (actorHex == null) null else hosts[scopedId]
        override fun liveHosts() = hosts
        override fun onTeardown(listener: () -> Unit) {
            listeners += listener
        }
        fun fireTeardown() = listeners.forEach { it() }
    }

    /**
     * Stands in for the owned-tree host: bodies "fetched" from [remote] into a
     * cache dir, or into [keptRoot] for a write; [offline] fails every fetch
     * and every write the nest would have to record.
     */
    private class FakeHost(private val cacheRoot: File) : OnDemandSetHost {
        val keptRoot = File(cacheRoot, "kept")
        val items = mutableMapOf<String, List<OnDemandItem>>()
        val remote = mutableMapOf<String, ByteArray>()
        val opened = mutableListOf<String>()
        val closes: MutableList<Pair<String, ByteArray>> = Collections.synchronizedList(mutableListOf())
        private val closed = CountDownLatch(1)
        val created = mutableListOf<String>()
        val renamed = mutableListOf<Pair<String, String>>()
        @Volatile var sweeps = 0
        var offline = false

        fun awaitCloses(n: Int) {
            val deadline = System.currentTimeMillis() + 5_000
            while (closes.size < n && System.currentTimeMillis() < deadline) closed.await(20, TimeUnit.MILLISECONDS)
            assertEquals(n, closes.size)
        }

        override suspend fun enumerate(parentRel: String) = items[parentRel].orEmpty()
        override suspend fun item(rel: String) =
            items.values.flatten().firstOrNull { it.rel == rel }
                ?: if (items.containsKey(rel)) OnDemandItem(rel, rel.substringAfterLast('/'), 0, 0, isDir = true) else null
        override suspend fun openForRead(rel: String): String {
            opened += rel
            val body = File(cacheRoot, rel)
            if (!body.isFile) {
                if (offline) error("offline")
                val bytes = remote[rel] ?: error("no such document")
                body.parentFile!!.mkdirs()
                body.writeBytes(bytes)
            }
            return body.absolutePath
        }
        override suspend fun openForWrite(rel: String): OnDemandWriteOpen {
            if (offline) error("offline")
            val body = File(keptRoot, rel)
            body.parentFile!!.mkdirs()
            body.writeBytes(remote[rel] ?: error("no such document"))
            return OnDemandWriteOpen(body.absolutePath, "base:$rel".toByteArray())
        }
        override suspend fun closedWrite(rel: String, baseVersion: ByteArray): OnDemandAck {
            closes += rel to baseVersion
            return OnDemandAck(acked = true, contentChanged = false, excluded = false)
        }
        override suspend fun createDocument(rel: String): OnDemandAck {
            if (rel.substringAfterLast('/').startsWith(".")) return OnDemandAck(acked = false, contentChanged = false, excluded = true)
            created += rel
            return OnDemandAck(acked = true, contentChanged = false, excluded = false)
        }
        override suspend fun deleteDocument(rel: String) {
            if (offline) error("could not be recorded")
            items.replaceAll { _, rows -> rows.filterNot { it.rel == rel } }
        }
        override suspend fun renameDocument(fromRel: String, toRel: String): OnDemandAck {
            if (offline) error("could not be recorded")
            renamed += fromRel to toRel
            return OnDemandAck(acked = true, contentChanged = false, excluded = false)
        }
        override suspend fun refresh() = false
        override suspend fun sweepKeptRoot(): OnDemandSweep {
            sweeps++
            return OnDemandSweep(emptyList(), emptyList(), emptyList())
        }
        override fun close() {}
    }
}
