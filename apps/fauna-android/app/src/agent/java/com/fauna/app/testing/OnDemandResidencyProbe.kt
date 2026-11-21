package com.fauna.app.testing

import com.fauna.app.provider.FfiOnDemandSource
import com.fauna.app.provider.OnDemandSetHost
import java.io.File

/**
 * Where one document of the SAF documents provider rests on this device — the
 * reading the androidTest bridge's `/documents/residency` serves
 * (`drivers/android.py`'s `documents_residency`), and the cache trim its
 * `/documents/reclaim-cache` stands in for.
 *
 * Android's on-demand surface has no host-visible file (`on-demand-files-android.md`
 * § Android SAF DocumentsProvider binding): a body rests in the set's **kept
 * root** (`filesDir`: an edit the nest has not recorded) or its **cache root**
 * (`cacheDir`: a recorded body the nest can serve again), or nowhere — a
 * placeholder. So the e2e legs of `docs/features/files-on-demand.md` (outcomes
 * 4, 5, 8, 11) read residency in-process, off the host the provider itself
 * calls, the way a desktop leg reads a FUSE `stat` or a cloud-files attribute.
 *
 * Debug-only (`src/agent`): compiled out of every release artifact with the
 * rest of the automation surface (e2e-automation-surface-gating.md).
 */
object OnDemandResidencyProbe {

    /** Which root holds a body. */
    enum class Root(val wire: String) { KEPT("kept"), CACHE("cache"), OTHER("other") }

    /**
     * One document's residency: the row's size as the provider lists it,
     * whether the host holds the row hydrated, which root its body rests in
     * (null — a placeholder) and the body's bytes on disk.
     */
    data class Residency(val sizeBytes: Long, val hydrated: Boolean, val root: Root?, val bodyBytes: Long)

    /**
     * A provider document id split into its set and its path: the id grammar
     * is `<ref-component>@<actor-id-hex>/<rel>` (the provider's own parse).
     * Null for the account's root or a set itself — neither has a body.
     */
    fun split(documentId: String): Pair<String, String>? {
        val scopedId = documentId.substringBefore('/', "")
        val rel = documentId.substringAfter('/', "")
        if (scopedId.isEmpty() || !scopedId.contains('@') || rel.isEmpty()) return null
        return scopedId to rel
    }

    /** [rel]'s residency on [host], or null when the set lists no such document. */
    suspend fun read(host: OnDemandSetHost, rel: String, filesDir: File, cacheDir: File): Residency? {
        val item = host.item(rel) ?: return null
        if (item.isDir) return null
        val hydrated = rel in host.hydratedRels()
        val body = host.lookupBody(rel)?.let(::File)?.takeIf { it.isFile }
        val root = body?.let {
            when {
                it.startsWith(filesDir) -> Root.KEPT
                it.startsWith(cacheDir) -> Root.CACHE
                else -> Root.OTHER
            }
        }
        return Residency(item.sizeBytes, hydrated, root, body?.length() ?: 0L)
    }

    /**
     * The OS's cache trim, as storage pressure or the user's own "Clear cache"
     * runs it: every recorded body of [actorHex]'s on-demand sets is deleted
     * from the cache dir. Nothing of ours is told — the host observes the
     * reclaim at its next open, start or refresh (the binding's *Freeing space
     * is the OS's cache reclaim*). The kept root is never touched: the OS
     * reclaims only the cache dir. Returns how many files went.
     *
     * Only the on-demand tree is trimmed, never the whole cache dir: the bridge
     * stages its own inputs there (`e2e-input`), and a real trim's reach over
     * the rest is the OS's choice, which no assertion depends on.
     */
    fun reclaimCache(cacheDir: File, actorHex: String): Int {
        val tree = File(cacheDir, "${FfiOnDemandSource.CACHE_TREE}/$actorHex")
        val files = tree.walkBottomUp().filter { it.isFile }.toList()
        files.forEach { it.delete() }
        return files.count { !it.exists() }
    }
}
