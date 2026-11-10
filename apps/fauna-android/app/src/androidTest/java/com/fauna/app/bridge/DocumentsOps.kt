package com.fauna.app.bridge

import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import com.fauna.app.provider.FaunaDocumentsProvider
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
 * and read each presented set's park.
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
        )
        context.contentResolver.query(uri, projection, null, null, null)?.use { c ->
            while (c.moveToNext()) {
                out.put(
                    JSONObject()
                        .put("id", c.getString(0))
                        .put("name", c.getString(1))
                        .put("flags", c.getInt(2)),
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

    /** Each presented set's park read: name, folder id, the host's mark, the un-recorded count. */
    fun parks(): JSONArray {
        val source = EntryPointAccessors.fromApplication(
            context.applicationContext,
            FaunaDocumentsProvider.OnDemandSourceEntryPoint::class.java,
        ).onDemandSource()
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
