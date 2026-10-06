package com.fauna.app.core

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import dagger.hilt.android.qualifiers.ApplicationContext
import com.fauna.app.R
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.data.db.SyncFile
import com.fauna.app.data.db.SyncFileDao
import com.fauna.app.data.db.SyncFileState
import com.fauna.app.data.db.WatchedDirectory
import com.fauna.app.data.db.WatchedDirectoryDao
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import java.io.File
import java.util.UUID
import javax.inject.Inject
import javax.inject.Provider
import javax.inject.Singleton

@Singleton
class WatchedDirectoryManager @Inject constructor(
    @ApplicationContext private val context: Context,
    private val api: ApiClient,
    // Providers, not the DAOs themselves — see PhotoBackupEngine: a @Singleton
    // that captures a DAO keeps serving the database open at its construction,
    // across a switch that replaced it.
    private val syncFileDaoProvider: Provider<SyncFileDao>,
    private val watchedDirectoryDaoProvider: Provider<WatchedDirectoryDao>,
    private val secureStorage: SecureStorage,
    private val accountStores: AccountStores
) {
    private val syncFileDao: SyncFileDao get() = syncFileDaoProvider.get()
    private val watchedDirectoryDao: WatchedDirectoryDao
        get() = watchedDirectoryDaoProvider.get()

    companion object {
        private const val MAX_FILE_SIZE = 100L * 1024 * 1024 // 100 MB
    }

    val isScanning = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)

    // The sync engine's state dir — the SAME directory [PhotoBackupEngine]
    // passes, so both ingresses share one device-local state store
    // (`FfiSyncEngineHost` is a per-actor singleton keyed on this dir).
    private fun stateDir(): String = accountStores.syncStateDir()

    suspend fun scanDirectory(dir: WatchedDirectory): Int {
        val deviceIdHex = secureStorage.deviceId ?: return 0
        val host = api.syncEngineHost(deviceIdHex, stateDir()) ?: return 0
        // The set this directory ingests into, by identity — the row's only
        // key, so a rename of the set does not touch it.
        val folderId = dir.folderId

        val treeUri = Uri.parse(dir.treeUri)
        val treeDocId = DocumentsContract.getTreeDocumentId(treeUri)
        val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, treeDocId)

        val projection = arrayOf(
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_SIZE,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_LAST_MODIFIED,
            DocumentsContract.Document.COLUMN_DOCUMENT_ID
        )

        val cursor = context.contentResolver.query(
            childrenUri,
            projection,
            null,
            null,
            null
        ) ?: return 0

        var uploaded = 0

        cursor.use {
            val nameCol = it.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DISPLAY_NAME)
            val sizeCol = it.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_SIZE)
            val mimeCol = it.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_MIME_TYPE)
            val docIdCol = it.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DOCUMENT_ID)

            while (it.moveToNext()) {
                val displayName = it.getString(nameCol) ?: continue
                val size = it.getLong(sizeCol)
                val mimeType = it.getString(mimeCol) ?: "application/octet-stream"
                val documentId = it.getString(docIdCol) ?: continue

                // Skip directories
                if (mimeType == DocumentsContract.Document.MIME_TYPE_DIR) continue

                val syncPath = "${dir.displayName}/$displayName"

                // Skip if already synced
                val existing = syncFileDao.getByPath(syncPath)
                if (existing != null) continue

                // Build the document URI for reading
                val docUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, documentId)

                // Read bytes
                val bytes = try {
                    context.contentResolver.openInputStream(docUri)?.use { stream ->
                        stream.readBytes()
                    } ?: continue
                } catch (e: Exception) {
                    errorMessage.value = context.getStringFmt(
                        R.string.media_watched_error_read, displayName, e.message
                    )
                    continue
                }

                // Skip files larger than 100MB
                if (bytes.size > MAX_FILE_SIZE) continue

                var stagedFile: File? = null
                try {
                    // Strip EXIF metadata from images
                    val strippedBytes = ExifStripper.strip(bytes, mimeType)

                    // Sealed ingest: stage the stripped bytes to a temp file
                    // (the host takes a path, mirroring the photo-library
                    // ingress), push through the ordinary sealed chunk
                    // pipeline + `changes.record` custody upsert, then delete
                    // the staged copy. `ingest_file` fails closed on an
                    // indeterminate content-key binding (a bound set refuses
                    // rather than uploading plaintext) — surfaced below, never
                    // silently degraded.
                    val stagingDir = File(context.cacheDir, "watched-dir-staging").apply { mkdirs() }
                    val staged = File(stagingDir, UUID.randomUUID().toString())
                    stagedFile = staged
                    staged.writeBytes(strippedBytes)
                    host.ingestFile(folderId, staged.absolutePath, syncPath)

                    // Upsert SyncFile as SYNCED — this row is now dedup-by-
                    // existence bookkeeping only (the engine host's own SyncDb
                    // owns real sync state); manifestHash has no value
                    // post-ingest, so the column is left empty.
                    syncFileDao.upsert(
                        SyncFile(
                            path = syncPath,
                            folder = dir.folder,
                            sizeBytes = strippedBytes.size.toLong(),
                            manifestHash = "",
                            state = SyncFileState.SYNCED,
                            updatedAt = System.currentTimeMillis()
                        )
                    )

                    uploaded++
                } catch (e: Exception) {
                    errorMessage.value = context.getStringFmt(
                        R.string.media_watched_error_upload, displayName, e.message
                    )
                    // Continue with next file
                } finally {
                    stagedFile?.delete()
                }
            }
        }

        return uploaded
    }

    suspend fun scanAllEnabled(): Int {
        if (isScanning.value) return 0

        isScanning.value = true
        errorMessage.value = null

        try {
            val directories = watchedDirectoryDao.getEnabled().first()
            var total = 0
            for (dir in directories) {
                total += scanDirectory(dir)
            }
            return total
        } catch (e: Exception) {
            errorMessage.value = e.message
            return 0
        } finally {
            isScanning.value = false
        }
    }
}
