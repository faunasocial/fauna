package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.ui.util.machinePossibility
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.AliasPolicyView
import uniffi.fauna_client_mail_settings.AuthPolicyView
import uniffi.fauna_client_mail_settings.DmarcPolicyView
import uniffi.fauna_client_mail_settings.ImapPolicyView
import uniffi.fauna_client_mail_settings.MailPolicyAction
import uniffi.fauna_client_mail_settings.MailPolicyConfirm
import uniffi.fauna_client_mail_settings.MailPolicyMachine
import uniffi.fauna_client_mail_settings.MailPolicySnapshot
import uniffi.fauna_client_mail_settings.MailPolicyStatus
import uniffi.fauna_client_mail_settings.MicrosoftRegistrationView
import uniffi.fauna_client_mail_settings.OutboundPolicyView
import uniffi.fauna_client_mail_settings.SpamPolicyView
import uniffi.fauna_client_mail_settings.SubmissionPolicyView
import uniffi.fauna_core.ReadState
import javax.inject.Inject

/**
 * Renders the shared `MailPolicyMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the flat admin `admin-mail` policy page — the mail-enable toggle +
 * the five projected policy sub-structs + the nest-side alias-policy group, each
 * a full-PUT save. Per priority #2 this view-model holds **no** policy logic; it
 * owns the machine, mirrors its snapshot into a StateFlow, and dispatches actions.
 * All seven write paths are live nest WS-RPC kinds (`mail-policy-config.md`
 * § Implementation status today); nest rejections surface via `snapshot.error` →
 * the global error-message banner, never faked green. Linux lead:
 * apps/fauna-linux/src/settings/admin_mail.rs.
 */
