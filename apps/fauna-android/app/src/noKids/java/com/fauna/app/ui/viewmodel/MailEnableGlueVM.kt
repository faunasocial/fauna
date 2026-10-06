package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import com.fauna.app.core.ApiClient
import com.fauna.app.core.CriticalAlertsHost
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.ShellLog
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject

/**
 * Post-onboarding enable-email / enable-caldav hand-off
 * (docs/goal/behavior/onboarding.md §3b, caldav-server.md § Independent enablement).
 *
 * The claim-time enable-email / enable-caldav intents are machine-derived defaults
 * (`emailEnableRequested()` / `caldavEnableRequested()` — the wizard checkboxes are
 * retired with the storage-mode page, onboarding.md § 3b-bis) and record intent
 * only: the wizard runs pre-identity but
 * `fauna.bridges.set_mail_enabled` / `set_caldav_enabled` are Admin-class.
 * [OnboardingHost] latches both intents on `WizardOutcome::LoggedIn`; the authed
 * app surface invokes [provisionMailAtFirstSetup] and [applyPendingCaldavEnable] once
 * on mount to fire each toggle with the now-authenticated Admin client. Email and
 * CalDAV gate independently, so each is captured + fired separately — but
 * SEQUENTIALLY, with the sibling intents snapshotted first via
 * [peekMailWasRequested] / [peekCaldavWasRequested], never as independent
 * concurrent launches: the calendar/contacts-only MSEK-mint gates below need
 * the ORIGINAL request booleans, not whichever glue call happens to consume
 * its own latch first. Mirrors Linux's authed launch glue (which defers the
 * DNS-credential seal the same way) and is idempotent with the mail-settings
 * enable path, so a missed fire is recoverable.
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
    private val criticalAlertsHost: CriticalAlertsHost,
) : ViewModel() {

    /**
     * First-authenticated-setup mail glue — ONE entry for both fresh-onboarding
     * paths, run once at the first authed launch after onboarding reached
     * `LoggedIn` (the latch is `null` on every returning-user relaunch → no-op),
     * `am_i_admin`-discriminated, mirroring Linux's `provision_mail_at_first_setup`:
     *
     * - **Admin claim**: honor the latched machine-derived enable-email intent
     *   — auto-mint the admin's own mailbox via the shared
     *   `enableMailWithGeneratedPassword`, which also fires the deployment-wide
     *   Admin-class `set_mail_enabled(true)` (booting the co-located bridge, which
     *   self-enrolls over loopback and is auto-approved nest-side — the admin's
     *   enable IS the approval, mail-bridge-lifecycle.md § Onboarding auto-approval).
     * - **New (non-admin) user**: auto-mint the user's own mailbox iff the
     *   deployment policy allows — `emailEnabled && autoEnableMailForNewUsers` (read
     *   from `fauna.setup.status`) && no mailbox yet. The gate + mint live in the
     *   shared `autoEnableMailForNewUser` (returns null, not an error, when gated
     *   off), so a new user gets a working `<handle>@<domain>` mailbox with no admin
     *   provisioning and no manual toggle.
     *
     * The latched intent is meaningful only on the admin branch (it defaults ON for
     * the non-admin invite-redeem path, which never runs the admin-path claim
     * steps), so `am_i_admin` — not the flag — gates the admin
     * branch. The generated password is dropped — sealed into the credential and
     * re-revealable on the mail-settings page
     * (`mail-settings-credential-item-reveal-secret`). Best-effort: a failure is
     * logged, not surfaced (the shared path is idempotent and the mail-settings page
     * is the backstop). mail-credentials.md § Auto-enable for new users.
     */
    suspend fun provisionMailAtFirstSetup() {
        val enableEmail = host.consumePendingFirstSetupMail() ?: return
        try {
            val machine = api.buildMailSettingsMachine()
            if (machine == null) {
                ShellLog.w(
                    "MailEnableGlue",
                    "first-setup mail auto-mint skipped: no mail-settings machine (recoverable via mail-settings)",
                )
                return
            }
            if (api.checkIsAdmin()) {
                // Admin claim: honor the onboarding enable-email checkbox.
                if (enableEmail) machine.enableMailWithGeneratedPassword("Default")
            } else {
                // New non-admin user: gated auto-mint per deployment policy. The
                // shared method returns null (not an error) when a gate fails
                // (policy off, or a mailbox already exists).
                val status = api.nestSetupStatus()
                machine.autoEnableMailForNewUser(
                    status.emailEnabled,
                    status.autoEnableMailForNewUsers,
                    "Default",
                )
            }
        } catch (e: Exception) {
            android.util.Log.w(
                "MailEnableGlue",
                "first-setup mail auto-mint failed (recoverable via mail-settings): ${e.message}",
            )
            ShellLog.w("MailEnableGlue", "first-setup mail auto-mint failed (recoverable via mail-settings): ${e.message}")
        }
    }

    /**
     * Non-consuming peek at the latched first-setup mail intent, for the
     * launch-glue call site to snapshot BEFORE any glue call consumes its own
     * latch — [applyPendingCaldavEnable]'s and [applyPendingCarddavEnable]'s
     * MSEK-mint gates need the ORIGINAL request boolean, never call-order-
     * dependent leftovers (mirrors apple's `FaunaApp.swift` sequencing).
     */
    fun peekMailWasRequested(): Boolean = host.peekPendingFirstSetupMail() ?: false

    /** Sibling of [peekMailWasRequested] for the CalDAV intent — needed by
     *  [applyPendingCarddavEnable]'s gate too (a contacts-only setup mints
     *  only when NEITHER mail NOR CalDAV already did). */
    fun peekCaldavWasRequested(): Boolean = host.peekPendingCaldavEnable()

    /**
     * Fire `set_caldav_enabled(true)` exactly once iff onboarding just completed
     * with enable-caldav left on. Sibling of [provisionMailAtFirstSetup], fired
     * independently because CalDAV gates separately from email. Best-effort: a
     * failure is logged and not surfaced (the toggle is idempotent and the
     * mail-settings page is the backstop). No-op on every ordinary launch.
     *
     * [mailWasEnabled] is the sibling first-setup mail intent, read by the
     * caller via [peekMailWasRequested] **before** [provisionMailAtFirstSetup]
     * consumes its own latch — passed in rather than re-read here so this
     * method never depends on call order. On a **calendar-only** first setup
     * (`!mailWasEnabled`) this ALSO mints the admin's shared MSEK — the
     * per-actor key material the calendar store seals under — mirroring linux
     * `provision_caldav_mailbox_at_first_setup` / apple
     * `MailEnableGlue.applyPendingCaldavEnable` (android was missing the mint
     * entirely; the calendar-only case never got its MSEK). Admin-only,
     * matching the mail path (a non-admin CalDAV auto-mint policy is unbuilt).
     */
    suspend fun applyPendingCaldavEnable(mailWasEnabled: Boolean) {
        if (!host.consumePendingCaldavEnable()) return
        try {
            api.setCaldavEnabled(true)
        } catch (e: Exception) {
            android.util.Log.w(
                "MailEnableGlue",
                "post-onboarding set_caldav_enabled failed (recoverable via mail-settings): ${e.message}",
            )
            ShellLog.w("MailEnableGlue", "post-onboarding set_caldav_enabled failed (recoverable via mail-settings): ${e.message}")
        }
        if (mailWasEnabled) return
        try {
            if (!api.checkIsAdmin()) return
            val machine = api.buildMailSettingsMachine() ?: return
            machine.enableCaldavMailboxWithGeneratedPassword("Default")
        } catch (e: Exception) {
            android.util.Log.w(
                "MailEnableGlue",
                "post-onboarding CalDAV-only mailbox mint failed (recoverable via mail-settings): ${e.message}",
            )
            ShellLog.w("MailEnableGlue", "post-onboarding CalDAV-only mailbox mint failed (recoverable via mail-settings): ${e.message}")
        }
    }

    /**
     * Fire `set_carddav_enabled(true)` exactly once iff onboarding just completed
     * with enable-carddav left on. Contacts sibling of [applyPendingCaldavEnable],
     * fired independently because CardDAV gates separately from both email and
     * calendar (carddav-server.md § Independent enablement). Best-effort: a
     * failure is logged and not surfaced (the toggle is idempotent and the
     * admin-contacts page is the backstop). No-op on every ordinary launch.
     *
     * [mailWasEnabled] / [caldavWasEnabled] are the sibling first-setup intents,
     * read by the caller before their own glue consumes its latch — see
     * [applyPendingCaldavEnable]'s doc. Unlike WebDAV, a **contacts-only**
     * deployment (`!mailWasEnabled && !caldavWasEnabled`) ALSO mints the admin's
     * shared MSEK — the address-book store seals to the MSEK-derived recipient
     * keypair — but only when neither email nor CalDAV already minted it (exactly
     * one MSEK-minting path per first setup, mirroring linux
     * `provision_carddav_mailbox_at_first_setup` / apple
     * `MailEnableGlue.applyPendingCarddavEnable`). Admin-only.
     */
    suspend fun applyPendingCarddavEnable(mailWasEnabled: Boolean, caldavWasEnabled: Boolean) {
        if (!host.consumePendingCarddavEnable()) return
        try {
            api.setCarddavEnabled(true)
        } catch (e: Exception) {
            android.util.Log.w(
                "MailEnableGlue",
                "post-onboarding set_carddav_enabled failed (recoverable via admin-contacts): ${e.message}",
            )
            ShellLog.w("MailEnableGlue", "post-onboarding set_carddav_enabled failed (recoverable via admin-contacts): ${e.message}")
        }
        if (mailWasEnabled || caldavWasEnabled) return
        try {
            if (!api.checkIsAdmin()) return
            val machine = api.buildMailSettingsMachine() ?: return
            machine.enableCarddavMailboxWithGeneratedPassword("Default")
        } catch (e: Exception) {
            android.util.Log.w(
                "MailEnableGlue",
                "post-onboarding CardDAV-only mailbox mint failed (recoverable via admin-contacts): ${e.message}",
            )
            ShellLog.w("MailEnableGlue", "post-onboarding CardDAV-only mailbox mint failed (recoverable via admin-contacts): ${e.message}")
        }
    }

    /**
     * Fire `set_webdav_enabled(true)` exactly once iff onboarding just completed
     * with enable-webdav left on. Files sibling of [applyPendingCarddavEnable],
     * fired independently because WebDAV gates separately from email, calendar,
     * and contacts (webdav-server.md § Independent enablement). Best-effort: a
     * failure is logged and not surfaced (the toggle is idempotent and the
     * admin-files page is the backstop). No-op on every ordinary launch. Unlike
     * carddav, WebDAV has NO per-actor mailbox, so there is no companion mint.
     */
    suspend fun applyPendingWebdavEnable() {
        if (!host.consumePendingWebdavEnable()) return
        try {
            api.setWebdavEnabled(true)
        } catch (e: Exception) {
            android.util.Log.w(
                "MailEnableGlue",
                "post-onboarding set_webdav_enabled failed (recoverable via admin-files): ${e.message}",
            )
            ShellLog.w("MailEnableGlue", "post-onboarding set_webdav_enabled failed (recoverable via admin-files): ${e.message}")
        }
    }

    /**
     * Opportunistically refresh the published mail content-sealing epoch
     * schedule (`docs/goal/architecture/encryption-at-rest.md` § Capability
     * tiering → *Content-sealing epochs*). Fired at the same universal
     * post-auth hook as [PostAuthGlueVM.runDeploymentSeedCustodyLeg]:
     * best-effort, fire-and-forget, idempotent nest-side — a no-op when mail
     * isn't enabled. Without it a schedule published at enable-mail
     * time slides stale past the publish horizon for a user who never
     * re-runs enable/rotate; the design's degradation (seal under the newest
     * published epoch — still correct, just coarser) covers that gap safely
     * in the meantime, so a failure here is never surfaced to the user.
     */
    suspend fun refreshMailEpochSchedule() {
        try {
            api.refreshMailEpochSchedule()
            ShellLog.i("EpochScheduleGlue", "refreshMailEpochSchedule: ok")
        } catch (e: Exception) {
            ShellLog.w("EpochScheduleGlue", "refreshMailEpochSchedule failed: ${e.message}")
        }
    }

    /**
     * Run the feeders that have no page of their own (`critical-alerts.md`
     * § Mechanism → *Who runs the detector*) at the same universal post-auth
     * hook as [PostAuthGlueVM.runDeploymentSeedCustodyLeg] — immediately, then every
     * `RE_SWEEP_INTERVAL_SECS` for as long as the identity lives.
     *
     * Launched on [CriticalAlertsHost]'s own process-wide scope, not this
     * function's caller: the loop never returns under normal operation (it
     * stops itself only at [CriticalAlertsHost.clearAll], the identity-
     * teardown boundary), and this VM's own scope dies with the Composable
     * that hosts it — a scope mismatch that would silently kill the loop far
     * earlier than intended. `startSweepLoop` itself is fire-and-forget, so
     * this function returns immediately; a thrown error inside the loop is
     * logged by the shared crate, not here.
     */
    fun runCriticalAlertSweepLoop() {
        criticalAlertsHost.startSweepLoop {
            try {
                api.runCriticalAlertSweepLoop()
            } catch (e: Exception) {
                ShellLog.w("CriticalAlertSweepGlue", "runCriticalAlertSweepLoop failed: ${e.message}")
            }
        }
    }

}
