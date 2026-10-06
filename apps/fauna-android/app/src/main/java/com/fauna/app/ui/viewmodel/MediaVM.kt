package com.fauna.app.ui.viewmodel

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.fauna_media_machine.FileVersionSummary
import uniffi.fauna_media_machine.MediaItemSummary
import uniffi.fauna_media_machine.MediaMachine
import uniffi.fauna_media_machine.MediaObserver
import uniffi.fauna_media_machine.MediaPageSnapshot
import javax.inject.Inject

/**
 * View-model for the Media page (`docs/goal/ui/media.md`) — the unified cross-set
 * Windows-Explorer view over the media inside the user's folders. Holds the
 * shared page-level [MediaMachine] (built over the session's WS-RPC connection via
 * [ApiClient.buildMediaMachine]) and renders the whole page off its
 * [MediaPageSnapshot] — the cross-set item list plus the client-held
 * `media-view-toggle` / `media-sort-select` / `media-folder-filter` view state.
 * **No page logic client-side**: every read is a snapshot getter and every gesture
 * forwards to the machine (priority #2, observer-driven rendering per the doc's
 * § Architectural rules). Mirrors [DevicesVM]'s observer→state pattern (and the
 * linux LEAD `apps/fauna-linux/src/views/media/mod.rs`).
 *
 * Media is the **content plane only** — it reads folders, it never configures
 * them (rule 4); all folder config lives in Settings → Folders ([DevicesVM] /
 * `FoldersScreen`), not here.
 */
