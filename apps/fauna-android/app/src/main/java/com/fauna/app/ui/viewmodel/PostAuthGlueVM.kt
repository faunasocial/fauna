package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import com.fauna.app.core.ApiClient
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.ShellLog
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject

/**
 * The legs every flavor runs at the universal post-auth hook (the authed app
 * surface's mount, `FaunaNavHost`): the one-tap trust mint, the host-address
 * report, the deployment-seed custody leg and the seal backfill. Each is
 * best-effort; only an unconfirmed custody leg is user-visible.
 *
 * The bridge legs that share the same hook (the first-setup mail / CalDAV /
 * CardDAV / WebDAV enables, the mail epoch refresh and the critical-alert
 * sweep) live in [MailEnableGlueVM], in `src/noKids/`: the kids build type
 * compiles every bridge out (`family-safety.md` § The account age band, the
 * kids-app bullet, item (4)), so they run from `KidsExcisedPostAuthEffects`
 * rather than from here.
 */
@HiltViewModel
class PostAuthGlueVM @Inject constructor(
    private val api: ApiClient,
    private val host: OnboardingHost,
) : ViewModel() {

    /**
     * onboarding.md § 3b-ter: the one-tap trust answer latched on the
     * `trust_prompt` page. The page only asks — minting needs this
     * authenticated session and the nest's own content-processor roster — so
     * the mint runs here, deferred to the authed launch surface.
     * Best-effort and log-only by design (mirrors tui/linux's
     * `mint_default_trust_set`): a failure must not paint an error over a
     * completed onboarding, and the same trust is grantable any time from
     * Settings → Nests. WHICH grants is not decided here either:
     * `MintDefaultSet` mints exactly what the shared mint catalog derives, so
     * this glue holds no policy that could drift from the Nests page's own
     * picker (and an empty set on a box with nothing enrolled yet is an
     * honest no-op, not an error).
     */
    suspend fun mintDefaultTrustSet() {
        if (!host.consumePendingTrustGranted()) return
        try {
            val machine = api.buildLinkedNestsMachine()
            if (machine == null) {
                ShellLog.w(
                    "MailEnableGlue",
                    "one-tap trust: no linked-nests machine (recoverable via Settings → Nests)",
                )
                return
            }
            machine.dispatch(uniffi.fauna_client_pair.LinkedNestsAction.MintDefaultSet)
        } catch (e: Exception) {
            android.util.Log.w(
                "MailEnableGlue",
                "one-tap trust: minting the default set failed (recoverable via Settings → Nests): ${e.message}",
            )
            ShellLog.w(
                "MailEnableGlue",
                "one-tap trust: minting the default set failed (recoverable via Settings → Nests): ${e.message}",
            )
        }
    }

    /**
     * Register the RecoveryKey root the user confirmed on the sign-up
     * `recovery_kit` page (`identity-succession.md` § The RecoveryKey →
     * *Creation UX*) — THAT root, the one just written down, never a fresh one,
     * over the shared `register_deferred_kit` ceremony tui, linux and web run
     * at the same edge. No-op when the kit was skipped or never offered.
     * Best-effort and log-only: the ceremony never errors on its own failure,
     * and Settings' `recovery-kit-status` tells the truth (never-created, or
     * registered-no-escrow).
     */
    suspend fun registerDeferredRecoveryKit() {
        val kit = host.consumePendingRecoveryKit() ?: return
        try {
            api.registerDeferredRecoveryKit(kit)
        } catch (e: Exception) {
            ShellLog.w("RecoveryKitGlue", "registering the sign-up recovery kit failed: ${e.message}")
        }
    }

    /**
     * Host-address acquisition glue (domains-and-tls-bootstrap.md § Host-address
     * acquisition). Fired at the universal post-auth hook, admin-gated — a
     * non-admin call is refused nest-side, so gating avoids a pointless failing
     * RPC on every connect. Fire-and-forget: logs the outcome, never surfaces to
     * the user (a later connect retries idempotently).
     */
    suspend fun reportHostAddress() {
        if (!api.checkIsAdmin()) return
        try {
            when (val outcome = api.reportHostAddress()) {
                is com.fauna.ffi.FfiHostAddressOutcome.Reported ->
                    ShellLog.i("HostAddressGlue", "Reported host address: ${outcome.nestIpv4}")
                is com.fauna.ffi.FfiHostAddressOutcome.SkippedNoPublicIp ->
                    ShellLog.d("HostAddressGlue", "SkippedNoPublicIp (normal on a LAN box)")
                is com.fauna.ffi.FfiHostAddressOutcome.Failed ->
                    ShellLog.w("HostAddressGlue", "Failed: ${outcome.error}")
            }
        } catch (e: Exception) {
            ShellLog.w("HostAddressGlue", "reportHostAddress failed: ${e.message}")
        }
    }

    /** What [runDeploymentSeedCustodyLeg] surfaces: the `NOT_PROTECTED_*` cases warrant a
     *  user-visible warning (the launch glue maps them to an [com.fauna.app.core.AppMessages]
     *  warning banner); `OK` shows nothing (custody held or just captured, or not an
     *  admin of this nest — nothing owed). */
    enum class RecoveryCustodyOutcome { OK, NOT_PROTECTED_MISMATCH, NOT_PROTECTED_FAILED }

    /**
     * The deployment-seed **custody leg** at the universal post-auth hook
     * (`box-recovery.md` § The plane-era recovery floor, *(c) The writes*) — the
     * only capture, run on every login and every returning-user relaunch: if the
     * account plane holds no live entry for THIS nest and this identity is an
     * admin here, the shared leg fetches the box's seed, refuses one that does not
     * derive to the bound id (BR-2), and merges the entry. The account-runtime
     * seat runs the same leg at its store-ready edge, so a run here that found no
     * store yet is retried there and at the next connect.
     *
     * A run that ends with custody **unconfirmed for an admin** is USER-VISIBLE,
     * never a silent log: the returned outcome drives the launch surface's
     * recovery-custody warning banner (the android idiom of linux's toast).
     * `NOT_ADMIN` and `ALREADY_CUSTODIED` show nothing.
     */
    suspend fun runDeploymentSeedCustodyLeg(): RecoveryCustodyOutcome =
        try {
            recoveryCustodyOutcomeOf(api.selfHealDeploymentSeedCustody())
        } catch (e: Exception) {
            // The bound nest id could not be resolved (or a bad secret) — custody
            // could not even be attempted, so it is not confirmed.
            ShellLog.w(
                "DeploymentSeedGlue",
                "deployment-seed custody leg failed: ${e.message}",
            )
            RecoveryCustodyOutcome.NOT_PROTECTED_FAILED
        }

    /** The custody leg's outcome projected onto the recovery-custody warning. */
    private fun recoveryCustodyOutcomeOf(
        outcome: com.fauna.ffi.FfiDeploymentSeedSelfHeal,
    ): RecoveryCustodyOutcome {
        ShellLog.i("DeploymentSeedGlue", "custody leg: $outcome")
        return when (outcome) {
            is com.fauna.ffi.FfiDeploymentSeedSelfHeal.AlreadyCustodied,
            is com.fauna.ffi.FfiDeploymentSeedSelfHeal.NotAdmin -> RecoveryCustodyOutcome.OK
            is com.fauna.ffi.FfiDeploymentSeedSelfHeal.HandoffUnavailable,
            is com.fauna.ffi.FfiDeploymentSeedSelfHeal.NestHoldsNoSeed ->
                RecoveryCustodyOutcome.NOT_PROTECTED_FAILED
            is com.fauna.ffi.FfiDeploymentSeedSelfHeal.Captured -> when (outcome.v1) {
                com.fauna.ffi.FfiDeploymentSeedCapture.REFUSED_MISMATCH -> {
                    ShellLog.e(
                        "DeploymentSeedGlue",
                        "deployment-seed custody refused: seed does not derive to this nest (BR-2); recovery not protected",
                    )
                    RecoveryCustodyOutcome.NOT_PROTECTED_MISMATCH
                }
                com.fauna.ffi.FfiDeploymentSeedCapture.WROTE,
                com.fauna.ffi.FfiDeploymentSeedCapture.ALREADY_HELD_SAME,
                com.fauna.ffi.FfiDeploymentSeedCapture.REFUSED_DIFFERING ->
                    RecoveryCustodyOutcome.OK
            }
        }
    }

    /**
     * The S8 D1 + D3 client-driven seal backfill (`file-sync.md` § Sealed
     * names & paths → Implementation status today), at the same universal
     * post-auth hook as [runDeploymentSeedCustodyLeg], same posture: best-effort,
     * fire-and-forget — an unreachable nest just retries next session start.
     * The D1-then-D3-skip-member sequencing lives once in the shared
     * `seal_backfill` sweep (`ApiClient.runSealBackfillSweep`), which every
     * UniFFI app now calls instead of hand-rolling the loop; this glue only
     * logs. Never throws — the sweep is best-effort by contract.
     */
    suspend fun runSealBackfill() {
        val report = api.runSealBackfillSweep()
        val noteworthy = report.fieldsError != null ||
            report.rosterError != null ||
            report.setFailures > 0u ||
            report.tags.stampFailures > 0u ||
            (report.fields?.let { it.names > 0u || it.updateFailures > 0u || it.identityMismatch > 0u } ?: false)
        if (noteworthy) {
            ShellLog.w(
                "SealBackfillGlue",
                "sweep: fieldsError=${report.fieldsError} rosterError=${report.rosterError} " +
                    "setFailures=${report.setFailures} tagStampFailures=${report.tags.stampFailures}",
            )
        } else {
            ShellLog.i(
                "SealBackfillGlue",
                "sweep: ok (setsSwept=${report.setsSwept}, memberSetsSkipped=${report.memberSetsSkipped})",
            )
        }
    }
}
