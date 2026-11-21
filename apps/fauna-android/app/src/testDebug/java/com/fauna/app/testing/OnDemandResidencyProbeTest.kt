package com.fauna.app.testing

import com.fauna.app.provider.OnDemandAck
import com.fauna.app.provider.OnDemandItem
import com.fauna.app.provider.OnDemandSetHost
import com.fauna.app.provider.OnDemandSweep
import com.fauna.app.provider.OnDemandWriteOpen
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

/**
 * The bridge's residency read and cache trim (`OnDemandResidencyProbe`), over a
 * fake owned-tree host laid out as the shared Rust lays the two roots: the kept
 * root under `filesDir`, the cache root under `cacheDir`
 * (`on-demand-files-android.md` § Android SAF DocumentsProvider binding, *Two
 * roots per set*).
 */
class OnDemandResidencyProbeTest {

    @get:Rule val tmp = TemporaryFolder()

    private val actor = "ab".repeat(32)

    private class Host(val kept: File, val cache: File) : OnDemandSetHost {
        val rows = mutableMapOf<String, Long>()
        val hydrated = mutableSetOf<String>()

        override suspend fun enumerate(parentRel: String) = rows.map { (rel, size) -> item(rel, size) }
        override suspend fun item(rel: String) =
            rows[rel]?.let { item(rel, it) } ?: if (rel == "dir") OnDemandItem("dir", "dir", 0, 0, isDir = true) else null
        private fun item(rel: String, size: Long) = OnDemandItem(rel, rel.substringAfterLast('/'), size, 0, isDir = false)
        override suspend fun hydratedRels() = hydrated.toList()
        override suspend fun lookupBody(rel: String) =
            listOf(File(kept, rel), File(cache, rel)).firstOrNull { it.isFile }?.absolutePath
        override suspend fun openForRead(rel: String) = error("unused")
        override suspend fun openForWrite(rel: String): OnDemandWriteOpen = error("unused")
        override suspend fun closedWrite(rel: String, baseVersion: ByteArray): OnDemandAck = error("unused")
        override suspend fun createDocument(rel: String): OnDemandAck = error("unused")
        override suspend fun deleteDocument(rel: String) = error("unused")
        override suspend fun renameDocument(fromRel: String, toRel: String): OnDemandAck = error("unused")
        override suspend fun refresh() = false
        override suspend fun sweepKeptRoot() = OnDemandSweep(emptyList(), emptyList(), emptyList())
        override fun close() {}
    }

    private fun layout(): Triple<File, File, Host> {
        val filesDir = tmp.newFolder("files")
        val cacheDir = tmp.newFolder("cache")
        val set = "local%3A1"
        val host = Host(
            File(filesDir, "on-demand/$actor/$set").apply { mkdirs() },
            File(cacheDir, "on-demand/$actor/$set").apply { mkdirs() },
        )
        return Triple(filesDir, cacheDir, host)
    }

    @Test
    fun splitTakesTheSetAndThePathAndRefusesTheRootAndASet() {
        assertEquals("local%3A1@$actor" to "a/b.txt", OnDemandResidencyProbe.split("local%3A1@$actor/a/b.txt"))
        assertNull(OnDemandResidencyProbe.split("fauna"))
        assertNull(OnDemandResidencyProbe.split("local%3A1@$actor"))
        assertNull(OnDemandResidencyProbe.split("local%3A1@$actor/"))
    }

    @Test
    fun aPlaceholderAHydratedBodyAndAnUnrecordedEditReadApart() = runTest {
        val (filesDir, cacheDir, host) = layout()
        host.rows += mapOf("p.txt" to 5L, "h.txt" to 3L, "e.txt" to 4L)

        // A placeholder: listed at its size, no body anywhere.
        val p = OnDemandResidencyProbe.read(host, "p.txt", filesDir, cacheDir)!!
        assertEquals(5L, p.sizeBytes)
        assertFalse(p.hydrated)
        assertNull(p.root)
        assertEquals(0L, p.bodyBytes)

        // A recorded body: hydrated, in the cache root.
        File(host.cache, "h.txt").writeText("abc")
        host.hydrated += "h.txt"
        val h = OnDemandResidencyProbe.read(host, "h.txt", filesDir, cacheDir)!!
        assertTrue(h.hydrated)
        assertEquals(OnDemandResidencyProbe.Root.CACHE, h.root)
        assertEquals(3L, h.bodyBytes)

        // An edit the nest has not recorded: in the kept root.
        File(host.kept, "e.txt").writeText("edit")
        val e = OnDemandResidencyProbe.read(host, "e.txt", filesDir, cacheDir)!!
        assertEquals(OnDemandResidencyProbe.Root.KEPT, e.root)

        // Not listed, or a directory: no residency.
        assertNull(OnDemandResidencyProbe.read(host, "gone.txt", filesDir, cacheDir))
        assertNull(OnDemandResidencyProbe.read(host, "dir", filesDir, cacheDir))
    }

    @Test
    fun theTrimEmptiesTheAccountsCacheRootsAndNeverTheKeptRoot() = runTest {
        val (filesDir, cacheDir, host) = layout()
        host.rows += mapOf("h.txt" to 3L, "sub/i.txt" to 2L, "e.txt" to 4L)
        File(host.cache, "h.txt").writeText("abc")
        File(host.cache, "sub").mkdirs()
        File(host.cache, "sub/i.txt").writeText("hi")
        File(host.kept, "e.txt").writeText("edit")
        // Another account's cache and the bridge's own staging are not this trim's.
        val other = File(cacheDir, "on-demand/${"cd".repeat(32)}/x.txt").apply { parentFile!!.mkdirs(); writeText("x") }
        val staged = File(cacheDir, "e2e-input/in.txt").apply { parentFile!!.mkdirs(); writeText("in") }

        assertEquals(2, OnDemandResidencyProbe.reclaimCache(cacheDir, actor))

        assertNull(OnDemandResidencyProbe.read(host, "h.txt", filesDir, cacheDir)!!.root)
        assertNull(OnDemandResidencyProbe.read(host, "sub/i.txt", filesDir, cacheDir)!!.root)
        assertEquals(OnDemandResidencyProbe.Root.KEPT, OnDemandResidencyProbe.read(host, "e.txt", filesDir, cacheDir)!!.root)
        assertTrue(other.isFile)
        assertTrue(staged.isFile)
    }
}
