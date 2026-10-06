package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_backups_machine.BackupsMachine
import uniffi.fauna_backups_machine.BackupsObserver
import uniffi.fauna_backups_machine.BackupsSnapshot
import javax.inject.Inject

/**
 * The Backups page's **snapshot half**, rendered off the shared
 * [BackupsMachine] (`libs/fauna-backups-machine` over the UniFFI face
 * `ApiClient.buildBackupsMachine`). Architectural rule 1 binds from the
 * machine's landing: this VM builds the machine, republishes its snapshot, and
 * forwards gestures — it holds no page logic (`ui/backups.md` § Snapshot-list
 * shape).
 *
 * What the machine adoption retired here, each an item on this app's
 * *Reconciliation ledger* row:
 *
 *  * **The Room snapshot cache.** `SnapshotDao` had no delete, so a pruned or
 *    deleted snapshot stayed listed forever and could drive a stale
 *    `last-backed-up`. The machine's list IS the state now — there is no second
 *    copy to evict, which is why the whole `snapshots` table went with it.
 *  * **The two hardcoded-English integrity strings** ("Integrity check
 *    passed." / "Integrity check found errors: …") — the verdict is the shared
 *    `is_ok` predicate rendered from i18n on the screen's own result surface.
 *  * **Successes in the error banner.** The prune result and the check verdict
 *    both wrote `errorMessage`; a completed check is a RESULT, not an error
 *    (Architectural rule 6), and prune is now preview-first.
 *  * **The client-supplied `keep_last = 3` prune.** The button opens a dry-run
 *    preview over the set's own resting retention policy and execute is offered
 *    only from that preview (Architectural rule 5 — this page never supplies a
 *    policy).
 *  * **The ad-hoc `isCreating` / `isLoading` / `immediateDeleting` flags** —
 *    one single-flight `in_progress_op` gates every mutating control.
 */
