package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ApiException
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiBackupDestinationStatus
import com.fauna.ffi.FfiBackupDestinationView
import com.fauna.ffi.FfiDestinationAuditRow
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
     * be the sync device id [SecureStorage.deviceId] holds — the same one this
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
            secureStorage.deviceId.orEmpty(), name.trim(), capacityCapBytes,
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
        val deviceId = secureStorage.deviceId
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
                errorMessage.value = e.message ?: "error"
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
        val deviceId = secureStorage.deviceId
        if (deviceId.isNullOrBlank()) return null
        val info = runCatching { api.custodianStoreFootprint(dataDir()) }.getOrNull()
            ?: return null
        return com.fauna.ffi.FfiOwnCustodianStore(deviceId, info.sourceRegressions)
    }

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
    }
}
