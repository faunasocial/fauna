package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ApiException
import com.fauna.app.core.SecureStorage
import com.fauna.app.ui.util.loadFailureText
import com.fauna.ffi.FfiBackupDestinationStatus
import com.fauna.ffi.FfiBackupDestinationView
import com.fauna.ffi.FfiDestinationAuditRow
import com.fauna.ffi.FfiReseedResult
import com.fauna.ffi.destinationRowIsAClientDevice
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * Backup-destination management section on the Backups page
 * (`docs/goal/ui/backups.md` § Manage backup destinations, ratified 2026-06-14).
 *
 * Pure glue over the shared `backup_destinations_*` FFI (config read + the
 * resolve→add / edit / remove-and-save sequencing the desktop apps drive
 * directly) — priority #2/#3, no logic here. Mirrors the Linux lead
 * (apps/fauna-linux/src/views/backups/destinations.rs).
 *
 * The per-row LIVE status (`backup-destination-last-upload-time` /
 * `-backlog-count`) is read from the NEST's `fauna.backup.status` projection over
 * the gated FFI fn `backup_destination_status` (via
 * [ApiClient.loadBackupDestinationStatus]) and rebuilt on page mount + after every
 * add/edit/remove, uniform with linux/apple/windows (backups.md
 * § Per-destination status read, repointed 2026-07-24). Until a per-app always-on
 * upload coordinator runs, `last_upload_time` is `None` ("never") while
 * `backlog_count` is a live count of un-uploaded segments. No device id or backup
 * dir travels with the read any more — the nest derives the owner from the
 * authenticated connection.
 */