@HiltViewModel
class BackupsVM @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage
) : ViewModel() {

    private val _snapshot = MutableStateFlow<BackupsSnapshot?>(null)
    val snapshot: StateFlow<BackupsSnapshot?> = _snapshot.asStateFlow()

    private val observer = object : BackupsObserver {
        override fun onChanged() {
            _snapshot.value = machine?.snapshot()
            closeImmediateDeleteModalIfLanded()
        }
    }

    private var machine: BackupsMachine? = null

    /**
     * Build the machine over the current connection if not already built, then
     * refresh. A null return (nest not yet connected) leaves an empty page; the
     * next [refresh] retries the build (mirrors [DevicesVM]).
     */
    fun start() = refresh()

    fun refresh() {
        val m = ensureMachine() ?: return
        viewModelScope.launch { m.refresh() }
    }

    fun selectFolder(name: String) {
        val m = machine ?: return
        viewModelScope.launch { m.selectFolder(name) }
    }

    fun createSnapshot() {
        val m = machine ?: return
        viewModelScope.launch { m.createSnapshot() }
    }

    fun deleteSnapshot(id: Long) {
        val m = machine ?: return
        viewModelScope.launch { m.deleteSnapshot(id) }
    }

    /** Recovery is offered ONLY out of `SoftDeleted` (backups.md § *Soft-deleted
     *  rows*) — the machine refuses the gesture for any other row, so the render
     *  guard is the affordance rule, never the enforcement. */
    fun undeleteSnapshot(id: Long) {
        val m = machine ?: return
        viewModelScope.launch { m.undeleteSnapshot(id) }
    }

    /** Open one snapshot's file list (`snapshot-detail-files`) — the
     *  custody-wired sealed-plane read the machine owns. */
    fun openSnapshot(id: Long) {
        val m = machine ?: return
        viewModelScope.launch { m.openSnapshot(id) }
    }

    fun closeSnapshotDetail() {
        machine?.closeSnapshotDetail()
        _snapshot.value = machine?.snapshot()
    }

    /** Prune OPENS the dry-run preview; nothing is deleted until the user
     *  executes from it. */
    fun prunePreview() {
        val m = machine ?: return
        viewModelScope.launch { m.prunePreview() }
    }

    fun pruneExecute() {
        val m = machine ?: return
        viewModelScope.launch { m.pruneExecute() }
    }

    fun cancelPrunePreview() {
        machine?.cancelPrunePreview()
        _snapshot.value = machine?.snapshot()
    }

    /** Clicking the tagged button RUNS the check — no second confirm step. */
    fun checkIntegrity() {
        val m = machine ?: return
        viewModelScope.launch { m.check() }
    }

    /**
     * The single-file download's own failure text. Deliberately NOT the
     * machine's `error`: the download is app glue (the shared byte walk plus
     * this platform's save/share step), so a failure in it is this app's to
     * report — folding it into the machine's slot would let the next machine
     * tick silently clear it.
     */
    private val _downloadError = MutableStateFlow<String?>(null)
    val downloadError: StateFlow<String?> = _downloadError.asStateFlow()

    /** Single-file restore (`snapshot-file-download-button[i]`): fetch one
     * file's decrypted bytes via the shared client-side walk. Null on a missing
     * device id or a fetch failure (surfaced through [downloadError]); the
     * caller (the screen) does the platform-native save step. */
    suspend fun downloadSnapshotFile(snapshotId: Int, path: String): ByteArray? {
        val deviceIdHex = secureStorage.deviceId ?: return null
        return try {
            api.downloadSnapshotFileBytes(deviceIdHex, snapshotId, path).also {
                _downloadError.value = null
            }
        } catch (e: Exception) {
            _downloadError.value = e.message
            null
        }
    }

    /** Report a save-step failure the screen hit after the bytes arrived. */
    fun reportDownloadError(message: String?) {
        _downloadError.value = message
    }

    // ── Immediate-delete modal (backups.md § User actions, Architectural rule 4)
    // The owner-only `delete_immediate` override. NEVER a one-click affordance:
    // the confirm binds the MACHINE's predicate, which threads the real in-flight
    // flag (the half every app got wrong by hard-coding it).
    private val _immediateDeleteTargetId = MutableStateFlow<Long?>(null)
    val immediateDeleteTargetId: StateFlow<Long?> = _immediateDeleteTargetId.asStateFlow()

    /** The exact acknowledge phrase, pinned to the protocol constant the nest
     *  checks byte-for-byte, so the friction bar can't drift from the server's
     *  `acknowledge_mismatch` gate. */
    fun immediateDeleteAckText(): String = api.immediateDeleteAckText()

    /** `immediate-delete-confirm-button`'s enabled flag — the machine's, with its
     *  real in-flight state. Never re-derived here. */
    fun immediateDeleteEnabled(confirmId: String, targetId: String, acknowledge: String): Boolean =
        machine?.immediateDeleteEnabled(confirmId, targetId, acknowledge) ?: false

    fun openImmediateDelete(id: Long) {
        _immediateDeleteTargetId.value = id
    }

    fun cancelImmediateDelete() {
        _immediateDeleteTargetId.value = null
    }

    fun deleteSnapshotImmediate(id: Long, confirmId: String, acknowledge: String) {
        val m = machine ?: return
        // The modal deliberately stays open across the call: [closeImmediateDeleteModalIfLanded]
        // closes it when the row leaves the machine's list, so the nest's
        // `hard_floor_breach` refusal leaves the friction bar and its typed inputs
        // standing for a retry.
        viewModelScope.launch { m.deleteSnapshotImmediate(id, confirmId, acknowledge) }
    }

    /**
     * Close the friction-bar modal once the row it targets has actually left the
     * machine's list — the delete *landed*.
     *
     * Deliberately keyed on the row's disappearance rather than on a returning
     * call: a refusal returns from the same call and must leave the modal
     * standing. An in-flight op or a live error both mean "not landed", so
     * neither closes it.
     */
    private fun closeImmediateDeleteModalIfLanded() {
        val target = _immediateDeleteTargetId.value ?: return
        val snap = _snapshot.value ?: return
        if (snap.inProgressOp != null || snap.error != null) return
        if (snap.snapshots.any { it.id == target }) return
        _immediateDeleteTargetId.value = null
    }

    private fun ensureMachine(): BackupsMachine? {
        machine?.let { return it }
        val built = api.buildBackupsMachine(observer, secureStorage.deviceId) ?: return null
        machine = built
        return built
    }
}
