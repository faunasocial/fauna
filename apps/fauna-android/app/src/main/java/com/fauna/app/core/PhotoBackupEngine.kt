package com.fauna.app.core

import android.content.ContentUris
import android.content.Context
import android.provider.MediaStore
import dagger.hilt.android.qualifiers.ApplicationContext
import com.fauna.app.R
import com.fauna.app.data.db.PhotoBackupDao
import com.fauna.app.data.db.PhotoBackupRecord
import com.fauna.app.data.db.SyncFileState
import com.fauna.app.ui.util.getStringFmt
import com.fauna.ffi.FfiPhotoLibrarySet
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.yield
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.UUID
import javax.inject.Inject
import javax.inject.Provider
import javax.inject.Singleton

@Singleton
class PhotoBackupEngine @Inject constructor(
    @ApplicationContext private val context: Context,
    private val apiClient: ApiClient,
    private val networkMonitor: NetworkMonitor,
    // A Provider, not the DAO itself: this is a process-wide @Singleton, so a DAO
    // captured at construction would stay bound to the account-scoped database
    // open at that moment and keep serving it after a switch closed and replaced
    // it (account-scoping.md § the switch/sign-out isolation contract). Resolving
    // per call follows AccountStores' live handle.
    private val photoBackupDaoProvider: Provider<PhotoBackupDao>,
    private val secureStorage: SecureStorage,
    private val accountStores: AccountStores
) {
    private val photoBackupDao: PhotoBackupDao get() = photoBackupDaoProvider.get()

    init {
        // The resolved photo-library set name below is the ACTIVE account's; drop
        // it when that account's session ends, or the incoming account would
        // ingest into the outgoing one's set.
        accountStores.registerCloser("photo-library-set") { photoSet = null }
    }

    companion object {
        private const val MAX_FILE_SIZE = 100L * 1024 * 1024 // 100 MB
        private const val SNAPSHOT_INTERVAL = 50
    }

    private val _totalScanned = MutableStateFlow(0)
    val totalScanned: StateFlow<Int> = _totalScanned

    private val _uploadedCount = MutableStateFlow(0)
    val uploadedCount: StateFlow<Int> = _uploadedCount

    private val _currentFile = MutableStateFlow<String?>(null)
    val currentFile: StateFlow<String?> = _currentFile

    private val _isSyncing = MutableStateFlow(false)
    val isSyncing: StateFlow<Boolean> = _isSyncing

    private val _lastError = MutableStateFlow<String?>(null)
    val lastError: StateFlow<String?> = _lastError

    private val dateFormat = SimpleDateFormat("yyyy/MM", Locale.US)

    /**
     * The resolved photo-library set — its identity (`folderId`, the key the
     * ingest takes) and its name (the label the snapshot call takes) — cached
     * for the process life. Resolution hits the nest (it may create the set),
     * so it's done once and lazily — mirrors apple `PhotoBackupEngine.swift`'s
     * cached `photoSet`.
     */
    private var photoSet: FfiPhotoLibrarySet? = null

    // The sync engine's state dir (folder-map.json / photo-ingress.json) — the
    // SAME directory [WatchedDirectoryManager] passes, so both ingresses share
    // one device-local state store (`FfiSyncEngineHost` is a per-actor
    // singleton keyed on this dir).
    private fun stateDir(): String = accountStores.syncStateDir()

    /**
     * The folder this device's photo ingress feeds — the wizard-created
     * "Photo Library" set (`ui/folders.md` § Photo backup →
     * *Target set model*). Must succeed before the first ingest of a
     * pass; see [ApiClient.resolvePhotoLibrarySet].
     */
    private suspend fun photoLibrarySet(): FfiPhotoLibrarySet {
        photoSet?.let { return it }
        val deviceIdHex = secureStorage.deviceId
            ?: throw IllegalStateException("Photo backup is not configured (no device id)")
        val set = apiClient.resolvePhotoLibrarySet(deviceIdHex, stateDir())
        photoSet = set
        return set
    }

    suspend fun syncNewPhotos(): Int {
        if (_isSyncing.value) return 0

        _isSyncing.value = true
        _uploadedCount.value = 0
        _lastError.value = null

        try {
            val deviceIdHex = secureStorage.deviceId ?: return 0
            val host = apiClient.syncEngineHost(deviceIdHex, stateDir()) ?: return 0

            // Resolve (creating if needed) the target set BEFORE the first
            // ingest — a failure here fails the whole pass rather than
            // uploading into a set the nest doesn't have (the silent-failure
            // mode this cutover replaces: `changes.record` into the old
            // hardcoded "photos" set was rejected every time and swallowed
            // into this same error field — android photo backup never
            // persisted a photo).
            val photoSet = try {
                photoLibrarySet()
            } catch (e: Exception) {
                _lastError.value = context.getStringFmt(R.string.photo_backup_error_prepare_set, e.message)
                return 0
            }

            scanMediaStore(MediaStore.Images.Media.EXTERNAL_CONTENT_URI)
            scanMediaStore(MediaStore.Video.Media.EXTERNAL_CONTENT_URI)

            val pending = photoBackupDao.getPending()
            var uploaded = 0

            for (record in pending) {
                yield()
                if (!networkMonitor.shouldSyncPhotos()) {
                    _lastError.value = context.getString(R.string.photo_backup_error_wifi_lost)
                    break
                }

                if (record.sizeBytes > MAX_FILE_SIZE) {
                    continue
                }

                var stagedFile: File? = null
                try {
                    _currentFile.value = record.contentUri

                    val uri = android.net.Uri.parse(record.contentUri)
                    val rawBytes = context.contentResolver.openInputStream(uri)?.use {
                        it.readBytes()
                    } ?: continue
                    val bytes = ExifStripper.strip(rawBytes, record.mediaType)

                    val creationDate = Date(record.creationDate)
                    val displayName = resolveDisplayName(uri)
                    val remotePath = "${dateFormat.format(creationDate)}/$displayName"

                    // Sealed ingest: stage the stripped bytes to a temp file
                    // (the host takes a path — mirrors apple's exported-asset
                    // temp file), push it through the ordinary sealed chunk
                    // pipeline + `changes.record` custody upsert, then delete
                    // the staged copy. No watch dir, no reconcile pass, so
                    // deleting our temp cannot tombstone the ingested file.
                    val stagingDir = File(context.cacheDir, "photo-backup-staging").apply { mkdirs() }
                    val staged = File(stagingDir, UUID.randomUUID().toString())
                    stagedFile = staged
                    staged.writeBytes(bytes)
                    host.ingestFile(photoSet.folderId, staged.absolutePath, remotePath)

                    // The engine host's own SyncDb owns sync state now — this
                    // row is only the OS-asset dedup ledger (claim 8,
                    // `file-sync.md:148-160`/`:283`), so manifestHash stays
                    // NULL post-ingest (mirrors apple's `PhotoBackupRecord`).
                    photoBackupDao.markSynced(
                        id = record.mediaStoreId,
                        state = SyncFileState.SYNCED,
                        remotePath = remotePath
                    )

                    uploaded++
                    _uploadedCount.value = uploaded

                    // Control-plane call, never engine work — same resolved
                    // set as the ingest above: snapshotting a set we didn't
                    // write to is worse than either bug alone.
                    if (uploaded % SNAPSHOT_INTERVAL == 0) {
                        apiClient.createSnapshot(photoSet.name)
                    }
                } catch (e: Exception) {
                    _lastError.value = context.getStringFmt(
                        R.string.photo_backup_error_upload_item, record.contentUri, e.message
                    )
                    // Continue with next file
                } finally {
                    stagedFile?.delete()
                }
            }

            return uploaded
        } finally {
            _currentFile.value = null
            _isSyncing.value = false
        }
    }

    private suspend fun scanMediaStore(collectionUri: android.net.Uri) {
        val projection = arrayOf(
            MediaStore.MediaColumns._ID,
            MediaStore.MediaColumns.DISPLAY_NAME,
            MediaStore.MediaColumns.SIZE,
            MediaStore.MediaColumns.MIME_TYPE,
            MediaStore.MediaColumns.DATE_ADDED
        )

        val cursor = context.contentResolver.query(
            collectionUri,
            projection,
            null,
            null,
            "${MediaStore.MediaColumns.DATE_ADDED} DESC LIMIT 500"
        ) ?: return

        var scanned = 0
        cursor.use {
            val idCol = it.getColumnIndexOrThrow(MediaStore.MediaColumns._ID)
            val nameCol = it.getColumnIndexOrThrow(MediaStore.MediaColumns.DISPLAY_NAME)
            val sizeCol = it.getColumnIndexOrThrow(MediaStore.MediaColumns.SIZE)
            val mimeCol = it.getColumnIndexOrThrow(MediaStore.MediaColumns.MIME_TYPE)
            val dateCol = it.getColumnIndexOrThrow(MediaStore.MediaColumns.DATE_ADDED)

            while (it.moveToNext()) {
                val id = it.getLong(idCol)
                val name = it.getString(nameCol) ?: "unknown"
                val size = it.getLong(sizeCol)
                val mime = it.getString(mimeCol) ?: "application/octet-stream"
                val dateAdded = it.getLong(dateCol)

                val contentUri = ContentUris.withAppendedId(collectionUri, id).toString()
                val creationDate = dateAdded * 1000L // MediaStore DATE_ADDED is in seconds

                val record = PhotoBackupRecord(
                    mediaStoreId = id,
                    contentUri = contentUri,
                    sizeBytes = size,
                    mediaType = mime,
                    creationDate = creationDate,
                    state = SyncFileState.LOCAL_ONLY
                )

                // insert uses IGNORE so duplicates are silently skipped
                photoBackupDao.insert(record)

                scanned++
            }
        }

        _totalScanned.value += scanned
    }

    private fun resolveDisplayName(uri: android.net.Uri): String {
        val projection = arrayOf(MediaStore.MediaColumns.DISPLAY_NAME)
        context.contentResolver.query(uri, projection, null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                return cursor.getString(0) ?: "unknown"
            }
        }
        return uri.lastPathSegment ?: "unknown"
    }
}
