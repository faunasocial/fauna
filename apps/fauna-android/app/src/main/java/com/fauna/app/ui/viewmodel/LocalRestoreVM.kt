package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiSnapshotSummary
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/** Render-time progress of the local restore (backups.md § Restore — `restore-progress`). */
enum class RestoreProgress { IDLE, RUNNING, DONE }

/**
 * Drives the local-restore action card on the Backups page
 * (`docs/goal/ui/backups.md` § Restore from backup destination → the
 * disaster-recovery / local-restore path). The wireable case today is the
 * **local** restore: pick a message-kind snapshot, re-type its id (the friction
 * bar), and dispatch `fauna.filesync.snapshot.restore_message_kind` once.
 *
 * The cross-location `restore-source-select` (backup-destination picker) is
 * disabled at zero destinations and the cross-location chunk pull is blocked on
 * fauna-sync Plan 4 (backups.md § Impl-status), so this VM only exposes whether
 * destinations exist — the local snapshot picker is the wired source.
 *
 * All composition lives in the shared `fauna-client-snapshots` crate reached via
 * [ApiClient]'s `FfiSnapshotsClient` seam (priority #2); the FFI calls live here
 * so the Compose Content stays Robolectric-safe. Mirrors the linux lead
 * (apps/fauna-linux/src/views/backups/restore.rs `build_local_restore_action`).
 */
@HiltViewModel
class LocalRestoreVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    /** Owner-implicit message-kind snapshots feeding `restore-snapshot-select`. */
    private val _snapshots = MutableStateFlow<List<FfiSnapshotSummary>>(emptyList())
    val snapshots: StateFlow<List<FfiSnapshotSummary>> = _snapshots.asStateFlow()

    /** Whether ≥1 backup destination is configured — gates `restore-source-select`. */
    private val _hasDestinations = MutableStateFlow(false)
    val hasDestinations: StateFlow<Boolean> = _hasDestinations.asStateFlow()

    private val _progress = MutableStateFlow(RestoreProgress.IDLE)
    val progress: StateFlow<RestoreProgress> = _progress.asStateFlow()

    /** `restore-warning`: the last restore's reply said `config_present == false`
     *  (the bridge can't sign in after restart until the account's configuration
     *  is restored too). The reply is the advisory's only carrier; the text is
     *  the shared i18n string, never the reply's diagnostic `note`. */
    private val _configAbsent = MutableStateFlow(false)
    val configAbsent: StateFlow<Boolean> = _configAbsent.asStateFlow()

    val errorMessage = MutableStateFlow<String?>(null)

    fun refresh() {
        // Arriving on the page is not the end of a restore: the progress line
        // returns to its idle prompt and the one-shot advisory goes with it.
        _progress.value = RestoreProgress.IDLE
        _configAbsent.value = false
        viewModelScope.launch {
            // Each read degrades independently — a missing destination list (e.g.
            // no backup nest yet) must not blank the local snapshot picker.
            _snapshots.value = runCatching { api.listMessageKindSnapshots() }
                .getOrElse {
                    errorMessage.value = it.message
                    emptyList()
                }
            _hasDestinations.value = runCatching { api.backupDestinationsList().isNotEmpty() }
                .getOrDefault(false)
        }
    }

    /** Dispatch the local single-snapshot restore. The friction bar (typed id ==
     *  selected snapshot id) is enforced in the Content; this trusts it. */
    fun restore(snapshotId: Long, confirmId: String) {
        viewModelScope.launch {
            _progress.value = RestoreProgress.RUNNING
            _configAbsent.value = false
            try {
                val reply = api.restoreMessageKind(snapshotId, confirmId)
                // Decided before DONE is published, so a reader that sees DONE
                // sees the advisory's final verdict.
                _configAbsent.value = !reply.configPresent
                _progress.value = RestoreProgress.DONE
            } catch (e: Exception) {
                errorMessage.value = e.message
                _progress.value = RestoreProgress.IDLE
            }
        }
    }
}
