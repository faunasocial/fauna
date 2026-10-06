package com.fauna.app.provider

import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.CancellationSignal
import android.os.Handler
import android.os.HandlerThread
import android.os.OperationCanceledException
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.DocumentsProvider
import android.webkit.MimeTypeMap
import androidx.annotation.VisibleForTesting
import com.fauna.app.R
import com.fauna.app.core.ShellLog
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent
import java.io.File
import java.io.FileNotFoundException
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking

/**
 * The SAF `DocumentsProvider` of the android on-demand binding
 * (`on-demand-files.md` § Android SAF DocumentsProvider binding). It runs in
 * the app's own process and translates `DocumentsContract` calls into the
 * shared-Rust host's (`provider_face` + `owned_tree`) through [OnDemandSource];
 * it holds no sync state and makes no sync decision.
 *
 * - **One root per active account**, none when signed out; its children are
 *   the account's desired sets (the shared presence plan).
 * - **Document ids** are `<ref-component>@<actor-id-hex>/<rel>` — the shared
 *   `ActorScopedFolderRef` identifier (`local%3A1@…`; the ref half percent-encoded,
 *   so it never carries the `/` this id splits on) followed by the folder-relative
 *   path (a set itself is its bare scoped id) — so a persisted grant survives a
 *   set rename; the set's name is only a label. An id scoped to another account
 *   is not found.
 * - **Open hydrates** into the cache root and returns the real body; a body
 *   that cannot be fetched fails the open with [FileNotFoundException] —
 *   never a stand-in body.
 * - **Two-way, record-first.** A write-mode open promotes the body into the
 *   kept root and hands out a descriptor whose close listener calls the host's
 *   `closedWrite` exactly once — an error close included; an un-recorded
 *   write stays in the kept root and the next sweep uploads it. Create, delete,
 *   rename and move go through the host's owned-tree operations and throw when
 *   the nest cannot record them; a directory is deleted, renamed or moved file
 *   by file, and an empty directory cannot be created (it has no row). No
 *   thumbnail flag is advertised, so browsing never hydrates.
 * - **A reader's set is read-only.** A folder shared with the account without
 *   a writer grant advertises no write, create, delete, rename or move on
 *   anything in it, and a write that arrives anyway is refused here before the
 *   host — which refuses it too.
 * - **Nothing resident**: a refresh tick runs while the process lives, the
 *   browser's own re-query triggers one too — each pull re-drives the
 *   kept-root sweep — and a change notifies the cursors. Dependencies resolve
 *   at the first call, never in [onCreate].
 */
class FaunaDocumentsProvider : DocumentsProvider() {

    /**
     * A `ContentProvider` is created before the app's Hilt injection can reach
     * it, so it pulls its source out of the graph at the first call.
     */
    @EntryPoint
    @InstallIn(SingletonComponent::class)
    interface OnDemandSourceEntryPoint {
        fun onDemandSource(): FfiOnDemandSource
    }