@HiltViewModel
class AdminMailVM @Inject constructor(
    private val api: ApiClient,
    @ApplicationContext context: Context,
) : ViewModel() {

    private val machine: MailPolicyMachine? = api.buildMailPolicyMachine()

    // Every action but the health re-check writes what the policy read decides
    // (admin.md § Errors & edge cases, *Controls wait for their read*): the
    // shared `MailPolicyMachine::write_possibility` — still loading, failed to
    // load, still saving.
    private val write = ReadGatedWrite(context) { machinePossibility(machine) { it.writePossibility() } }

    /** What every write control on the page is painted from. */
    val writeAnswer: StateFlow<uniffi.fauna_core.Possibility> = write.answer

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    /** A return by Back to this page reads it again (`AdminEntryReread`). */
    val entryReread = AdminEntryReread(::hydrate)

    init {
        hydrate()
        // A read that has not answered is read again when the nest connection
        // comes back (`docs/goal/behavior/admin.md` § Errors & edge cases,
        // *Controls wait for their read*; tui's `reconnect_reread_op`): only
        // while the shared machine's `read_state` is unresolved, so a draft
        // over an answered page survives.
        viewModelScope.rereadOnReconnect(
            api.reconnectTick,
            answered = { machine?.readState() == ReadState.RESOLVED },
            reread = ::hydrate,
        )
    }

    /** Load the effective config (`get_mail_config` + `get_alias_policy`); retry
     *  while the socket comes up. */
    private fun hydrate() {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.hydrate()
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that has not landed yet
                // (NestClient::request_inner), and the machine records any real failure
                // in its own snapshot, which the publish below surfaces.
            }
            publish(m)
        }
    }

    fun setMailEnabled(enabled: Boolean) =
        dispatch(MailPolicyAction.SetMailEnabled(enabled = enabled))

    /** `admin-mail-health-recheck-button`: a fresh blocklist self-check + diagnostics
     *  run, then a health re-read. */
    fun recheckHealth() = dispatch(MailPolicyAction.RecheckHealth, gated = false)

    /** One press of a same-button two-click confirm (the warm-up reset, the two
     *  secret rotations): the shared machine arms it, or — when it is the armed
     *  one — runs its act; the screen keeps no arm of its own. */
    fun press(button: MailPolicyConfirm) = dispatch(MailPolicyAction.Press(button = button))

    fun saveSpam(policy: SpamPolicyView) = dispatch(MailPolicyAction.SaveSpam(policy = policy))
    fun saveAuth(policy: AuthPolicyView) = dispatch(MailPolicyAction.SaveAuth(policy = policy))
    fun saveSubmission(policy: SubmissionPolicyView) = dispatch(MailPolicyAction.SaveSubmission(policy = policy))
    fun saveImap(policy: ImapPolicyView) = dispatch(MailPolicyAction.SaveImap(policy = policy))
    fun saveOutbound(policy: OutboundPolicyView) = dispatch(MailPolicyAction.SaveOutbound(policy = policy))
    fun saveAlias(policy: AliasPolicyView) = dispatch(MailPolicyAction.SaveAlias(policy = policy))
    fun saveDmarc(policy: DmarcPolicyView) = dispatch(MailPolicyAction.SaveDmarc(policy = policy))

    /** `admin-mail-microsoft-save-button`: the two drafts go out as typed; the
     *  shared machine checks them (both blank clears) and refuses a bad pair onto
     *  `snapshot.error` before any round trip. */
    fun saveMicrosoftRegistration(clientId: String, tenant: String) =
        dispatch(MailPolicyAction.SetMicrosoftRegistration(clientId = clientId, tenant = tenant))


    /** The standing-publish toggle's own gesture: the machine full-PUTs the
     *  persisted spam group with the one field flipped and re-reads. */
    fun setBaselineStandingPublish(enabled: Boolean) =
        dispatch(MailPolicyAction.SetBaselineStandingPublish(enabled = enabled))

    private fun dispatch(action: MailPolicyAction, gated: Boolean = true) {
        // The press asks the answer its control was painted from, live: a press
        // racing the read, or one while an earlier write is on its way, is
        // refused with its reason on `error-message` and nothing is sent.
        if (gated && !write.begin { errorMessage.value = it }) return
        val m = machine ?: return if (gated) write.end() else Unit
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            if (gated) write.end()
            publish(m)
        }
    }

    private fun publish(m: MailPolicyMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
        write.refresh()
    }

    private companion object {

        // Placeholder used only when the nest socket is absent (machine == null);
        // a live machine returns the catalog-default snapshot, then hydrate()
        // replaces it with the effective config.
        val EMPTY_SNAPSHOT = MailPolicySnapshot(
            mailEnabled = false,
            // Neutral placeholder (no nest socket); a live machine returns the
            // catalog default (default-on) and hydrate() replaces it.
            autoEnableMailForNewUsers = false,
            spam = SpamPolicyView(
                maxScoreBeforeSpamFolder = 0u,
                maxScoreBeforeReject = 0u,
                dnsblServers = emptyList(),
                rejectNoRdns = false,
                greylistEnabled = false,
                greylistDelaySecs = 0u,
                maxConnPerMin = 0u,
                fcrdnsMode = "off",
                heloIdentityRequired = false,
                rejectFcrdnsFail = false,
                maxMessageBytes = 0u,
                bayesianWeightMilli = 0u,
                bayesianMinSamples = 0u,
                bayesianFullConfidenceSamples = 0u,
                trainingHistoryRetentionDays = 0u,
                unlistedRecipientPenalty = 0u,
                baselineStandingPublish = false,
            ),
            auth = AuthPolicyView(
                enforceDmarc = false,
                enforceDmarcQuarantine = false,
                enforceSpfHardfail = false,
                enforceDkim = false,
                logOnly = false,
                maxAuthFailuresPerMinute = 0u,
                maxConnPerIp = 0u,
            ),
            submission = SubmissionPolicyView(maxPerDay = 0u, maxRecipientsPerMessage = 0u),
            imap = ImapPolicyView(
                idleTimeoutSecs = 0u,
                tombstoneRetentionDays = 0u,
                deleteNonempty = "forbidden",
                bodystructureCacheMax = 0u,
                storageBytesDefault = 0uL,
                messageCountDefault = 0u,
            ),
            outbound = OutboundPolicyView(
                retryScheduleSeconds = emptyList(),
                permanentFailureTimeoutHours = 0u,
                delayWarningAtHours = 0u,
                ndrRateLimitDays = 0u,
                suppressNdrSpfHardfail = false,
                suppressNdrDmarcReject = false,
                postmasterCcBounces = false,
                tlsrptSendReports = false,
                ipv6Enabled = false,
                treat5xxAsTransient = emptyList(),
            ),
            alias = AliasPolicyView(
                exactAliasesMax = 0u,
                reservedLocalParts = emptyList(),
                subaddressingEnabled = false,
                wildcardPrefixEnabled = false,
            ),
            // Pre-hydration placeholder, like every group above: the first
            // `Refresh` replaces it with the nest's `get_dmarc_policy` answer.
            dmarc = DmarcPolicyView(
                policyMode = "reject",
                subdomainPolicyMode = "reject",
                pct = 100u.toUByte(),
                adkimMode = "s",
                aspfMode = "s",
                ruaDestination = null,
                rufPublish = false,
                rufDestination = null,
                foMode = "0",
                riSeconds = 86400u,
                aggregateRetentionDays = 90u,
                forensicRetentionDays = 30u,
                outboundAggregateEnabled = true,
            ),
            // Pre-hydration placeholder: the project's registration, until the
            // first `Refresh` reads `fauna.setup.status`.
            microsoftRegistration = MicrosoftRegistrationView(
                clientId = "",
                tenant = "",
                isDefault = true,
            ),
            status = MailPolicyStatus.IDLE,
            baselineState = null,
            baselinePublishResult = null,
            health = null,
            error = null,
            armed = null,
            secretRotation = null,
        )
    }
}
