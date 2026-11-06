package com.fauna.app.core

import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow

/**
 * The seam `backup_audit_run_now` needs from the e2e harness: the Backups
 * page's view model owns the audit picture (`BackupDestinationsVM.auditRows`),
 * so a pass that only ran in the agent would persist state the page never
 * repaints from. The debug-only `TestAgent` calls [request]; a live view model
 * collects [requests] and runs its own audit refresh, the same one a
 * destination add/edit/remove triggers (linux's `backup_audit::poke_rerun`
 * twin; testing.md convention 15 — the automation caller lives in the debug
 * source set, production never emits).
 */
object E2eBackupAuditPoke {
    private val _requests = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    val requests: SharedFlow<Unit> = _requests

    /** Whether a Backups page view model is collecting right now. */
    fun isListening(): Boolean = _requests.subscriptionCount.value > 0

    /** `false` when no Backups page view model is collecting — the caller
     *  refuses loudly (convention 11) rather than dropping the poke. */
    fun request(): Boolean = isListening() && _requests.tryEmit(Unit)
}