    companion object {
        private const val TAG = "FaunaDocumentsProvider"

        /** The root's own document: the parent of the account's sets. */
        const val ROOT_DOCUMENT_ID = "fauna"

        /** Test seam: a source that stands in for [FfiOnDemandSource]. */
        @VisibleForTesting
        @Volatile
        var sourceOverride: OnDemandSource? = null

        private val DEFAULT_ROOT_PROJECTION = arrayOf(
            DocumentsContract.Root.COLUMN_ROOT_ID,
            DocumentsContract.Root.COLUMN_MIME_TYPES,
            DocumentsContract.Root.COLUMN_FLAGS,
            DocumentsContract.Root.COLUMN_ICON,
            DocumentsContract.Root.COLUMN_TITLE,
            DocumentsContract.Root.COLUMN_SUMMARY,
            DocumentsContract.Root.COLUMN_DOCUMENT_ID,
        )

        /** A document inside a set: written, deleted, renamed and moved through the host. */
        private const val FILE_FLAGS = DocumentsContract.Document.FLAG_SUPPORTS_WRITE or
            DocumentsContract.Document.FLAG_SUPPORTS_DELETE or
            DocumentsContract.Document.FLAG_SUPPORTS_RENAME or
            DocumentsContract.Document.FLAG_SUPPORTS_MOVE

        /** A directory inside a set: its files are deleted, renamed and moved one by one. */
        private const val DIR_FLAGS = DocumentsContract.Document.FLAG_DIR_SUPPORTS_CREATE or
            DocumentsContract.Document.FLAG_SUPPORTS_DELETE or
            DocumentsContract.Document.FLAG_SUPPORTS_RENAME or
            DocumentsContract.Document.FLAG_SUPPORTS_MOVE

        /** A set itself: documents are created in it; the set is never renamed or deleted here. */
        private const val SET_FLAGS = DocumentsContract.Document.FLAG_DIR_SUPPORTS_CREATE

        private val DEFAULT_DOCUMENT_PROJECTION = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_LAST_MODIFIED,
            DocumentsContract.Document.COLUMN_FLAGS,
            DocumentsContract.Document.COLUMN_SIZE,
        )
    }

    /** A parsed document id: the account's root, a set, or a path inside a set. */
    private sealed interface DocId {
        object Root : DocId
        data class InSet(val scopedId: String, val rel: String) : DocId
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var resolved: OnDemandSource? = null
    private var tickStarted = false
    private val refreshing = ConcurrentHashMap.newKeySet<String>()

    /** The thread a written descriptor's close listener runs on (it only hands off to [scope]). */
    private val closeHandler: Handler by lazy {
        Handler(HandlerThread("fauna-documents-close").apply { start() }.looper)
    }

    private val authority: String
        get() = "${context!!.packageName}.documents"

    override fun onCreate(): Boolean = true

    /** The source, resolved at the first call (never in [onCreate]); starts the refresh tick. */
    @Synchronized
    private fun source(): OnDemandSource {
        resolved?.let { return it }
        val source = sourceOverride ?: EntryPointAccessors
            .fromApplication(context!!.applicationContext, OnDemandSourceEntryPoint::class.java)
            .onDemandSource()
        source.onTeardown { notify(DocumentsContract.buildRootsUri(authority)) }
        resolved = source
        startTick(source)
        return source
    }

    override fun queryRoots(projection: Array<out String>?): Cursor {
        val result = MatrixCursor(projection ?: DEFAULT_ROOT_PROJECTION)
        result.setNotificationUri(context!!.contentResolver, DocumentsContract.buildRootsUri(authority))
        val actor = source().activeActorHex() ?: return result
        result.newRow().apply {
            add(DocumentsContract.Root.COLUMN_ROOT_ID, actor)
            add(DocumentsContract.Root.COLUMN_MIME_TYPES, "*/*")
            add(
                DocumentsContract.Root.COLUMN_FLAGS,
                DocumentsContract.Root.FLAG_SUPPORTS_IS_CHILD or DocumentsContract.Root.FLAG_SUPPORTS_CREATE,
            )
            add(DocumentsContract.Root.COLUMN_ICON, android.R.drawable.ic_menu_save)
            add(DocumentsContract.Root.COLUMN_TITLE, "Fauna")
            add(DocumentsContract.Root.COLUMN_SUMMARY, context!!.getString(R.string.file_sync_saf_root_summary))
            add(DocumentsContract.Root.COLUMN_DOCUMENT_ID, ROOT_DOCUMENT_ID)
        }
        return result
    }

    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor {
        val result = MatrixCursor(projection ?: DEFAULT_DOCUMENT_PROJECTION)
        when (val id = parse(documentId)) {
            DocId.Root -> addDirRow(result, ROOT_DOCUMENT_ID, "Fauna", 0)
            is DocId.InSet -> {
                if (id.rel.isEmpty()) {
                    val set = runBlocking { source().sets(refresh = false) }
                        .firstOrNull { it.scopedId == id.scopedId }
                        ?: throw FileNotFoundException("no such set: $documentId")
                    addDirRow(result, set.scopedId, set.name, setFlags(set))
                } else {
                    val item = runBlocking { hostFor(id).item(id.rel) }
                        ?: throw FileNotFoundException("no such document: $documentId")
                    addItemRow(result, id.scopedId, item, readOnly(id.scopedId))
                }
            }
        }
        return result
    }

    override fun queryChildDocuments(
        parentDocumentId: String,
        projection: Array<out String>?,
        sortOrder: String?,
    ): Cursor {
        val result = MatrixCursor(projection ?: DEFAULT_DOCUMENT_PROJECTION)
        result.setNotificationUri(
            context!!.contentResolver,
            DocumentsContract.buildChildDocumentsUri(authority, parentDocumentId),
        )
        when (val id = parse(parentDocumentId)) {
            DocId.Root -> runBlocking { source().sets(refresh = true) }
                .forEach { addDirRow(result, it.scopedId, it.name, setFlags(it)) }
            is DocId.InSet -> {
                val host = hostFor(id)
                val readOnly = readOnly(id.scopedId)
                runBlocking { host.enumerate(id.rel) }
                    .forEach { addItemRow(result, id.scopedId, it, readOnly) }
                // The browser's own refresh: a re-query pulls the set once more
                // in the background and notifies if anything moved.
                refreshAsync(id.scopedId, host)
            }
        }
        return result
    }

    override fun openDocument(
        documentId: String,
        mode: String,
        signal: CancellationSignal?,
    ): ParcelFileDescriptor {
        val pfdMode = ParcelFileDescriptor.parseMode(mode)
        val id = parse(documentId) as? DocId.InSet
        if (id == null || id.rel.isEmpty()) throw FileNotFoundException("not a document: $documentId")
        val host = hostFor(id)
        signal?.throwIfCanceled()
        val write = pfdMode != ParcelFileDescriptor.MODE_READ_ONLY
        if (write && readOnly(id.scopedId)) throw FileNotFoundException("read-only: $documentId")
        val opened = try {
            runBlocking(Dispatchers.IO) {
                // A read honours cancellation mid-fetch. A write does not: an
                // open the host completed but the caller abandoned would count
                // a writer no close ever releases.
                if (!write) {
                    val open = coroutineContext.job
                    signal?.setOnCancelListener { open.cancel() }
                }
                if (write) host.openForWrite(id.rel) else OnDemandWriteOpen(host.openForRead(id.rel), ByteArray(0))
            }
        } catch (e: CancellationException) {
            throw OperationCanceledException("open cancelled: $documentId")
        } catch (e: Exception) {
            // Offline, signed out, a refused capability: a placeholder is
            // present-but-unreadable — never answered with other bytes.
            throw FileNotFoundException("unavailable: $documentId (${e.message})")
        }
        if (!write) return ParcelFileDescriptor.open(File(opened.path), pfdMode)
        // The host counts this open as a writer until its one closedWrite: the
        // listener fires once per descriptor, on a clean close and an error
        // close alike, and a descriptor that fails to open is closed here.
        val listener = writtenCloseListener(id.scopedId, id.rel, host, opened.baseVersion)
        return try {
            ParcelFileDescriptor.open(File(opened.path), pfdMode, closeHandler, listener)
        } catch (e: Exception) {
            listener.onClose(null)
            throw FileNotFoundException("unavailable: $documentId (${e.message})")
        }
    }

    /**
     * The close listener of one written descriptor: it fires once, on a clean
     * close and an error close alike, and the host ingests the kept-root body
     * against the open's base. A change the nest does not record stays in the
     * kept root, and the next pull's sweep re-drives it.
     */
    @VisibleForTesting
    internal fun writtenCloseListener(
        scopedId: String,
        rel: String,
        host: OnDemandSetHost,
        baseVersion: ByteArray,
    ) = ParcelFileDescriptor.OnCloseListener { error ->
        if (error != null) ShellLog.w(TAG, "written descriptor of $scopedId closed with an error: ${error.message}")
        scope.launch {
            runCatching { host.closedWrite(rel, baseVersion) }
                .onSuccess { ack ->
                    if (!ack.acked) ShellLog.i(TAG, "write not recorded yet — kept for the next sweep")
                }
                .onFailure { ShellLog.w(TAG, "closed write not ingested — kept for the next sweep: ${it.message}") }
            notifyCursors()
        }
    }

    override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String {
        val parent = parse(parentDocumentId) as? DocId.InSet
            ?: throw UnsupportedOperationException("documents are created inside a set")
        if (mimeType == DocumentsContract.Document.MIME_TYPE_DIR) {
            // A directory is only its files' paths: an empty one has no row
            // and could not be kept.
            throw UnsupportedOperationException("an empty directory cannot be created")
        }
        refuseReadOnly(parent.scopedId)
        val host = hostFor(parent)
        val rel = runBlocking { uniqueRel(host, parent.rel, validName(displayName)) }
        val ack = hostCall("create") { host.createDocument(rel) }
        if (ack.excluded) throw IllegalArgumentException("not synced: $displayName is an ignored name")
        notifyCursors()
        return "${parent.scopedId}/$rel"
    }

    override fun deleteDocument(documentId: String) {
        val id = parse(documentId) as? DocId.InSet
        if (id == null || id.rel.isEmpty()) throw UnsupportedOperationException("a set is not deleted here")
        refuseReadOnly(id.scopedId)
        val host = hostFor(id)
        try {
            hostCall("delete") { host.deleteDocument(id.rel) }
        } finally {
            // A directory's walk may have deleted some files before it stopped.
            notifyCursors()
        }
    }

    override fun renameDocument(documentId: String, displayName: String): String? {
        val id = parse(documentId) as? DocId.InSet
        if (id == null || id.rel.isEmpty()) throw UnsupportedOperationException("a set is not renamed here")
        val parentRel = id.rel.substringBeforeLast('/', "")
        val toRel = join(parentRel, validName(displayName))
        if (toRel == id.rel) return null
        return moveTo(id, toRel)
    }

    override fun moveDocument(
        sourceDocumentId: String,
        sourceParentDocumentId: String,
        targetParentDocumentId: String,
    ): String {
        val id = parse(sourceDocumentId) as? DocId.InSet
        if (id == null || id.rel.isEmpty()) throw UnsupportedOperationException("a set is not moved here")
        val target = parse(targetParentDocumentId) as? DocId.InSet
            ?: throw UnsupportedOperationException("documents are moved inside a set")
        if (target.scopedId != id.scopedId) {
            throw UnsupportedOperationException("a document is moved within its own set")
        }
        return moveTo(id, join(target.rel, id.rel.substringAfterLast('/')))
    }

    /** Rename or move within one set, record-first; returns the document's new id. */
    private fun moveTo(id: DocId.InSet, toRel: String): String {
        refuseReadOnly(id.scopedId)
        val host = hostFor(id)
        try {
            val ack = hostCall("rename") { host.renameDocument(id.rel, toRel) }
            if (ack.excluded) throw IllegalArgumentException("not synced: the new name is ignored")
        } finally {
            notifyCursors()
        }
        return "${id.scopedId}/$toRel"
    }

    /** Run one host write; a failure (unrecordable, existing, ignored) reaches the caller. */
    private fun <T> hostCall(what: String, call: suspend () -> T): T = try {
        runBlocking(Dispatchers.IO) { call() }
    } catch (e: CancellationException) {
        throw OperationCanceledException("$what cancelled")
    } catch (e: Exception) {
        throw IllegalStateException("$what not recorded: ${e.message}", e)
    }

    /** SAF's collision convention: `name`, then `name (1).ext`, `name (2).ext`, … */
    private suspend fun uniqueRel(host: OnDemandSetHost, parentRel: String, name: String): String {
        val dot = name.lastIndexOf('.')
        val base = if (dot > 0) name.substring(0, dot) else name
        val ext = if (dot > 0) name.substring(dot) else ""
        var candidate = join(parentRel, name)
        var n = 1
        while (host.item(candidate) != null) {
            candidate = join(parentRel, "$base ($n)$ext")
            n++
        }
        return candidate
    }

    private fun validName(displayName: String): String {
        if (displayName.isEmpty() || displayName == "." || displayName == ".." || displayName.contains('/')) {
            throw IllegalArgumentException("not a valid name: $displayName")
        }
        return displayName
    }

    private fun join(parentRel: String, name: String) = if (parentRel.isEmpty()) name else "$parentRel/$name"

    override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean =
        when (parse(parentDocumentId)) {
            DocId.Root -> documentId != ROOT_DOCUMENT_ID && parse(documentId) is DocId.InSet
            is DocId.InSet -> documentId.startsWith("$parentDocumentId/")
        }

    override fun shutdown() {
        scope.coroutineContext[kotlinx.coroutines.Job]?.cancel()
        super.shutdown()
    }

    /**
     * Parse a document id against the ACTIVE account: `<ref-component>@<actor-hex>`
     * is a set, `<ref-component>@<actor-hex>/<rel>` a path in it. Anything else —
     * including an id another account's grant still holds — is not found.
     */
    private fun parse(documentId: String): DocId {
        if (documentId == ROOT_DOCUMENT_ID) return DocId.Root
        val actor = source().activeActorHex() ?: throw FileNotFoundException("signed out")
        val scopedId = documentId.substringBefore('/')
        val rel = documentId.substringAfter('/', "")
        if (!scopedId.endsWith("@$actor") || scopedId.length == actor.length + 1) {
            throw FileNotFoundException("not this account's document: $documentId")
        }
        return DocId.InSet(scopedId, rel)
    }

    private fun hostFor(id: DocId.InSet): OnDemandSetHost =
        runBlocking { source().host(id.scopedId) }
            ?: throw FileNotFoundException("no such set: ${id.scopedId}")

    /**
     * Whether the account holds the set as a reader. A set the plan does not
     * name reads as read-only — nothing is advertised for a set we cannot see.
     */
    private fun readOnly(scopedId: String): Boolean =
        runBlocking { source().sets(refresh = false) }.firstOrNull { it.scopedId == scopedId }?.readOnly ?: true

    private fun refuseReadOnly(scopedId: String) {
        if (readOnly(scopedId)) throw UnsupportedOperationException("this folder is shared with you read-only")
    }

    /** A set's own row: documents are created in it unless the account only reads it. */
    private fun setFlags(set: OnDemandSet) = if (set.readOnly) 0 else SET_FLAGS

    private fun addDirRow(cursor: MatrixCursor, documentId: String, name: String, flags: Int) {
        cursor.newRow().apply {
            add(DocumentsContract.Document.COLUMN_DOCUMENT_ID, documentId)
            add(DocumentsContract.Document.COLUMN_MIME_TYPE, DocumentsContract.Document.MIME_TYPE_DIR)
            add(DocumentsContract.Document.COLUMN_DISPLAY_NAME, name)
            add(DocumentsContract.Document.COLUMN_LAST_MODIFIED, null)
            add(DocumentsContract.Document.COLUMN_FLAGS, flags)
            add(DocumentsContract.Document.COLUMN_SIZE, null)
        }
    }

    private fun addItemRow(cursor: MatrixCursor, scopedId: String, item: OnDemandItem, readOnly: Boolean) {
        if (item.isDir) {
            addDirRow(cursor, "$scopedId/${item.rel}", item.name, if (readOnly) 0 else DIR_FLAGS)
            return
        }
        cursor.newRow().apply {
            add(DocumentsContract.Document.COLUMN_DOCUMENT_ID, "$scopedId/${item.rel}")
            add(DocumentsContract.Document.COLUMN_MIME_TYPE, mimeTypeOf(item.name))
            add(DocumentsContract.Document.COLUMN_DISPLAY_NAME, item.name)
            add(DocumentsContract.Document.COLUMN_LAST_MODIFIED, item.mtime * 1000L)
            // No thumbnail flag: browsing a folder never hydrates.
            add(DocumentsContract.Document.COLUMN_FLAGS, if (readOnly) 0 else FILE_FLAGS)
            add(DocumentsContract.Document.COLUMN_SIZE, item.sizeBytes)
        }
    }

    private fun mimeTypeOf(name: String): String {
        val ext = name.substringAfterLast('.', "").lowercase()
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext) ?: "application/octet-stream"
    }

    /** Pull one set in the background (at most one pull in flight per set); notify on a change. */
    private fun refreshAsync(scopedId: String, host: OnDemandSetHost) {
        if (!refreshing.add(scopedId)) return
        scope.launch {
            try {
                if (pull(host)) notifyCursors()
            } finally {
                refreshing.remove(scopedId)
            }
        }
    }

    /**
     * One pull: the nest's changes, then the kept-root sweep — every write
     * that did not record (closed offline, or its process killed before the
     * upload) is re-driven, and evictions are observed. True when anything moved.
     */
    private suspend fun pull(host: OnDemandSetHost): Boolean {
        val changed = runCatching { host.refresh() }
            .onFailure { ShellLog.w(TAG, "refresh failed: ${it.message}") }
            .getOrDefault(false)
        val swept = runCatching { host.sweepKeptRoot() }
            .onFailure { ShellLog.w(TAG, "kept-root sweep failed: ${it.message}") }
            .getOrNull()
        return changed || !swept?.recorded.isNullOrEmpty() || !swept?.evicted.isNullOrEmpty()
    }

    /** The refresh tick, on the shared reconcile cadence, while the process lives. */
    private fun startTick(source: OnDemandSource) {
        if (tickStarted) return
        tickStarted = true
        val periodMs = runCatching { com.fauna.ffi.defaultRescanIntervalSecs().toLong() * 1000L }
            .getOrDefault(300_000L)
        scope.launch {
            while (isActive) {
                delay(periodMs)
                var moved = false
                for (host in source.liveHosts().values) moved = pull(host) or moved
                if (moved) notifyCursors()
            }
        }
    }

    /**
     * Notify every cursor this provider handed out: a notification on the
     * authority's base URI reaches the observers of every document URI under it.
     */
    private fun notifyCursors() = notify(Uri.Builder().scheme("content").authority(authority).build())

    private fun notify(uri: Uri) {
        context?.contentResolver?.notifyChange(uri, null)
    }
}