@HiltViewModel
class BackupDestinationsVM @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    private val accountStores: AccountStores,
) : ViewModel() {

    private val _destinations = MutableStateFlow<List<FfiBackupDestinationView>>(emptyList())
    val destinations: StateFlow<List<FfiBackupDestinationView>> = _destinations.asStateFlow()

    /**
     * LIVE per-destination status keyed by `destination_id` — read from the shared
     * `BackupCoordinator::destination_status()` over the FFI fn. A destination with
     * no entry (read not yet done, or it degraded on a transient socket hiccup)
     * renders the not-yet-backed-up baseline ("never" / "0 queued"), uniform with
     * linux/apple.
     */
    private val _statuses = MutableStateFlow<Map<String, FfiBackupDestinationStatus>>(emptyMap())
    val statuses: StateFlow<Map<String, FfiBackupDestinationStatus>> = _statuses.asStateFlow()

    /**
     * The client's own audit picture, keyed by `destination_id` — read from
     * [ApiClient.backupAuditRunPass] (`docs/goal/ui/backups.md` § Audit-alert
     * surface). A destination with no entry has never been audited this
     * process ⇒ the row renders "never" and no banner, same baseline as a
     * fresh enrollment. Populated by [triggerAudit], which is deliberately a
     * separate round trip from [refresh]/[refreshStatuses] — see there.
     */
    private val _auditRows = MutableStateFlow<Map<String, FfiDestinationAuditRow>>(emptyMap())
    val auditRows: StateFlow<Map<String, FfiDestinationAuditRow>> = _auditRows.asStateFlow()

    /**
     * Bytes this device is holding with **no** destination row claiming them, or
     * `null` when the `backup-orphaned-store-row` must not paint at all
     * (`backups.md` § Manage backup destinations → *Reclaim this device's copy*).
     *
     * ⚠ **The verdict is reached in the op, never on a render path** — tui's and
     * linux's op-timing rule, and it is not stylistic: deciding it needs this
     * device's sync id plus a disk walk, and a composable that recomputed it per
     * frame would run a filesystem measurement inside recomposition. So it is
     * refreshed on mount, and after every mutation, by [refreshOrphanedStore],
     * and the view only reads this cached answer.
     *
     * `null` also covers every refusal: no device id, an empty store, a failed
     * read. Each is the conservative direction — the row arms a destructive
     * gesture over the owner's only offline copy, so *cannot tell* must never
     * paint it.
     */
    private val _orphanedStoreBytes = MutableStateFlow<ULong?>(null)
    val orphanedStoreBytes: StateFlow<ULong?> = _orphanedStoreBytes.asStateFlow()

    /**
     * A restore (re-seed) is running: every `backup-destination-reseed-button`
     * is disarmed until it ends, so one tap cannot start two ceremonies, and
     * `backup-destination-reseed-result` carries the running line.
     */
    private val _reseedRunning = MutableStateFlow(false)
    val reseedRunning: StateFlow<Boolean> = _reseedRunning.asStateFlow()

    /**
     * How the last restore ended, or `null` before a first run. The record is
     * the shared face's own (`FfiReseedResult`): the view resolves its lines
     * and its two failure sentences, because i18n stays in the view — this VM
     * never re-reads the verdict from the lines.
     */
    private val _reseedResult = MutableStateFlow<FfiReseedResult?>(null)
    val reseedResult: StateFlow<FfiReseedResult?> = _reseedResult.asStateFlow()

    /**
     * The nest destination the last restore pulled back from, or `null` for
     * the custodian ceremony — the view names it in the pull-back's own
     * re-enrollment failure (`backups.backup_reseed_pull_back_reenroll_failed`).
     */
    private val _reseedPulledFrom = MutableStateFlow<FfiBackupDestinationView?>(null)
    val reseedPulledFrom: StateFlow<FfiBackupDestinationView?> = _reseedPulledFrom.asStateFlow()

    /**
     * The `destination_id` whose `backup-audit-recover-confirm-modal` is open,
     * or `null` (`ui/backups.md` § Audit-alert surface, the fifth reason).
     */
    private val _recoverConfirming = MutableStateFlow<String?>(null)
    val recoverConfirming: StateFlow<String?> = _recoverConfirming.asStateFlow()

    /**
     * A recovery is running: every `backup-audit-recover-button` is disarmed
     * until it ends, so one tap cannot start two runs, and
     * `backup-audit-recover-result` carries the running line.
     */
    private val _recoverRunning = MutableStateFlow(false)
    val recoverRunning: StateFlow<Boolean> = _recoverRunning.asStateFlow()

    /** How the last recovery ended, or `null` before a first run. */
    private val _recoverResult = MutableStateFlow<RecoverResult?>(null)
    val recoverResult: StateFlow<RecoverResult?> = _recoverResult.asStateFlow()

    /** True while a list/add/edit/remove round-trip is in flight (disables controls). */
    private val _working = MutableStateFlow(false)
    val working: StateFlow<Boolean> = _working.asStateFlow()

    val errorMessage = MutableStateFlow<String?>(null)

    fun refresh() = run { api.backupDestinationsList() }

    fun add(url: String, name: String) = run { api.backupDestinationAdd(url.trim(), name.trim()) }

    /**
     * Enroll **this device** as a client custodian (`backups.md` § Third
     * destination kind → *Enrollment*) — the add dialog's other branch, taken
     * when the user picks the client-device kind.
     *
     * The device id is read **here** rather than in the composable, and it must
     * be the sync device id [SecureStorage.resolveDeviceId] returns — the same one this
     * phone's file-sync engines present — or the source nest keys the
     * custodian's status row on a device nothing drives. Its absence is
     * surfaced, never defaulted: passing the blank string reaches the shared
     * enroll's own `MissingDeviceId` refusal by the same path linux takes,
     * rather than inventing a second message for it here.
     *
     * [capacityCapBytes] is `null` for uncapped — a real choice the user makes
     * by leaving the field blank, never a substituted default.
     */
    fun enrollCustodian(name: String, capacityCapBytes: ULong?) = run {
        api.backupDestinationEnrollCustodian(
            secureStorage.resolveDeviceId().orEmpty(), name.trim(), capacityCapBytes,
        )
    }

    fun edit(id: String, url: String, name: String) =
        run { api.backupDestinationEdit(id, url.trim(), name.trim()) }

    /**
     * Remove a destination, and — only when the user ticked
     * `backup-destination-remove-reclaim-checkbox` — free this device's copy
     * straight after.
     *
     * **Ordering is load-bearing and mirrors linux's
     * `remove_destination_and_maybe_reclaim`:** the reclaim runs strictly AFTER
     * a successful deregister, never before and never concurrently. Reclaiming
     * first would destroy the owner's only offline copy while the destination
     * row still stood — and if the removal then failed, the user would be left
     * with a row claiming a copy that no longer exists.
     *
     * A reclaim that refuses or fails after a landed removal is surfaced with
     * **the removal's own success stated**, never silently and never as a bare
     * failure: reporting only "could not free" over a list that no longer shows
     * the row is the one reading a user cannot act on. The surviving store's own
     * `backup-orphaned-store-row` is both the honest state and the way to retry.
     */
    fun remove(id: String, alsoReclaim: Boolean = false) {
        viewModelScope.launch {
            _working.value = true
            try {
                // The removal is committed to state the moment it lands. Nothing
                // below may retract it: a failed reclaim afterwards is reported
                // ALONGSIDE a removal that really did happen, never instead of
                // it.
                _destinations.value = api.backupDestinationRemove(id)
                if (alsoReclaim) {
                    reclaimAfterRemoval()
                }
                refreshStatuses()
                refreshOrphanedStore()
                triggerAudit()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _working.value = false
            }
        }
    }

    /**
     * The opt-in reclaim that follows a landed removal.
     *
     * Surfaces a refusal or failure without ever claiming the removal failed —
     * and deliberately surfaces the shared `still_hosting` message itself rather
     * than composing an English "removed, but could not free" sentence around
     * it: that composed string has no approved i18n key, and the removal's
     * success is already visible in the list it just left. What the user needs
     * is the reason the bytes are still there plus a way to retry, and the
     * `backup-orphaned-store-row` this refresh is about to paint IS that way.
     */
    private suspend fun reclaimAfterRemoval() {
        val problem = runCatching { api.reclaimCustodianStore(dataDir()) }.fold(
            onSuccess = { if (it.stillHosting) RECLAIM_STILL_HOSTING else null },
            onFailure = { it.message ?: "error" },
        )
        if (problem != null) errorMessage.value = problem
    }

    /**
     * Free this device's whole sealed store — the confirmed standalone
     * `backup-destination-reclaim-button` action, not tied to a removal.
     *
     * A `still_hosting` refusal is surfaced as an error even though the call
     * itself succeeded: nothing was deleted, and reporting it as success would
     * retire the row in the user's mind while the bytes remain.
     */
    fun reclaimStore() = run {
        val outcome = api.reclaimCustodianStore(dataDir())
        if (outcome.stillHosting) throw ApiException(RECLAIM_STILL_HOSTING)
        _destinations.value
    }

    /**
     * Where `backup-destination-reseed-button` paints: `null` for the orphaned
     * store's row, a `destination_id` for a destination row. The shared
     * `backupReseedRows` decides (the rule linux's and tui's `reseed_rows` and
     * apple's `reseedRows` apply) over what this VM already caches — the list
     * and the orphaned verdict — so it costs no round trip. No device id claims
     * no destination row; a failed call offers nothing.
     */
    fun reseedRows(
        destinations: List<FfiBackupDestinationView>,
        orphanedStoreBytes: ULong?,
    ): List<String?> = runCatching {
        com.fauna.ffi.backupReseedRows(
            destinations, secureStorage.resolveDeviceId().orEmpty(), orphanedStoreBytes != null,
            // android hosts the nest-held pull-back: [reseed] runs it in-process.
            true,
        )
    }.getOrDefault(emptyList())

    /**
     * Whether the restore armed on [destinationId]'s row pulls back from a
     * surviving nest (the shared `backupReseedRunsPullBack`, never the row's
     * kind re-read here) — the confirm then names that nest.
     */
    fun reseedRunsPullBack(destinations: List<FfiBackupDestinationView>, destinationId: String): Boolean =
        runCatching { com.fauna.ffi.backupReseedRunsPullBack(destinations, destinationId) }
            .getOrDefault(false)

    /**
     * The confirmed restore (`backups.md` § Restore after losing the nest).
     *
     * Nothing about the ceremony's order is decided here: the shared in-process
     * face pauses this device's backup, runs the ceremony and re-enrolls this
     * device when the outcome is whole; this only records how it ended, as
     * linux's `render_reseed` and apple's `applyReseed` do. A thrown failure is
     * folded into a stop — nothing was made live either way.
     *
     * Re-lists only when nothing is left to say: the re-enrolled row appears and
     * the orphaned verdict is re-measured against it. A stop or a re-enrollment
     * failure keeps its message.
     *
     * [source] is the pressed row: `null` for the orphaned store's, a
     * `destination_id` for a destination row's. A surviving nest's row runs
     * the nest-held pull-back through the shared host; every other runs the
     * custodian ceremony.
     */
    fun reseed(source: String? = null) {
        if (_reseedRunning.value) return
        val deviceId = secureStorage.resolveDeviceId()
        val pullFrom = source
            ?.takeIf { reseedRunsPullBack(_destinations.value, it) }
            ?.let { id -> _destinations.value.firstOrNull { it.destinationId == id } }
        viewModelScope.launch {
            _reseedRunning.value = true
            _reseedResult.value = null
            _reseedPulledFrom.value = pullFrom
            val result = try {
                if (deviceId.isNullOrBlank()) throw ApiException("no device id")
                if (pullFrom != null) {
                    api.pullBackFromDestination(deviceId, pullFrom.destinationId)
                } else {
                    api.reseedCustodianStore(deviceId, dataDir())
                }
            } catch (e: Exception) {
                reseedStopped(e.message ?: "error")
            } finally {
                _reseedRunning.value = false
            }
            _reseedResult.value = result
            if (result.stopped == null && result.reenrollError == null) {
                refresh()
            }
        }
    }

    /** `backup-audit-recover-button[i]`: arm the confirm for [destinationId]. */
    fun armRecover(destinationId: String) {
        if (_recoverRunning.value) return
        _recoverConfirming.value = destinationId
    }

    /** `backup-audit-recover-cancel-button`: close the confirm, no side effect. */
    fun cancelRecover() {
        _recoverConfirming.value = null
    }

    /**
     * `backup-audit-recover-confirm-button`: run the shared recovery once, then
     * re-run the audit pass so the cleared notice is read back from the store,
     * never assumed. The run goes through the shared FFI face; a thrown
     * failure is folded into the result as its reason, and re-running is safe
     * (nothing is recovered twice).
     */
    fun confirmRecover() {
        val destinationId = _recoverConfirming.value ?: return
        if (_recoverRunning.value) return
        val deviceId = secureStorage.resolveDeviceId()
        _recoverConfirming.value = null
        viewModelScope.launch {
            _recoverRunning.value = true
            _recoverResult.value = null
            val result = try {
                if (deviceId.isNullOrBlank()) throw ApiException("no device id")
                // The row's kind decides the leg (the shared predicate, never a
                // raw compare): a nest runs the network leg; this device's own
                // custodian row — the only client-device row `recoverOffered`
                // answers yes for — runs the store leg in this process.
                val ownRow = _destinations.value.any {
                    it.destinationId == destinationId && destinationRowIsAClientDevice(it)
                }
                val ran = if (ownRow) {
                    api.recoverCustodianStore(deviceId, dataDir())
                } else {
                    api.backupRecoverFromDestination(deviceId, destinationId)
                }
                RecoverResult(lines = ran.lines, failure = null)
            } catch (e: Exception) {
                RecoverResult(lines = emptyList(), failure = e.message ?: "error")
            } finally {
                _recoverRunning.value = false
            }
            _recoverResult.value = result
            refreshAudit()
        }
    }

    /**
     * Re-derive the orphaned-store verdict — mount, and after every mutation.
     *
     * Both halves come from shared Rust and neither is re-implemented here: the
     * footprint is this device's own disk (reached WITHOUT a custodian host,
     * which does not exist in this state), and the join is
     * `custodian_store_is_orphaned` over the destination list already loaded.
     *
     * Every failure path lands on `null` — no device id, no read — because the
     * row it paints arms a destructive gesture and "cannot tell" must not offer
     * to delete the owner's only offline copy.
     */
    private suspend fun refreshOrphanedStore() {
        val deviceId = secureStorage.resolveDeviceId()
        if (deviceId.isNullOrBlank()) {
            _orphanedStoreBytes.value = null
            return
        }
        val info = runCatching { api.custodianStoreFootprint(dataDir()) }.getOrNull()
        if (info == null) {
            _orphanedStoreBytes.value = null
            return
        }
        val orphaned = runCatching {
            com.fauna.ffi.custodianStoreIsOrphaned(
                _destinations.value, deviceId, info.bytes > 0uL,
            )
        }.getOrDefault(false)
        _orphanedStoreBytes.value = if (orphaned) info.bytes else null
    }

    private fun dataDir(): String = accountStores.custodianStoreBaseDir()

    /**
     * Run one destination mutation, fold the freshly-persisted list back into
     * state, re-read the live per-destination status against that new set, and
     * surface failures. The shared FFI returns the updated list, so there is no
     * separate refresh round-trip (mirrors the linux closures). The raw error
     * (incl. the [EDIT_DIFFERENT_NEST_ERR] sentinel) rides through; the Screen
     * localizes the sentinel before showing it (i18n stays in the view).
     */
    private fun run(block: suspend () -> List<FfiBackupDestinationView>) {
        viewModelScope.launch {
            _working.value = true
            try {
                _destinations.value = block()
                refreshStatuses()
                refreshOrphanedStore()
                triggerAudit()
            } catch (e: Exception) {
                // The unwrapped line, so the first-listing gate's refusal reads
                // as `common.needs_nest`, not the generated `"msg=…"` form.
                errorMessage.value = e.loadFailureText() ?: "error"
            } finally {
                _working.value = false
            }
        }
    }

    /**
     * Read the LIVE per-destination status and rebuild the [statuses] map, keyed by
     * `destination_id`. Zero destinations ⇒ an empty map — the rows render only when
     * ≥1 destination is configured, and the FFI fn also skips its build +
     * `fauna.segments.list` round-trip. A transient failure leaves the prior map
     * (degrade to baseline before the first read), matching linux's
     * `run_status_read` / apple's `refreshStatuses` — no scary error before the user
     * acts.
     */
    private suspend fun refreshStatuses() {
        // The nest's projection since the leg-(d) repoint — no device id and no
        // segment-backup dir needed any more (backups.md § Per-destination status
        // read). Zero destinations still short-circuits: the rows render only when
        // at least one is configured.
        if (_destinations.value.isEmpty()) {
            _statuses.value = emptyMap()
            return
        }
        val rows = runCatching { api.loadBackupDestinationStatus() }
            .getOrNull() ?: return
        _statuses.value = rows.associateBy { it.destinationId }
    }

    /**
     * Fire the audit pass in its **own** round trip, never awaited by [run]'s
     * try block: the audit opens this client's own authenticated connection
     * to every configured destination, so a slow or unreachable one must not
     * delay re-enabling the add/edit/remove controls (mirrors linux/web,
     * which both keep the audit independent of the status-row read for the
     * same reason).
     */
    private fun triggerAudit() {
        viewModelScope.launch { refreshAudit() }
    }

    /**
     * Read the client's own audit picture and rebuild [auditRows], keyed by
     * `destination_id`. Zero destinations ⇒ empty map, same short-circuit as
     * [refreshStatuses]. A failed pass keeps the prior records — dropping
     * them would clear a standing banner on a merely transient failure,
     * which is the shared `merge_outcomes`' whole reason for existing.
     */
    private suspend fun refreshAudit() {
        if (_destinations.value.isEmpty()) {
            _auditRows.value = emptyMap()
            return
        }
        val rows = runCatching { api.backupAuditRunPass(ownCustodian()) }.getOrNull() ?: return
        _auditRows.value = rows.associateBy { it.destinationId }
    }

    /**
     * This device's own custodian store for the audit pass's fold: the same
     * footprint read [refreshOrphanedStore] makes, handed back with this
     * device's sync id. Nothing is derived here — the shared pass decides
     * which row the store's standing source regressions land on. No device
     * id or no read is `null`, "no store was read", which leaves that row's
     * record standing rather than clearing a notice on a failed read.
     */
    private suspend fun ownCustodian(): com.fauna.ffi.FfiOwnCustodianStore? {
        val deviceId = secureStorage.resolveDeviceId()
        if (deviceId.isNullOrBlank()) return null
        val info = runCatching { api.custodianStoreFootprint(dataDir()) }.getOrNull()
            ?: return null
        return com.fauna.ffi.FfiOwnCustodianStore(deviceId, info.sourceRegressions)
    }

    /**
     * How a recovery ended: the shared face's resolved `lines` (verdict first),
     * or the [failure] reason when the run never reached the shared driver's end.
     */
    data class RecoverResult(val lines: List<String>, val failure: String?)

    companion object {
        /** Mirror of `fauna_ffi::backup_destinations::EDIT_DIFFERENT_NEST_ERR`. */
        const val EDIT_DIFFERENT_NEST_ERR = "backup-destination-edit-different-nest"

        /**
         * A reclaim refused because this device was still backing up — the
         * shared `still_hosting` outcome, localized in the view like its
         * sibling above. Not an FFI mirror: `still_hosting` is a boolean field,
         * so this sentinel exists only to carry it to the one place that owns
         * i18n.
         */
        const val RECLAIM_STILL_HOSTING = "backup-reclaim-still-hosting"

        /** A stopped restore in the shared record's own shape. */
        internal fun reseedStopped(reason: String) = FfiReseedResult(
            stopped = reason, resultLines = emptyList(), isWhole = false, reenrollError = null,
        )
    }
}
