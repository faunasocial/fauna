package com.fauna.app.bridge

import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import com.fauna.app.provider.FaunaDocumentsProvider
import com.fauna.app.provider.FfiOnDemandSource
import com.fauna.app.testing.OnDemandResidencyProbe
import dagger.hilt.android.EntryPointAccessors
import kotlinx.coroutines.runBlocking
import org.json.JSONArray
import org.json.JSONObject
import java.io.FileOutputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger

/**
 * The documents provider as another app's picker drives it
 * (`drivers/android.py`'s `documents_*` verbs): list a directory, create a
 * document, open one for write and later write + close it, read one back,
 * read each presented set's park, read one document's residency, and trim
 * the account's on-demand cache as the OS does.
 *
 * The provider is guarded by `MANAGE_DOCUMENTS`, which `adb shell content`
 * does not hold; the instrumentation runs in the app's own process, whose uid
 * the provider admits, so its `ContentResolver` calls go through exactly the
 * provider entry points a picker's do — `openDocument`, the written-close
 * listener, `createDocument`. Nothing here reaches past the provider into the
 * host but [parks], the in-process read of the host's own state DB the app's
 * page folds (`file-sync.md` § Multi-writer shared sets → *Revocation on a
 * capability host*).
 *
 * An open-for-write is held across calls ([openWrite] then [writeClose]) so a
 * journey can open while the plan still advertises write and write after the
 * owner's demotion: the edit a user had open when their access changed.
 */
class DocumentsOps(private val context: Context) {
    private val authority = "${context.packageName}.documents"
    private val open = ConcurrentHashMap<Int, ParcelFileDescriptor>()
    private val handles = AtomicInteger(0)

    private fun docUri(documentId: String): Uri = DocumentsContract.buildDocumentUri(authority, documentId)

    /** The children of [parent] (the root's id lists the account's sets). */
    fun children(parent: String): JSONArray {
        val out = JSONArray()
        val uri = DocumentsContract.buildChildDocumentsUri(authority, parent)
        val projection = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_FLAGS,
            DocumentsContract.Document.COLUMN_SIZE,
        )
        context.contentResolver.query(uri, projection, null, null, null)?.use { c ->
            while (c.moveToNext()) {
                out.put(
                    JSONObject()
                        .put("id", c.getString(0))
                        .put("name", c.getString(1))
                        .put("flags", c.getInt(2))
                        // A directory row carries no size.
                        .put("size", if (c.isNull(3)) JSONObject.NULL else c.getLong(3)),
                )
            }
        }
        return out
    }

    /** Create an empty document named [name] in [parent]; its new id. */
    fun create(parent: String, name: String): String {
        val created = DocumentsContract.createDocument(context.contentResolver, docUri(parent), "text/plain", name)
            ?: error("the provider created no document for $name")
        return DocumentsContract.getDocumentId(created)
    }

    /** Open [documentId] for write and hold the descriptor; a handle for [writeClose]. */
    fun openWrite(documentId: String): Int {
        val pfd = context.contentResolver.openFileDescriptor(docUri(documentId), "rw")
            ?: error("the provider opened no descriptor for $documentId")
        return handles.incrementAndGet().also { open[it] = pfd }
    }

    /** Replace the held descriptor's bytes with [body] and close it — the write's close. */
    fun writeClose(handle: Int, body: ByteArray) {
        val pfd = open.remove(handle) ?: error("no open descriptor $handle")
        try {
            FileOutputStream(pfd.fileDescriptor).use { out ->
                out.channel.truncate(0)
                out.channel.position(0)
                out.write(body)
                out.fd.sync()
            }
        } finally {
            pfd.close()
        }
    }

    /** [documentId]'s bytes, as the provider serves them. */
    fun read(documentId: String): ByteArray =
        context.contentResolver.openInputStream(docUri(documentId))?.use { it.readBytes() }
            ?: error("the provider opened no stream for $documentId")

    private fun source(): FfiOnDemandSource = EntryPointAccessors.fromApplication(
        context.applicationContext,
        FaunaDocumentsProvider.OnDemandSourceEntryPoint::class.java,
    ).onDemandSource()

    /**
     * Where [documentId]'s body rests, read in-process off the host the provider
     * calls (`OnDemandResidencyProbe`): `{listed, size, hydrated, root, body_bytes}`,
     * `root` being `kept`, `cache` or null (a placeholder); `listed` false when the
     * set lists no such document. Reading never opens the document, so it never
     * hydrates — the twin of a desktop leg's `stat`.
     */
    fun residency(documentId: String): JSONObject {
        val (scopedId, rel) = OnDemandResidencyProbe.split(documentId)
            ?: error("not a document id: $documentId")
        val host = runBlocking { source().host(scopedId) } ?: error("no presented set: $scopedId")
        val read = runBlocking { OnDemandResidencyProbe.read(host, rel, context.filesDir, context.cacheDir) }
            ?: return JSONObject().put("listed", false)
        return JSONObject()
            .put("listed", true)
            .put("size", read.sizeBytes)
            .put("hydrated", read.hydrated)
            .put("root", read.root?.wire ?: JSONObject.NULL)
            .put("body_bytes", read.bodyBytes)
    }

    /**
     * The OS's cache trim over the signed-in account's on-demand cache roots
     * (`OnDemandResidencyProbe.reclaimCache`); how many files went. Nothing of
     * the app's is told — the host observes it at its next open or pull.
     */
    fun reclaimCache(): Int {
        val actor = source().activeActorHex() ?: error("signed out: no account's cache to trim")
        return OnDemandResidencyProbe.reclaimCache(context.cacheDir, actor)
    }

    /** Each presented set's park read: name, folder id, the host's mark, the un-recorded count. */
    fun parks(): JSONArray {
        val source = source()
        val out = JSONArray()
        runBlocking { source.presentedParks() }.forEach { (name, park) ->
            out.put(
                JSONObject()
                    .put("name", name)
                    .put("folder_id", park.folderId)
                    .put("access_revoked", park.accessRevoked)
                    .put("unrecorded", park.unrecorded.toLong()),
            )
        }
        return out
    }
}
