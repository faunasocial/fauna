package com.fauna.app.core

import com.fauna.ffi.FfiAftermathLeg
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import uniffi.fauna_core.LocalizedText
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The post-succession aftermath's per-leg progress, as the Recovery kit
 * section reads it (`docs/goal/ui/settings.md` § Recovery kit → *The
 * post-succession aftermath's progress lines*) — apple's `AftermathProgress`
 * held by `FaunaClient`, linux's `AftermathProgress`, web's module store.
 * One field per leg of the ordered sequence, fed by
 * [LoggingAftermathSink.progress] as each leg reports.
 *
 * `null` is a real value, not an absence: a leg whose outcome owes the user
 * nothing reports `null` from the shared projection, and the render hides that
 * line — never a Kotlin-side decision (priority #1/#3). It is recorded like any
 * other value, so a leg that settles into "nothing to report" clears a stale
 * line from an earlier pass rather than leaving it painted.
 */
data class AftermathProgress(
    /** Leg 2 — the `NestBackupKey` re-grant. */
    val backupRegrant: LocalizedText? = null,
    /** Leg 3 — the `__mls` re-seal. Reported by `fauna-ffi`'s `mls_sync_launcher`
     *  through the same sink, so it may arrive before, with, or after the rest of
     *  a pass, or alone (a resumed reseal from an earlier sign-in). */
    val mlsReseal: LocalizedText? = null,
    /** Leg 4 — the capability-grant re-mint. */
    val grantRemint: LocalizedText? = null,
    /** Leg 5 — the file corpus. Reports from the sync agent's own process, in no
     *  FFI app's sink (`FfiAftermathLeg` has no such case): **always `null`**
     *  here, like apple's, and the render path for it is unreachable. */
    val corpusReseal: LocalizedText? = null,
    /** Leg 7 — the `__drafts` re-seal. Rendered ABOVE leg 6. */
    val draftsReseal: LocalizedText? = null,
    /** Leg 6 — the mail burn, the only leg that takes something away. */
    val mailBurn: LocalizedText? = null,
)

/**
 * Process-wide holder of the current [AftermathProgress]. `@Singleton` because
 * the sink is built by [SuccessionAftermathHost] on the post-auth hook while the
 * Recovery kit section's view model is created per screen — the two meet here.
 * One account's pass: an account switch or sign-out drops it (`AccountStores`
 * closer), so the next account's page can never paint the previous one's lines.
 */
@Singleton
class AftermathProgressStore @Inject constructor(accountStores: AccountStores) {
    private val _progress = MutableStateFlow(AftermathProgress())
    val progress: StateFlow<AftermathProgress> = _progress.asStateFlow()

    init {
        accountStores.registerCloser("aftermath-progress") { _progress.value = AftermathProgress() }
    }

    /** Record one leg's already-resolved line; the FFI runtime's thread calls
     *  this, so the update is atomic. */
    fun record(leg: FfiAftermathLeg, line: LocalizedText?) {
        _progress.update { p ->
            when (leg) {
                FfiAftermathLeg.BACKUP_REGRANT -> p.copy(backupRegrant = line)
                FfiAftermathLeg.MLS_RESEAL -> p.copy(mlsReseal = line)
                FfiAftermathLeg.GRANT_REMINT -> p.copy(grantRemint = line)
                FfiAftermathLeg.DRAFTS_RESEAL -> p.copy(draftsReseal = line)
                FfiAftermathLeg.MAIL_BURN -> p.copy(mailBurn = line)
            }
        }
    }
}
