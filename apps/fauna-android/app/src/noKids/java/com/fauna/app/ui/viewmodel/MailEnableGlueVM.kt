package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import com.fauna.app.core.ApiClient
import com.fauna.app.core.CriticalAlertsHost
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.ShellLog
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject

/**
 * Post-onboarding serving-enablement hand-off
 * (docs/goal/behavior/onboarding.md § 3b *Mechanism*, caldav-server.md
 * § Independent enablement).
 *
 * The claim-time enable-email / enable-caldav / enable-carddav / enable-webdav
 * intents are machine-derived defaults (the wizard checkboxes are retired with
 * the storage-mode page, onboarding.md § 3b-bis) and record intent only: the
 * wizard runs pre-identity but the `set_*_enabled` toggles are Admin-class.
 * [OnboardingHost] latches all four on `WizardOutcome::LoggedIn`; the authed app
 * surface invokes [applyPostClaimServingEnablement] once on mount, which takes
 * the four latches together and makes the ONE shared-Rust call
 * (`fauna_client_mail_settings::serving_enablement`, via
 * `ApiClient.applyPostClaimServingEnablement`) that resolves `am_i_admin` and
 * fires the first-setup mail provision, the three DAV toggles and the
 * one-MSEK-mint-path companion mints itself. Mirrors apple's
 * `MailEnableGlue.applyPostClaimServingEnablement` and is idempotent with the
 * mail-settings enable path, so a missed fire is recoverable.
 *
 * Also the two other bridge legs of the same post-auth hook — the mail epoch
 * refresh and the critical-alert sweep (whose only feeder is the atproto
 * settings machine). Every leg here is a bridge the kids build type compiles
 * out, so this VM lives in `src/noKids/` and runs from
 * `KidsExcisedPostAuthEffects`; the legs every flavor keeps are
 * [PostAuthGlueVM]'s.
 */
@HiltViewModel
class MailEnableGlueVM @Inject constructor(
    private val api: ApiClient,
    private val host: OnboardingHost,
) : ViewModel() {

    /**
     * Post-claim serving enablement — ONE entry for every fresh-onboarding path,
     * run once at the first authed launch after onboarding reached `LoggedIn`
     * (the latch is `null` on every returning-user relaunch → no-op, which is the
     * once-only / opt-out guarantee: `disableMail` clears the MSEK but the latch
     * stays unset, so an opt-out is not re-minted on the next launch). Consumes
     * all four latches together as one boundary and hands them to the shared
     * step, which does the `am_i_admin` discrimination, the DAV toggles and the
     * companion mints (exactly one MSEK-minting path per first setup) itself.
     * Best-effort: a failure is logged, not surfaced (the shared path is
     * idempotent and the mail-settings / admin pages are the backstop).
     */
    suspend fun applyPostClaimServingEnablement() {
        val intents = host.consumePendingServingEnablement() ?: return
        try {
            api.applyPostClaimServingEnablement(
                email = intents.email,
                caldav = intents.caldav,
                carddav = intents.carddav,
                webdav = intents.webdav,
            )
        } catch (e: Exception) {
            android.util.Log.w(
                "MailEnableGlue",
                "post-claim serving enablement failed (recoverable via mail-settings/admin pages): ${e.message}",
            )
            ShellLog.w("MailEnableGlue", "post-claim serving enablement failed (recoverable via mail-settings/admin pages): ${e.message}")
        }
    }

    /**
     * Publish the subscriber encapsulation-key registry entry
     * (`post-quantum.md` § surface B) at the same universal post-auth hook as
     * [applyPostClaimServingEnablement]: idempotent, best-effort, logged only. A
     * third-party follow under `fauna:follow:write` is refused while the account
     * has no entry (`authorization-server.md` § Scope grammar → *The sixth arm*).
     */
    suspend fun publishSubscriberEncapsKey() {
        try {
            api.publishSubscriberEncapsKey()
            ShellLog.i("EncapsKeyGlue", "publishSubscriberEncapsKey: ok")
        } catch (e: Exception) {
            ShellLog.w("EncapsKeyGlue", "publishSubscriberEncapsKey failed: ${e.message}")
        }
    }

    /**
     * Run the feeders that have no page of their own (`critical-alerts.md`
     * § Mechanism → *Who runs the detector*) at the same universal post-auth
     * hook as [PostAuthGlueVM.runDeploymentSeedCustodyLeg]: the re-sweep loop
     * on a first or identity-changing sign-in, one pass on a same-identity
     * re-establish ([com.fauna.app.core.KidsExcisedApi.startCriticalAlertSweep]).
     * Fire-and-forget on [CriticalAlertsHost]'s own process-wide scope — this
     * VM's scope dies with the Composable that hosts it, which would silently
     * kill a loop that stops only at [CriticalAlertsHost.clearAll].
     */
    fun startCriticalAlertSweep() {
        api.startCriticalAlertSweep()
    }

}