@HiltViewModel
class MediaVM @Inject constructor(
    savedStateHandle: SavedStateHandle,
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    @ApplicationContext private val appContext: Context,
) : ViewModel() {

    private val _snapshot = MutableStateFlow<MediaPageSnapshot?>(null)
    /** The whole renderable Media page; null until the machine is built + first refresh. */
    val snapshot: StateFlow<MediaPageSnapshot?> = _snapshot

    private val observer = object : MediaObserver {
        override fun onChanged() {
            _snapshot.value = machine?.snapshot()
        }
    }

    private var machine: MediaMachine? = null

    // Search deep link (`SearchNav.File` — search.md § Where logic lives →
    // *Result navigation (deep link)*; `apps/fauna-tui/src/media/mod.rs`
    // `Action::OpenFile` is the lead-app reference), reached via the
    // `media/file/{openFolderId}/{openPathHash}` route (`FaunaNavHost.kt`).
    // Consumed exactly once, by the next [refresh] — `locateFile` reads the
    // machine's own in-memory aggregate, so it is only meaningful AFTER a
    // `fauna.media.list` round trip has populated it.
    private var pendingDeepLink: Pair<Long, String>? =
        savedStateHandle.get<String>("openFolderId")?.toLongOrNull()?.let { folderId ->
            savedStateHandle.get<String>("openPathHash")?.let { pathHash -> folderId to pathHash }
        }

    private val _deepLinkedItem = MutableStateFlow<MediaItemSummary?>(null)
    /** The item a `SearchNav.File` located — opened via its own `media-item-detail`
     *  surface, independent of the per-tile dialogs [MediaItemTile] owns, since the
     *  deep-linked item may not even be in the currently-filtered set. */
    val deepLinkedItem: StateFlow<MediaItemSummary?> = _deepLinkedItem.asStateFlow()

    /** Dismiss the deep-linked item's detail (`media-item-detail-close-button` /
     *  a successful delete — mirrors [MediaItemTile]'s own dismiss). */
    fun clearDeepLinkedItem() {
        _deepLinkedItem.value = null
    }

    init {
        // `fauna.sync.changed` — a record landed in a folder this actor
        // participates in (own other device, or a fellow member). The push is
        // a nudge: re-read the cross-set media snapshot so a collaborator's
        // add/remove appears without waiting for the next page visit
        // (file-sync.md § Remote-change nudge; same tick idiom as
        // EventsVM.calendarChangedTick).
        viewModelScope.launch {
            api.folderChangedTick.collect { refresh() }
        }
        // Reconnect backstop: a push fired while the socket was down is never
        // replayed, so a mounted Media screen must re-pull on reconnect too —
        // `StaleSurfaces::on_reconnect()` stales `media` along with every other
        // surface (`transport.md` § Which surfaces a push invalidates). This VM
        // had no reconnect arm before adopting the shared classifier (the same
        // class of gap the seam's own audit found on web's Events page).
        viewModelScope.launch {
            api.reconnectTick.collect { refresh() }
        }
    }

    /** Build the machine over the current connection if needed, then refresh. */
    fun start() {
        refresh()
    }

    fun refresh() {
        val m = ensureMachine() ?: return
        val backupKey = api.ownerBackupKey()
        viewModelScope.launch {
            m.refresh(backupKey)
            // Only meaningful once the aggregate above is populated —
            // `locate_file` reads the machine's in-memory `raw` state
            // synchronously, never fetching on its own.
            pendingDeepLink?.let { (folderId, pathHash) ->
                pendingDeepLink = null
                val item = m.locateFile(folderId, pathHash)
                if (item != null) {
                    // Point the browse at the item's own set — a deep link
                    // says nothing about the filter the user last left here
                    // (tui `Action::OpenFile`, apps/fauna-tui/src/media/mod.rs).
                    m.setFilter(item.folder)
                    _deepLinkedItem.value = item
                }
                // item == null: deleted, renamed, or in a set this seat cannot
                // read — the DROPPED outcome (search.md § State & data
                // shape), arriving one click late. The page still opens,
                // just with nothing pre-selected.
            }
        }
    }

    // ── View-state gestures (sync setters; the machine notifies the observer) ──

    /** Flip the `media-view-toggle` between list and thumbnail-grid. */
    fun toggleView() {
        val m = machine ?: return
        m.setViewGrid(!(_snapshot.value?.viewGrid ?: false))
    }

    /** Set the `media-sort-select` key (`"name"` / `"size"` / `"date"`). */
    fun setSort(key: String) {
        machine?.setSort(key)
    }

    /** Set the `media-folder-filter`: a set name, or null for the all-media default. */
    fun setFilter(folder: String?) {
        machine?.setFilter(folder)
    }

    /**
     * Enter a followed public folder's browse scope (`media.md` § Followed public
     * folders): [value] is a `FollowedScopeOption.value` from the snapshot, handed
     * back verbatim — the shared gesture fetches the listing on demand from the
     * folder's home nest and reports its own failures on the page banner.
     */
    fun selectFollowed(value: String) {
        val m = machine ?: return
        viewModelScope.launch { m.selectFollowedScope(value) }
    }

    // ── Upload / thumbnail (async; sealed + fetched in shared Rust) ────────────

    /**
     * Upload the picked file [uri] into the currently-selected set via the shared
     * `MediaMachine::upload_selected` gesture (seal → POST → record → refresh; the
     * "which set" policy lives in shared Rust). Reads the content bytes + display
     * name off the main thread; the member path is the picked file's display name.
     * Silently no-ops when there is no identity/device yet (a logged-in user always
     * has both); a missing selected set surfaces `media.error_no_set` from the
     * machine (the page `error-message` banner).
     */
    fun upload(uri: Uri) {
        val m = machine ?: return
        val deviceId = secureStorage.deviceId ?: return
        val backupKey = api.ownerBackupKey() ?: return
        viewModelScope.launch {
            val (name, bytes) = withContext(Dispatchers.IO) {
                displayName(uri) to readBytes(uri)
            }
            if (bytes == null || bytes.isEmpty()) return@launch
            m.uploadSelected(deviceId, name, bytes, backupKey)
        }
    }

    /**
     * Fetch + decrypt a `media-thumbnail` blob into paint-ready bytes via the
     * shared `MediaMachine::fetch_thumbnail` (direct-by-hash GET + owner-`BackupKey`
     * decrypt — all shared Rust). Returns null on a missing key or any per-item
     * fetch/decode error so the caller keeps the placeholder — one unreadable
     * thumbnail must never blank the page (`media.md` § Implementation status).
     */
    suspend fun fetchThumbnail(hash: String): ByteArray? {
        val m = machine ?: return null
        val backupKey = api.ownerBackupKey() ?: return null
        return runCatching { m.fetchThumbnail(hash, backupKey) }.getOrNull()
    }

    // ── media-item-detail + file-version-history (media.md § Element IDs) ──────

    /**
     * `fauna.files.versions.list` for one item, oldest→newest — the
     * `file-version-history` rows for the `media-item-detail` surface. Empty on any
     * failure (missing machine, FFI error) so the dialog just shows no rows rather
     * than crashing; a real load failure is rare (metadata-only, self-healing) and
     * not worth its own error surface here.
     *
     * `includePruned` — the `file-version-show-pruned-toggle` recovery browse
     * (`file-versions.md` § Retention (3), apps row 323): `true` also returns
     * soft-pruned rows (each carrying `pruned`), `false` is the live-only listing.
     */
    suspend fun loadVersions(folder: String, path: String, includePruned: Boolean = false): List<FileVersionSummary> {
        val m = machine ?: return emptyList()
        return runCatching { m.fileVersions(folder, path, includePruned) }.getOrElse { emptyList() }
    }

    /**
     * `file-version-undelete-button` — restore a soft-pruned version to the
     * listable population via the shared `MediaMachine::undelete_version`
     * (`file-versions.md` § Retention (3)). A per-item query like [loadVersions]:
     * the caller re-lists on success and surfaces the error on its own surface
     * (never the page `error-message` banner).
     */
    suspend fun undeleteVersion(path: String, versionNum: Long): Boolean {
        val m = machine ?: return false
        return runCatching { m.undeleteVersion(path, versionNum) }.isSuccess
    }

    /**
     * Restore [version] via the shared `MediaMachine::restore_version` gesture
     * (metadata-only re-point; file-sync.md § Restore). Like [deleteItem], the
     * machine swallows its error into the snapshot rather than throwing, so success
     * is read back from `snapshot.error` — re-published here so the page's existing
     * `error-message` banner (`MediaScreen`'s `errorText` effect) carries a failure.
     */
    suspend fun restoreVersion(folder: String, path: String, version: FileVersionSummary): Boolean {
        val m = machine ?: return false
        val deviceId = secureStorage.deviceId ?: return false
        m.restoreVersion(folder, deviceId, path, version)
        val snap = m.snapshot()
        _snapshot.value = snap
        return snap.error == null
    }

    /**
     * Delete the opened file via the shared `MediaMachine::delete` gesture (records
     * a tombstone; media.md § User actions). Same snapshot-error read-back as
     * [restoreVersion].
     */
    suspend fun deleteItem(folder: String, path: String): Boolean {
        val m = machine ?: return false
        val deviceId = secureStorage.deviceId ?: return false
        m.delete(folder, deviceId, path)
        val snap = m.snapshot()
        _snapshot.value = snap
        return snap.error == null
    }

    /**
     * `media-item-detail-download-button` — the item's plaintext, via one of the
     * two shared per-item queries (`media.md` § Element IDs): in a followed scope
     * the keyless `download_followed` (never `download_file` — a follower holds no
     * key), otherwise `download_file` keyed by [latest], the newest version row, so
     * a shared set opens under the content keys in this seat's custody and an
     * owner-only set under the owner `BackupKey`. Like [fetchThumbnail] it never
     * touches the page banner: the error comes back to the detail's own status line.
     */
    suspend fun downloadBytes(
        item: MediaItemSummary,
        latest: FileVersionSummary?,
        followedScope: String?,
    ): Result<ByteArray> {
        val m = machine ?: return Result.failure(IllegalStateException("not connected"))
        return runCatching {
            when {
                followedScope != null -> m.downloadFollowed(followedScope, item.path)
                latest != null -> {
                    val backupKey = api.ownerBackupKey() ?: error("no owner key")
                    m.downloadFile(
                        latest.manifestHash, latest.contentKeyVersion, item.folder, item.path, backupKey,
                    )
                }
                else -> error("no version to download")
            }
        }
    }

    // ── Share links (`share-links.md` § Flows) ─────────────────────────────────
    // Pure forwards: eligibility, the step state, the reveal-after-registration
    // rule, row states and errors all live in the shared machine, which notifies
    // the observer on every step; failures land in `snapshot.error` (the page
    // `error-message` banner).

    /** `share-link-button` — open the create surface on an eligible item. */
    fun openShareCreate(folder: String, path: String) {
        machine?.openShareCreate(folder, path)
    }

    /** `share-link-expiry-select` — one of `snapshot.shareExpiryOptions`. */
    fun setShareExpiry(value: String) {
        machine?.setShareExpiry(value)
    }

    /** `share-link-cancel-button` — close the create surface. */
    fun closeShareCreate() {
        machine?.closeShareCreate()
    }

    /** `share-link-create-button` — mint, register, then reveal the URL. */
    fun createShareLink() {
        val m = machine ?: return
        viewModelScope.launch { m.createShareLink() }
    }

    /** `share-link-list-button` — open and load the list. */
    fun openShareLinks() {
        val m = ensureMachine() ?: return
        viewModelScope.launch { m.openShareLinks() }
    }

    /** `share-link-list-close-button`. */
    fun closeShareLinks() {
        machine?.closeShareLinks()
    }

    /** `share-link-revoke-button` — arm the single confirm for an Active row. */
    fun armShareRevoke(tokenId: String) {
        machine?.armShareRevoke(tokenId)
    }

    /** `share-link-revoke-cancel-button`. */
    fun cancelShareRevoke() {
        machine?.cancelShareRevoke()
    }

    /** `share-link-revoke-confirm-button` — revoke the armed row, then re-list. */
    fun confirmShareRevoke() {
        val m = machine ?: return
        viewModelScope.launch { m.confirmShareRevoke() }
    }

    private fun ensureMachine(): MediaMachine? {
        machine?.let { return it }
        val built = api.buildMediaMachine(observer) ?: return null
        // Write-side label custody for the delete/restore gestures (S8 D2): the
        // same per-actor owner key the upload gesture seals with, injected once so
        // those records seal instead of resting plaintext-only. Mirrors linux/tui
        // (apps/fauna-linux/src/views/media/mod.rs).
        api.ownerBackupKey()?.let { built.setOwnerBackupKey(it) }
        // READ-side custody for a successor: the media corpus a succession
        // re-pointed is still sealed under the identities it succeeded from
        // (`succession-aftermath.md` § Re-key scope — *media*, folders,
        // backups). Deliberately a second injection rather than an extra key
        // on the line above: that one is the delete/restore **seal** root,
        // and a retired key must never reach it. Mirrors linux/tui/web/apple.
        api.predecessorBackupKeys().takeIf { it.isNotEmpty() }?.let {
            built.setPredecessorBackupKeys(it)
        }
        // …and the READER's half of the same walk: the attested predecessor ids,
        // so the listing's judge reads a row a retired identity signed as this
        // account's own (`writer-signed-change-records.md`, ruling (8)(b)).
        // Mirrors tui/linux/web.
        api.attestedPredecessorActorIds().takeIf { it.isNotEmpty() }?.let {
            built.setPredecessorActorIds(it)
        }
        // …and the keys PAIRED with those identities, replacing the bare keys
        // above: a row signed as a predecessor opens only under that
        // identity's root and its predecessors' (ruling (8)(c)).
        api.predecessorChain().takeIf { it.actorIds.isNotEmpty() }?.let {
            built.setPredecessorChain(it.actorIds, it.keys)
        }
        // Followed public folders as browse scopes (media.md § Followed public
        // folders) — without this `snapshot.followed` is permanently empty.
        api.wireMediaFollowedFolders(built)
        // The share-link author (`share-links.md` § Where logic lives): the
        // session's identity signs the token and seals its filename, and the
        // links point at this session's nest. After the predecessors, which it
        // reads — tui's and linux's order. A missing secret leaves it unwired;
        // the share gestures then report their own error.
        api.identitySecretBytes()?.let { built.setShareAuthor(it, api.nodeUrl) }
        machine = built
        return built
    }

    /** The picked content Uri's display name (the member path), best-effort. */
    private fun displayName(uri: Uri): String {
        appContext.contentResolver
            .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { cursor ->
                val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (idx >= 0 && cursor.moveToFirst()) {
                    cursor.getString(idx)?.let { if (it.isNotBlank()) return it }
                }
            }
        return uri.lastPathSegment?.substringAfterLast('/') ?: "upload"
    }

    private fun readBytes(uri: Uri): ByteArray? =
        runCatching { appContext.contentResolver.openInputStream(uri)?.use { it.readBytes() } }.getOrNull()
}
