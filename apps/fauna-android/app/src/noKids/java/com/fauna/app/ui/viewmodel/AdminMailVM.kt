package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.AliasPolicyView
import uniffi.fauna_client_mail_settings.AuthPolicyView
import uniffi.fauna_client_mail_settings.ImapPolicyView
import uniffi.fauna_client_mail_settings.MailPolicyAction
import uniffi.fauna_client_mail_settings.MailPolicyMachine
import uniffi.fauna_client_mail_settings.MailPolicySnapshot
import uniffi.fauna_client_mail_settings.MailPolicyStatus
import uniffi.fauna_client_mail_settings.OutboundPolicyView
import uniffi.fauna_client_mail_settings.SpamPolicyView
import uniffi.fauna_client_mail_settings.SubmissionPolicyView
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
) : ViewModel() {

    private val machine: MailPolicyMachine? = api.buildMailPolicyMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    init {
        hydrate()
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

    fun saveSpam(policy: SpamPolicyView) = dispatch(MailPolicyAction.SaveSpam(policy = policy))
    fun saveAuth(policy: AuthPolicyView) = dispatch(MailPolicyAction.SaveAuth(policy = policy))
    fun saveSubmission(policy: SubmissionPolicyView) = dispatch(MailPolicyAction.SaveSubmission(policy = policy))
    fun saveImap(policy: ImapPolicyView) = dispatch(MailPolicyAction.SaveImap(policy = policy))
    fun saveOutbound(policy: OutboundPolicyView) = dispatch(MailPolicyAction.SaveOutbound(policy = policy))
    fun saveAlias(policy: AliasPolicyView) = dispatch(MailPolicyAction.SaveAlias(policy = policy))

    /** Publish the opt-in aggregate as the deployment baseline; the outcome lands
     *  in `snapshot.baselinePublishResult`. */
    fun publishSpamBaseline() = dispatch(MailPolicyAction.PublishSpamBaseline)

    private fun dispatch(action: MailPolicyAction) {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            publish(m)
        }
    }

    private fun publish(m: MailPolicyMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
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
            status = MailPolicyStatus.IDLE,
            baselineState = null,
            baselinePublishResult = null,
            health = null,
            error = null,
        )
    }
}
