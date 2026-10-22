package com.fauna.app.ui.screen.settings

import com.fauna.app.ui.navigation.exitAdminToPrimary
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.FaunaGateVerdict
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.AdminMailVM
import uniffi.fauna_client_mail_settings.AliasPolicyView
import uniffi.fauna_client_mail_settings.AuthPolicyView
import uniffi.fauna_client_mail_settings.BaselinePublishView
import uniffi.fauna_client_mail_settings.BaselineStateView
import uniffi.fauna_client_mail_settings.DmarcPolicyView
import uniffi.fauna_client_mail_settings.ImapPolicyView
import uniffi.fauna_client_mail_settings.MailHealthView
import uniffi.fauna_client_mail_settings.MailPolicyConfirm
import uniffi.fauna_client_mail_settings.MailPolicyStatus
import uniffi.fauna_client_mail_settings.MicrosoftRegistrationView
import uniffi.fauna_client_mail_settings.OutboundPolicyView
import uniffi.fauna_client_mail_settings.SecretRotationView
import uniffi.fauna_client_mail_settings.SpamPolicyView
import uniffi.fauna_client_mail_settings.SubmissionPolicyView
import uniffi.fauna_client_mail_settings.dmarcForensicModeOptions
import uniffi.fauna_client_mail_settings.dmarcForensicRetentionWarns
import uniffi.fauna_client_mail_settings.fcrdnsModeOptions
import uniffi.fauna_client_mail_settings.imapDeleteNonemptyOptions
import uniffi.fauna_client_mail_settings.secretRotationText
import uniffi.fauna_client_mail_settings.senderDashboardLinks
import uniffi.fauna_core.LocalizedText
import social.fauna.generated.Ids

/**
 * The flat admin `admin-mail` policy page (`admin.md` § 6 Mail;
 * `mail-policy-config.md` § Policy catalog Tier 2). Renders **only** the genuine
 * admin-choice knobs that have a live `fauna.bridges.put_*` / `set_mail_enabled`
 * write-path — automatic concerns (DKIM/TLS/MTA-STS/DMARC-publish/scanning/
 * deliverability) have NO manual UI here. Scope is every live write-path group:
 * the mail-enable toggle + the five projected policy sub-structs (spam / auth /
 * submission / imap / outbound) + the nest-side alias-policy group.
 *
 * Stateless [AdminMailContent] is split out for the Compose test harness; the
 * VM-bound [AdminMailScreen] is the wrapper the NavHost mounts as an admin
 * sub-page. Dumb renderer of the shared `MailPolicyMachine`
 * (libs/fauna-client-mail-settings, over UniFFI) — no policy logic in the shell
 * (priority #2). Each save button issues a full PUT of its whole sub-struct; nest
 * rejects (e.g. `fauna.protocol.malformed` on out-of-order spam thresholds)
 * surface via the global error-message banner, never faked green. Mirrors the
 * Linux lead (apps/fauna-linux/src/settings/admin_mail.rs).
 */
@Composable
fun AdminMailScreen(
    navController: NavController,
    vm: AdminMailVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    AdminMailContent(
        mailEnabled = snapshot.mailEnabled,
        mailUnavailableHere = snapshot.mailUnavailableHere,
        spam = snapshot.spam,
        auth = snapshot.auth,
        submission = snapshot.submission,
        imap = snapshot.imap,
        outbound = snapshot.outbound,
        alias = snapshot.alias,
        dmarc = snapshot.dmarc,
        microsoft = snapshot.microsoftRegistration,
        baselinePublishResult = snapshot.baselinePublishResult,
        baselineState = snapshot.baselineState,
        health = snapshot.health,
        onRecheckHealth = vm::recheckHealth,
        armed = snapshot.armed,
        secretRotation = snapshot.secretRotation,
        onPress = vm::press,
        working = snapshot.status == MailPolicyStatus.WORKING,
        onBack = { navController.exitAdminToPrimary() },
        onSetMailEnabled = vm::setMailEnabled,
        onSaveSpam = vm::saveSpam,
        onSaveAuth = vm::saveAuth,
        onSaveSubmission = vm::saveSubmission,
        onSaveImap = vm::saveImap,
        onSaveOutbound = vm::saveOutbound,
        onSaveAlias = vm::saveAlias,
        onSaveDmarc = vm::saveDmarc,
        onSaveMicrosoft = vm::saveMicrosoftRegistration,
        onSetBaselineStanding = vm::setBaselineStandingPublish,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminMailContent(
    mailEnabled: Boolean,
    // `MailPolicySnapshot.mailUnavailableHere` — read, never re-derived: while mail
    // is off the master toggle is disabled with state `unavailable`, the notice
    // beneath it (admin.md § 6 Mail).
    mailUnavailableHere: Boolean = false,
    spam: SpamPolicyView,
    auth: AuthPolicyView,
    submission: SubmissionPolicyView,
    imap: ImapPolicyView,
    outbound: OutboundPolicyView,
    alias: AliasPolicyView,
    dmarc: DmarcPolicyView,
    microsoft: MicrosoftRegistrationView,
    working: Boolean,
    onBack: () -> Unit,
    onSetMailEnabled: (Boolean) -> Unit,
    onSaveSpam: (SpamPolicyView) -> Unit,
    onSaveAuth: (AuthPolicyView) -> Unit,
    onSaveSubmission: (SubmissionPolicyView) -> Unit,
    onSaveImap: (ImapPolicyView) -> Unit,
    onSaveOutbound: (OutboundPolicyView) -> Unit,
    onSaveAlias: (AliasPolicyView) -> Unit,
    onSaveDmarc: (DmarcPolicyView) -> Unit,
    onSaveMicrosoft: (clientId: String, tenant: String) -> Unit,
    // Outcome of the last PublishSpamBaseline action (null until published). A
    // default keeps the Compose test-harness call site unchanged. The publish button
    // is a same-button two-press through `onPress` / `armed`, like the other confirms.
    baselinePublishResult: BaselinePublishView? = null,
    // The served baseline's state (null = blank text) and the standing-publish
    // toggle's own gesture — `SetBaselineStandingPublish`, never a Spam-group draft.
    baselineState: BaselineStateView? = null,
    onSetBaselineStanding: (Boolean) -> Unit = {},
    // Local `YYYY-MM-DD` of an epoch-ms instant — the shared
    // `fauna_core::format::format_unix_local_date_ms`, injected FFI-free for the
    // Compose test like the parse lambdas below.
    formatDate: (Long) -> String = { com.fauna.ffi.formatUnixLocalDateMs(it) },
    // The mail health readout (null until the first read lands ⇒ no section).
    // Defaults keep the Compose test-harness call site unchanged.
    health: MailHealthView? = null,
    onRecheckHealth: () -> Unit = {},
    // The same-button two-click confirms (warm-up reset, the two secret
    // rotations) are one shared `Press`: the machine holds the arm
    // (`snapshot.armed`) and the page keeps none; `secretRotation` is the last
    // rotation's outcome, worded by the shared `secretRotationText`.
    armed: MailPolicyConfirm? = null,
    secretRotation: SecretRotationView? = null,
    onPress: (MailPolicyConfirm) -> Unit = {},
    // Mail-knob validation via the shared `fauna_core::format::parse_count`
    // (`u32`) and `parse_count_u64` (`u64`, the IMAP storage ceiling): trim +
    // parse; a `null` return ⇒ keep the persisted value (a full-PUT save never
    // silently zeroes a knob). Injected as FFI-free lambdas so the Compose test
    // stays off the native path (mirrors the `parsePort` admin-port leg).
    // value-formatting.md § Mail-knob validation.
    parseCount: (String) -> UInt? = { com.fauna.ffi.parseCount(it) },
    parseCountU64: (String) -> ULong? = { com.fauna.ffi.parseCountU64(it) },
    // Whether a forensic-report retention earns the page's PII warning — the
    // shared `dmarc_forensic_retention_warns`, injected FFI-free like the parse
    // lambdas so the Compose test stays off the native path.
    dmarcRetentionWarns: (UInt) -> Boolean = { dmarcForensicRetentionWarns(it) },
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_mail_page_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(
                        onClick = onBack,
                        modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
                    }
                },
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.admin_mail_page_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // ── Mail enable (deployment-wide master toggle) ──
            // A dispatch-on-change commit: flipping it IS the
            // `fauna.bridges.set_mail_enabled` call, so it declares. ⚠ Gated at
            // the CALL SITE, never inside [ToggleRow] — that composable serves
            // this one commit and sixteen pure BUFFERS (every per-policy boolean
            // below, which only sets a local `var` that rides a later save), so a
            // gate inside it would grey sixteen drafts the admin must still be
            // able to edit with no nest. Same one-composable-two-roles split the
            // admin-users `TierDropdown` records.
            val mailEnabledGate = faunaGate("fauna.bridges.set_mail_enabled", enabled = !working)
            // A mailbox already on keeps its live toggle so it can still be turned off.
            val mailUnavailable = !mailEnabled && mailUnavailableHere
            ToggleRow(
                label = stringResource(R.string.admin_mail_page_enabled_label),
                subtitle = if (mailUnavailable) null else stringResource(R.string.admin_mail_page_enabled_subtitle),
                checked = mailEnabled,
                enabled = mailEnabledGate.enabled && !mailUnavailable,
                onChange = onSetMailEnabled,
                testTag = Ids.ADMIN_MAIL_ENABLED_TOGGLE,
                state = if (mailEnabled) "on" else if (mailUnavailable) "unavailable" else "off",
            )
            if (mailUnavailable) {
                Text(
                    stringResource(R.string.mail_settings_unavailable_notice),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.ADMIN_MAIL_UNAVAILABLE_NOTICE),
                )
            }
            DisabledControlReasonText(mailEnabledGate.reason)

            health?.let { HealthGroup(it, working, armed, onRecheckHealth, onPress) }

            SpamGroup(
                spam, working, onSaveSpam, parseCount, baselinePublishResult, armed, onPress,
                baselineState, onSetBaselineStanding, formatDate,
            )
            AuthGroup(auth, working, onSaveAuth, parseCount)
            SubmissionGroup(submission, working, onSaveSubmission, parseCount)
            ImapGroup(imap, working, onSaveImap, parseCount, parseCountU64)
            OutboundGroup(outbound, working, onSaveOutbound, parseCount)
            AliasGroup(alias, working, onSaveAlias, parseCount)
            DmarcGroup(dmarc, working, onSaveDmarc, parseCount, dmarcRetentionWarns)
            MicrosoftGroup(microsoft, working, onSaveMicrosoft)
            SecretsGroup(working, armed, secretRotation, onPress)
        }
    }
}

// ── Group composables ─────────────────────────────────────────────────────────

/**
 * The mail health readout (`mail-deliverability.md` § The mail health readout) —
 * read-only, a dumb render of the shared fold: the status line, the seven
 * indexed check rows, the conditional delist link and the two action buttons.
 * Every sentence is shared Rust (`mailHealthStateLabel` / `mailHealthCheckStateLabel`
 * / `mailHealthHeartbeatLabel`) or the nest's own row detail; rows 5 and 6 are the
 * heartbeats, whose empty detail the stamp fills. The warm-up reset is a
 * same-button two-click confirm the shared machine holds (`armed`); the button
 * only relabels and presses.
 */
@Composable
private fun HealthGroup(
    health: MailHealthView,
    working: Boolean,
    armed: MailPolicyConfirm?,
    onRecheck: () -> Unit,
    onPress: (MailPolicyConfirm) -> Unit,
) {
    val context = LocalContext.current
    val uriHandler = LocalUriHandler.current
    val nowMs = System.currentTimeMillis()
    fun heartbeat(atSecs: Long?): String =
        ValueFormat.render(context, com.fauna.ffi.mailHealthHeartbeatLabel(atSecs, nowMs), nowMs)

    val statusLine = stringResourceFmt(
        R.string.admin_mail_page_health_status_line,
        localized(com.fauna.ffi.mailHealthStateLabel(health.state)) ?: "",
        heartbeat(health.lastOutboundDeliveredAt),
        heartbeat(health.lastInboundAcceptedAt),
    )

    Column(
        modifier = Modifier.testTag(Ids.ADMIN_MAIL_HEALTH_SECTION),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            stringResource(R.string.admin_mail_page_health_title),
            style = MaterialTheme.typography.titleMedium,
        )
        Text(statusLine, modifier = Modifier.testTag(Ids.ADMIN_MAIL_HEALTH_STATUS))
        health.checks.forEachIndexed { i, check ->
            val detail = when (i) {
                5 -> heartbeat(health.lastOutboundDeliveredAt)
                6 -> heartbeat(health.lastInboundAcceptedAt)
                else -> check.detail
            }
            Row(
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_MAIL_HEALTH_CHECK),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    resolveLocalized(context, LocalizedText(check.labelKey, emptyMap())) ?: "",
                    modifier = Modifier.weight(2f).testTag(Ids.ADMIN_MAIL_HEALTH_CHECK_LABEL),
                    style = MaterialTheme.typography.labelLarge,
                )
                Text(
                    localized(com.fauna.ffi.mailHealthCheckStateLabel(check.state)) ?: "",
                    modifier = Modifier.weight(1f).testTag(Ids.ADMIN_MAIL_HEALTH_CHECK_STATE),
                )
                Text(
                    detail,
                    modifier = Modifier.weight(3f).testTag(Ids.ADMIN_MAIL_HEALTH_CHECK_DETAIL),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        health.delistUrl?.let { url ->
            OutlinedButton(
                onClick = { uriHandler.openUri(url) },
                // The link's target, readable as its `href` attribute (this
                // app's one string-attribute carrier): the text is the label.
                modifier = Modifier.testTag(Ids.ADMIN_MAIL_HEALTH_DELIST_LINK)
                    .semantics { stateDescription = url },
            ) { Text(stringResource(R.string.admin_mail_page_health_delist)) }
        }
        val recheckGate = faunaGate("fauna.bridges.blocklist_self_check_run", enabled = !working)
        OutlinedButton(
            onClick = onRecheck,
            enabled = recheckGate.enabled,
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_HEALTH_RECHECK_BUTTON),
        ) { Text(stringResource(R.string.admin_mail_page_health_recheck)) }
        DisabledControlReasonText(recheckGate.reason)
        val resetGate = faunaGate("fauna.bridges.outbound_warmup_reset", enabled = !working)
        OutlinedButton(
            onClick = { onPress(MailPolicyConfirm.RESET_WARMUP) },
            enabled = resetGate.enabled,
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_HEALTH_WARMUP_RESET_BUTTON),
        ) {
            Text(
                stringResource(
                    if (armed == MailPolicyConfirm.RESET_WARMUP) R.string.admin_mail_page_health_warmup_reset_confirm
                    else R.string.admin_mail_page_health_warmup_reset,
                ),
            )
        }
        DisabledControlReasonText(resetGate.reason)
        // The sender-dashboard note (static help) and its two links, one per
        // receiver in the shared order; each link publishes its URL as its
        // `href` (stateDescription), as the delist link does.
        Text(
            stringResource(R.string.admin_mail_page_health_sender_dashboards_note),
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_HEALTH_SENDER_DASHBOARDS_NOTE),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        for (link in remember { senderDashboardLinks() }) {
            OutlinedButton(
                onClick = { uriHandler.openUri(link.url) },
                modifier = Modifier.testTag(Ids.ADMIN_MAIL_HEALTH_SENDER_DASHBOARD_LINK)
                    .semantics { stateDescription = link.url },
            ) {
                Text(
                    stringResource(
                        if (link.key == "gmail") R.string.admin_mail_page_health_sender_dashboard_gmail
                        else R.string.admin_mail_page_health_sender_dashboard_outlook,
                    ),
                )
            }
        }
    }
}

/**
 * The Secrets group (`mail-mass-mailing.md` § Secret rotation,
 * `mail-forwarding.md` § SRS scheme): the two rotate buttons, each the
 * same-button two-click the shared machine holds — an armed button relabels to
 * its confirm sentence — then the last rotation's outcome through the shared
 * `secretRotationText`.
 */
@Composable
private fun SecretsGroup(
    working: Boolean,
    armed: MailPolicyConfirm?,
    secretRotation: SecretRotationView?,
    onPress: (MailPolicyConfirm) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            stringResource(R.string.admin_mail_page_secrets_group_title),
            style = MaterialTheme.typography.titleMedium,
        )
        Text(
            stringResource(R.string.admin_mail_page_secrets_group_desc),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        val unsubscribeGate =
            faunaGate("fauna.bridges.rotate_list_unsubscribe_secret", enabled = !working)
        OutlinedButton(
            onClick = { onPress(MailPolicyConfirm.ROTATE_LIST_UNSUBSCRIBE_SECRET) },
            enabled = unsubscribeGate.enabled,
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_LIST_UNSUBSCRIBE_SECRET_ROTATE_BUTTON),
        ) {
            Text(
                stringResource(
                    if (armed == MailPolicyConfirm.ROTATE_LIST_UNSUBSCRIBE_SECRET) {
                        R.string.admin_mail_page_list_unsubscribe_secret_rotate_confirm
                    } else {
                        R.string.admin_mail_page_list_unsubscribe_secret_rotate
                    },
                ),
            )
        }
        DisabledControlReasonText(unsubscribeGate.reason)
        val srsGate = faunaGate("fauna.bridges.rotate_srs_secret", enabled = !working)
        OutlinedButton(
            onClick = { onPress(MailPolicyConfirm.ROTATE_SRS_SECRET) },
            enabled = srsGate.enabled,
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_FORWARD_SRS_SECRET_ROTATE_BUTTON),
        ) {
            Text(
                stringResource(
                    if (armed == MailPolicyConfirm.ROTATE_SRS_SECRET) {
                        R.string.admin_mail_page_forward_srs_secret_rotate_confirm
                    } else {
                        R.string.admin_mail_page_forward_srs_secret_rotate
                    },
                ),
            )
        }
        DisabledControlReasonText(srsGate.reason)
        secretRotation?.let {
            Text(
                localized(secretRotationText(it)) ?: "",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun SpamGroup(
    initial: SpamPolicyView,
    working: Boolean,
    onSave: (SpamPolicyView) -> Unit,
    parseCount: (String) -> UInt?,
    baselineResult: BaselinePublishView?,
    armed: MailPolicyConfirm?,
    onPress: (MailPolicyConfirm) -> Unit,
    baselineState: BaselineStateView?,
    onSetBaselineStanding: (Boolean) -> Unit,
    formatDate: (Long) -> String,
) {
    var junk by remember(initial) { mutableStateOf(initial.maxScoreBeforeSpamFolder.toString()) }
    var reject by remember(initial) { mutableStateOf(initial.maxScoreBeforeReject.toString()) }
    var dnsbl by remember(initial) { mutableStateOf(initial.dnsblServers.joinToString("\n")) }
    var rejectNoRdns by remember(initial) { mutableStateOf(initial.rejectNoRdns) }
    var greylistEnabled by remember(initial) { mutableStateOf(initial.greylistEnabled) }
    var greylistDelay by remember(initial) { mutableStateOf(initial.greylistDelaySecs.toString()) }
    var maxConn by remember(initial) { mutableStateOf(initial.maxConnPerMin.toString()) }
    var fcrdns by remember(initial) { mutableStateOf(initial.fcrdnsMode) }
    var heloRequired by remember(initial) { mutableStateOf(initial.heloIdentityRequired) }
    var rejectFcrdnsFail by remember(initial) { mutableStateOf(initial.rejectFcrdnsFail) }
    var maxBytes by remember(initial) { mutableStateOf(initial.maxMessageBytes.toString()) }
    // Per-user training (Tier-2 combined-score knobs; same put_spam_policy).
    var bayesianWeight by remember(initial) { mutableStateOf(initial.bayesianWeightMilli.toString()) }
    var bayesianMinSamples by remember(initial) { mutableStateOf(initial.bayesianMinSamples.toString()) }
    var bayesianFullConfidence by remember(initial) { mutableStateOf(initial.bayesianFullConfidenceSamples.toString()) }
    var trainingRetention by remember(initial) { mutableStateOf(initial.trainingHistoryRetentionDays.toString()) }
    var unlistedRecipientPenalty by remember(initial) { mutableStateOf(initial.unlistedRecipientPenalty.toString()) }

    GroupCard(
        stringResource(R.string.admin_mail_page_spam_group_title),
        stringResource(R.string.admin_mail_page_spam_group_desc),
    ) {
        NumField(stringResource(R.string.admin_mail_page_threshold_junk_label), stringResource(R.string.admin_mail_page_threshold_junk_subtitle), junk, { junk = it }, "admin-mail-spam-threshold-junk")
        NumField(stringResource(R.string.admin_mail_page_threshold_reject_label), stringResource(R.string.admin_mail_page_threshold_reject_subtitle), reject, { reject = it }, "admin-mail-spam-threshold-reject")
        MultilineField(stringResource(R.string.admin_mail_page_dnsbl_label), stringResource(R.string.admin_mail_page_dnsbl_subtitle), dnsbl, { dnsbl = it }, "admin-mail-dnsbl-servers")
        ToggleRow(stringResource(R.string.admin_mail_page_reject_no_rdns_label), null, rejectNoRdns, !working, { rejectNoRdns = it }, "admin-mail-reject-no-rdns-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_greylist_enabled_label), null, greylistEnabled, !working, { greylistEnabled = it }, "admin-mail-greylist-enabled-toggle")
        NumField(stringResource(R.string.admin_mail_page_greylist_delay_label), null, greylistDelay, { greylistDelay = it }, "admin-mail-greylist-delay-input")
        NumField(stringResource(R.string.admin_mail_page_max_conn_per_min_label), null, maxConn, { maxConn = it }, "admin-mail-max-conn-per-min-input")
        SelectField(
            stringResource(R.string.admin_mail_page_fcrdns_mode_label),
            remember { fcrdnsModeOptions() }.map { it.value to (localized(it.label) ?: it.value) },
            fcrdns, { fcrdns = it }, "admin-mail-fcrdns-mode-select",
        )
        ToggleRow(stringResource(R.string.admin_mail_page_helo_identity_label), null, heloRequired, !working, { heloRequired = it }, "admin-mail-helo-identity-required-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_reject_fcrdns_fail_label), null, rejectFcrdnsFail, !working, { rejectFcrdnsFail = it }, "admin-mail-reject-fcrdns-fail-toggle")
        NumField(stringResource(R.string.admin_mail_page_max_message_bytes_label), null, maxBytes, { maxBytes = it }, "admin-mail-max-message-bytes-input")
        NumField(stringResource(R.string.admin_mail_page_bayesian_weight_label), stringResource(R.string.admin_mail_page_bayesian_weight_subtitle), bayesianWeight, { bayesianWeight = it }, "admin-mail-spam-bayesian-weight")
        NumField(stringResource(R.string.admin_mail_page_bayesian_min_samples_label), stringResource(R.string.admin_mail_page_bayesian_min_samples_subtitle), bayesianMinSamples, { bayesianMinSamples = it }, "admin-mail-spam-bayesian-min-samples")
        NumField(stringResource(R.string.admin_mail_page_bayesian_full_confidence_samples_label), stringResource(R.string.admin_mail_page_bayesian_full_confidence_samples_subtitle), bayesianFullConfidence, { bayesianFullConfidence = it }, "admin-mail-spam-bayesian-full-confidence-samples")
        NumField(stringResource(R.string.admin_mail_page_training_history_retention_label), stringResource(R.string.admin_mail_page_training_history_retention_subtitle), trainingRetention, { trainingRetention = it }, "admin-mail-spam-training-history-retention")
        NumField(stringResource(R.string.admin_mail_page_unlisted_recipient_penalty_label), stringResource(R.string.admin_mail_page_unlisted_recipient_penalty_subtitle), unlistedRecipientPenalty, { unlistedRecipientPenalty = it }, "admin-mail-unlisted-recipient-penalty")
        SaveButton(
            "admin-mail-spam-save-button",
            stringResource(R.string.admin_mail_page_spam_save),
            faunaGate("fauna.bridges.put_spam_policy", enabled = !working),
        ) {
            onSave(
                SpamPolicyView(
                    maxScoreBeforeSpamFolder = parseCount(junk) ?: initial.maxScoreBeforeSpamFolder,
                    maxScoreBeforeReject = parseCount(reject) ?: initial.maxScoreBeforeReject,
                    dnsblServers = splitLines(dnsbl),
                    rejectNoRdns = rejectNoRdns,
                    greylistEnabled = greylistEnabled,
                    greylistDelaySecs = parseCount(greylistDelay) ?: initial.greylistDelaySecs,
                    maxConnPerMin = parseCount(maxConn) ?: initial.maxConnPerMin,
                    fcrdnsMode = fcrdns,
                    heloIdentityRequired = heloRequired,
                    rejectFcrdnsFail = rejectFcrdnsFail,
                    maxMessageBytes = parseCount(maxBytes) ?: initial.maxMessageBytes,
                    bayesianWeightMilli = parseCount(bayesianWeight) ?: initial.bayesianWeightMilli,
                    bayesianMinSamples = parseCount(bayesianMinSamples) ?: initial.bayesianMinSamples,
                    bayesianFullConfidenceSamples = parseCount(bayesianFullConfidence) ?: initial.bayesianFullConfidenceSamples,
                    trainingHistoryRetentionDays = parseCount(trainingRetention) ?: initial.trainingHistoryRetentionDays,
                    unlistedRecipientPenalty = parseCount(unlistedRecipientPenalty) ?: initial.unlistedRecipientPenalty,
                    // Carried through so a save never turns the standing baseline
                    // publish off (which withdraws the baseline); the toggle below is
                    // its own gesture.
                    baselineStandingPublish = initial.baselineStandingPublish,
                ),
            )
        }
        // ── Deployment baseline (admin opt-in aggregate; publish_spam_baseline) ──
        val baselineGate = faunaGate("fauna.bridges.publish_spam_baseline", enabled = !working)
        Button(
            onClick = { onPress(MailPolicyConfirm.PUBLISH_SPAM_BASELINE) },
            enabled = baselineGate.enabled,
            modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_MAIL_PUBLISH_SPAM_BASELINE_BUTTON),
        ) {
            Text(
                stringResource(
                    if (armed == MailPolicyConfirm.PUBLISH_SPAM_BASELINE) {
                        R.string.admin_mail_page_publish_spam_baseline_confirm
                    } else {
                        R.string.admin_mail_page_publish_spam_baseline_button
                    },
                ),
            )
        }
        DisabledControlReasonText(baselineGate.reason)
        Text(
            stringResource(R.string.admin_mail_page_publish_spam_baseline_subtitle),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        // `published` comes straight from the shared BaselinePublishView, so the
        // client just picks the message + interpolates.
        Text(
            baselineResult?.let {
                // `deferred` first: a delta-floor deferral also carries
                // `published = false` and must never read as "too few contributors".
                val base = if (it.deferred) {
                    stringResource(R.string.admin_mail_page_spam_baseline_waiting)
                } else if (it.published) {
                    stringResourceFmt(R.string.admin_mail_page_spam_baseline_published, it.contributors, it.sampleCount)
                } else {
                    stringResourceFmt(R.string.admin_mail_page_spam_baseline_withheld, it.contributors)
                }
                // The holder-side erosion count (silent-erosion fix, mail-spam.md
                // § Encrypted-mode interaction) — surfaced beside the published/
                // withheld message whenever the last run skipped anyone.
                if (it.skippedContributors > 0u) {
                    base + " " + stringResourceFmt(
                        R.string.admin_mail_page_spam_baseline_skipped_contributors,
                        it.skippedContributors,
                    )
                } else {
                    base
                }
            } ?: "",
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_PUBLISH_SPAM_BASELINE_RESULT),
        )
        // Standing publish — dispatches immediately off the PERSISTED group (the
        // shared machine flips the one field and re-reads), so a gate on its wire
        // kind, never a draft buffer like the toggles above.
        val standingGate = faunaGate("fauna.bridges.put_spam_policy", enabled = !working)
        ToggleRow(
            label = stringResource(R.string.admin_mail_page_spam_baseline_standing_label),
            subtitle = stringResource(R.string.admin_mail_page_spam_baseline_standing_subtitle),
            checked = initial.baselineStandingPublish,
            enabled = standingGate.enabled,
            onChange = onSetBaselineStanding,
            testTag = Ids.ADMIN_MAIL_SPAM_BASELINE_STANDING_TOGGLE,
        )
        DisabledControlReasonText(standingGate.reason)
        // The served baseline's state: published / none, then the waiting note
        // when the last run was deferred. Blank until the state read answers.
        Text(
            baselineState?.let {
                val published = it.published && it.publishedAtMs != null
                val line = if (published) {
                    stringResourceFmt(
                        R.string.admin_mail_page_spam_baseline_state_published,
                        it.contributors,
                        formatDate(it.publishedAtMs!!),
                    )
                } else {
                    stringResource(R.string.admin_mail_page_spam_baseline_state_none)
                }
                if (it.deferred) line + " " + stringResource(R.string.admin_mail_page_spam_baseline_waiting) else line
            } ?: "",
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_SPAM_BASELINE_STATE),
        )
    }
}

@Composable
private fun AuthGroup(initial: AuthPolicyView, working: Boolean, onSave: (AuthPolicyView) -> Unit, parseCount: (String) -> UInt?) {
    var enforceDmarc by remember(initial) { mutableStateOf(initial.enforceDmarc) }
    var enforceDmarcQuarantine by remember(initial) { mutableStateOf(initial.enforceDmarcQuarantine) }
    var enforceSpfHardfail by remember(initial) { mutableStateOf(initial.enforceSpfHardfail) }
    var enforceDkim by remember(initial) { mutableStateOf(initial.enforceDkim) }
    var logOnly by remember(initial) { mutableStateOf(initial.logOnly) }
    var maxFailures by remember(initial) { mutableStateOf(initial.maxAuthFailuresPerMinute.toString()) }
    var maxConnPerIp by remember(initial) { mutableStateOf(initial.maxConnPerIp.toString()) }

    GroupCard(
        stringResource(R.string.admin_mail_page_auth_group_title),
        stringResource(R.string.admin_mail_page_auth_group_desc),
    ) {
        ToggleRow(stringResource(R.string.admin_mail_page_enforce_dmarc_label), null, enforceDmarc, !working, { enforceDmarc = it }, "admin-mail-auth-enforce-dmarc-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_enforce_dmarc_quarantine_label), null, enforceDmarcQuarantine, !working, { enforceDmarcQuarantine = it }, "admin-mail-auth-enforce-dmarc-quarantine-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_enforce_spf_hardfail_label), null, enforceSpfHardfail, !working, { enforceSpfHardfail = it }, "admin-mail-auth-enforce-spf-hardfail-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_enforce_dkim_label), null, enforceDkim, !working, { enforceDkim = it }, "admin-mail-auth-enforce-dkim-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_log_only_label), null, logOnly, !working, { logOnly = it }, "admin-mail-auth-log-only-toggle")
        NumField(stringResource(R.string.admin_mail_page_max_failures_label), null, maxFailures, { maxFailures = it }, "admin-mail-auth-max-failures-input")
        NumField(stringResource(R.string.admin_mail_page_max_conn_per_ip_label), null, maxConnPerIp, { maxConnPerIp = it }, "admin-mail-auth-max-conn-per-ip-input")
        SaveButton(
            "admin-mail-auth-save-button",
            stringResource(R.string.admin_mail_page_auth_save),
            faunaGate("fauna.bridges.put_auth_policy", enabled = !working),
        ) {
            onSave(
                AuthPolicyView(
                    enforceDmarc = enforceDmarc,
                    enforceDmarcQuarantine = enforceDmarcQuarantine,
                    enforceSpfHardfail = enforceSpfHardfail,
                    enforceDkim = enforceDkim,
                    logOnly = logOnly,
                    maxAuthFailuresPerMinute = parseCount(maxFailures) ?: initial.maxAuthFailuresPerMinute,
                    maxConnPerIp = parseCount(maxConnPerIp) ?: initial.maxConnPerIp,
                ),
            )
        }
    }
}

@Composable
private fun SubmissionGroup(initial: SubmissionPolicyView, working: Boolean, onSave: (SubmissionPolicyView) -> Unit, parseCount: (String) -> UInt?) {
    var maxPerDay by remember(initial) { mutableStateOf(initial.maxPerDay.toString()) }
    var maxRecipients by remember(initial) { mutableStateOf(initial.maxRecipientsPerMessage.toString()) }

    GroupCard(
        stringResource(R.string.admin_mail_page_submission_group_title),
        stringResource(R.string.admin_mail_page_submission_group_desc),
    ) {
        NumField(stringResource(R.string.admin_mail_page_submission_max_per_day_label), stringResource(R.string.admin_mail_page_submission_max_per_day_subtitle), maxPerDay, { maxPerDay = it }, "admin-mail-submission-max-per-day-input")
        NumField(stringResource(R.string.admin_mail_page_submission_max_recipients_label), stringResource(R.string.admin_mail_page_submission_max_recipients_subtitle), maxRecipients, { maxRecipients = it }, "admin-mail-submission-max-recipients-input")
        SaveButton(
            "admin-mail-submission-save-button",
            stringResource(R.string.admin_mail_page_submission_save),
            faunaGate("fauna.bridges.put_submission_policy", enabled = !working),
        ) {
            onSave(
                SubmissionPolicyView(
                    maxPerDay = parseCount(maxPerDay) ?: initial.maxPerDay,
                    maxRecipientsPerMessage = parseCount(maxRecipients) ?: initial.maxRecipientsPerMessage,
                ),
            )
        }
    }
}

@Composable
private fun ImapGroup(initial: ImapPolicyView, working: Boolean, onSave: (ImapPolicyView) -> Unit, parseCount: (String) -> UInt?, parseCountU64: (String) -> ULong?) {
    var idleTimeout by remember(initial) { mutableStateOf(initial.idleTimeoutSecs.toString()) }
    var tombstone by remember(initial) { mutableStateOf(initial.tombstoneRetentionDays.toString()) }
    var deleteNonempty by remember(initial) { mutableStateOf(initial.deleteNonempty) }
    var cacheMax by remember(initial) { mutableStateOf(initial.bodystructureCacheMax.toString()) }
    var storageBytes by remember(initial) { mutableStateOf(initial.storageBytesDefault.toString()) }
    var messageCount by remember(initial) { mutableStateOf(initial.messageCountDefault.toString()) }

    GroupCard(
        stringResource(R.string.admin_mail_page_imap_group_title),
        stringResource(R.string.admin_mail_page_imap_group_desc),
    ) {
        NumField(stringResource(R.string.admin_mail_page_imap_idle_timeout_label), stringResource(R.string.admin_mail_page_imap_idle_timeout_subtitle), idleTimeout, { idleTimeout = it }, "admin-mail-imap-idle-timeout-input")
        NumField(stringResource(R.string.admin_mail_page_imap_tombstone_retention_label), stringResource(R.string.admin_mail_page_imap_tombstone_retention_subtitle), tombstone, { tombstone = it }, "admin-mail-imap-tombstone-retention-input")
        SelectField(
            stringResource(R.string.admin_mail_page_imap_delete_nonempty_label),
            remember { imapDeleteNonemptyOptions() }.map { it.value to (localized(it.label) ?: it.value) },
            deleteNonempty, { deleteNonempty = it }, "admin-mail-imap-delete-nonempty-select",
        )
        NumField(stringResource(R.string.admin_mail_page_imap_bodystructure_cache_label), stringResource(R.string.admin_mail_page_imap_bodystructure_cache_subtitle), cacheMax, { cacheMax = it }, "admin-mail-imap-bodystructure-cache-input")
        NumField(stringResource(R.string.admin_mail_page_imap_storage_bytes_label), stringResource(R.string.admin_mail_page_imap_storage_bytes_subtitle), storageBytes, { storageBytes = it }, "admin-mail-imap-storage-bytes-input")
        NumField(stringResource(R.string.admin_mail_page_imap_message_count_label), stringResource(R.string.admin_mail_page_imap_message_count_subtitle), messageCount, { messageCount = it }, "admin-mail-imap-message-count-input")
        SaveButton(
            "admin-mail-imap-save-button",
            stringResource(R.string.admin_mail_page_imap_save),
            faunaGate("fauna.bridges.put_imap_policy", enabled = !working),
        ) {
            onSave(
                ImapPolicyView(
                    idleTimeoutSecs = parseCount(idleTimeout) ?: initial.idleTimeoutSecs,
                    tombstoneRetentionDays = parseCount(tombstone) ?: initial.tombstoneRetentionDays,
                    deleteNonempty = deleteNonempty,
                    bodystructureCacheMax = parseCount(cacheMax) ?: initial.bodystructureCacheMax,
                    storageBytesDefault = parseCountU64(storageBytes) ?: initial.storageBytesDefault,
                    messageCountDefault = parseCount(messageCount) ?: initial.messageCountDefault,
                ),
            )
        }
    }
}

@Composable
private fun OutboundGroup(initial: OutboundPolicyView, working: Boolean, onSave: (OutboundPolicyView) -> Unit, parseCount: (String) -> UInt?) {
    var retrySchedule by remember(initial) { mutableStateOf(initial.retryScheduleSeconds.joinToString("\n")) }
    var permfail by remember(initial) { mutableStateOf(initial.permanentFailureTimeoutHours.toString()) }
    var delayWarning by remember(initial) { mutableStateOf(initial.delayWarningAtHours.toString()) }
    var ndrRateLimit by remember(initial) { mutableStateOf(initial.ndrRateLimitDays.toString()) }
    var suppressSpf by remember(initial) { mutableStateOf(initial.suppressNdrSpfHardfail) }
    var suppressDmarc by remember(initial) { mutableStateOf(initial.suppressNdrDmarcReject) }
    var tlsrpt by remember(initial) { mutableStateOf(initial.tlsrptSendReports) }
    var ipv6 by remember(initial) { mutableStateOf(initial.ipv6Enabled) }
    var treat5xx by remember(initial) { mutableStateOf(initial.treat5xxAsTransient.joinToString("\n")) }

    GroupCard(
        stringResource(R.string.admin_mail_page_outbound_group_title),
        stringResource(R.string.admin_mail_page_outbound_group_desc),
    ) {
        MultilineField(stringResource(R.string.admin_mail_page_outbound_retry_schedule_label), stringResource(R.string.admin_mail_page_outbound_retry_schedule_subtitle), retrySchedule, { retrySchedule = it }, "admin-mail-outbound-retry-schedule")
        NumField(stringResource(R.string.admin_mail_page_outbound_permfail_timeout_label), stringResource(R.string.admin_mail_page_outbound_permfail_timeout_subtitle), permfail, { permfail = it }, "admin-mail-outbound-permfail-timeout-input")
        NumField(stringResource(R.string.admin_mail_page_outbound_delay_warning_label), stringResource(R.string.admin_mail_page_outbound_delay_warning_subtitle), delayWarning, { delayWarning = it }, "admin-mail-outbound-delay-warning-input")
        NumField(stringResource(R.string.admin_mail_page_outbound_ndr_rate_limit_label), stringResource(R.string.admin_mail_page_outbound_ndr_rate_limit_subtitle), ndrRateLimit, { ndrRateLimit = it }, "admin-mail-outbound-ndr-rate-limit-input")
        ToggleRow(stringResource(R.string.admin_mail_page_outbound_suppress_ndr_spf_label), null, suppressSpf, !working, { suppressSpf = it }, "admin-mail-outbound-suppress-ndr-spf-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_outbound_suppress_ndr_dmarc_label), null, suppressDmarc, !working, { suppressDmarc = it }, "admin-mail-outbound-suppress-ndr-dmarc-toggle")
        // Read-only — project policy never CCs the postmaster.
        ToggleRow(stringResource(R.string.admin_mail_page_outbound_postmaster_cc_label), stringResource(R.string.admin_mail_page_outbound_postmaster_cc_subtitle), initial.postmasterCcBounces, false, {}, "admin-mail-outbound-postmaster-cc-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_outbound_tlsrpt_send_label), null, tlsrpt, !working, { tlsrpt = it }, "admin-mail-outbound-tlsrpt-send-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_outbound_ipv6_label), null, ipv6, !working, { ipv6 = it }, "admin-mail-outbound-ipv6-toggle")
        MultilineField(stringResource(R.string.admin_mail_page_outbound_treat_5xx_label), stringResource(R.string.admin_mail_page_outbound_treat_5xx_subtitle), treat5xx, { treat5xx = it }, "admin-mail-outbound-treat-5xx-transient")
        SaveButton(
            "admin-mail-outbound-save-button",
            stringResource(R.string.admin_mail_page_outbound_save),
            faunaGate("fauna.bridges.put_outbound_policy", enabled = !working),
        ) {
            onSave(
                OutboundPolicyView(
                    retryScheduleSeconds = splitLines(retrySchedule).mapNotNull { it.toULongOrNull() },
                    permanentFailureTimeoutHours = parseCount(permfail) ?: initial.permanentFailureTimeoutHours,
                    delayWarningAtHours = parseCount(delayWarning) ?: initial.delayWarningAtHours,
                    ndrRateLimitDays = parseCount(ndrRateLimit) ?: initial.ndrRateLimitDays,
                    suppressNdrSpfHardfail = suppressSpf,
                    suppressNdrDmarcReject = suppressDmarc,
                    // Read-only; resubmit the persisted value unchanged.
                    postmasterCcBounces = initial.postmasterCcBounces,
                    tlsrptSendReports = tlsrpt,
                    ipv6Enabled = ipv6,
                    treat5xxAsTransient = splitLines(treat5xx),
                ),
            )
        }
    }
}

@Composable
private fun AliasGroup(initial: AliasPolicyView, working: Boolean, onSave: (AliasPolicyView) -> Unit, parseCount: (String) -> UInt?) {
    var exactMax by remember(initial) { mutableStateOf(initial.exactAliasesMax.toString()) }
    var reserved by remember(initial) { mutableStateOf(initial.reservedLocalParts.joinToString("\n")) }
    var subaddressing by remember(initial) { mutableStateOf(initial.subaddressingEnabled) }
    var wildcard by remember(initial) { mutableStateOf(initial.wildcardPrefixEnabled) }

    GroupCard(
        stringResource(R.string.admin_mail_page_alias_group_title),
        stringResource(R.string.admin_mail_page_alias_group_desc),
    ) {
        NumField(stringResource(R.string.admin_mail_page_alias_exact_max_label), stringResource(R.string.admin_mail_page_alias_exact_max_subtitle), exactMax, { exactMax = it }, "admin-mail-alias-exact-max-input")
        MultilineField(stringResource(R.string.admin_mail_page_alias_reserved_label), stringResource(R.string.admin_mail_page_alias_reserved_subtitle), reserved, { reserved = it }, "admin-mail-alias-reserved-local-parts")
        ToggleRow(stringResource(R.string.admin_mail_page_alias_subaddressing_label), stringResource(R.string.admin_mail_page_alias_subaddressing_subtitle), subaddressing, !working, { subaddressing = it }, "admin-mail-alias-subaddressing-toggle")
        ToggleRow(stringResource(R.string.admin_mail_page_alias_wildcard_prefix_label), stringResource(R.string.admin_mail_page_alias_wildcard_prefix_subtitle), wildcard, !working, { wildcard = it }, "admin-mail-alias-wildcard-prefix-toggle")
        SaveButton(
            "admin-mail-alias-save-button",
            stringResource(R.string.admin_mail_page_alias_save),
            faunaGate("fauna.bridges.put_alias_policy", enabled = !working),
        ) {
            onSave(
                AliasPolicyView(
                    exactAliasesMax = parseCount(exactMax) ?: initial.exactAliasesMax,
                    reservedLocalParts = splitLines(reserved),
                    subaddressingEnabled = subaddressing,
                    wildcardPrefixEnabled = wildcard,
                ),
            )
        }
    }
}

/**
 * The DMARC-reports group (`dmarc-reporting.md` § Forensic-report receive): four
 * edited fields over the persisted catalog; every other record tag (`p`, `sp`,
 * `pct`, `adkim`, `aspf`, `rua`, the `ruf` destination, `ri`) rides through
 * `copy`. A retention past its ceiling is sent as typed — the nest refuses it
 * onto the error banner, never clamped here.
 */
@Composable
private fun DmarcGroup(
    initial: DmarcPolicyView,
    working: Boolean,
    onSave: (DmarcPolicyView) -> Unit,
    parseCount: (String) -> UInt?,
    retentionWarns: (UInt) -> Boolean,
) {
    var rufPublish by remember(initial) { mutableStateOf(initial.rufPublish) }
    var foMode by remember(initial) { mutableStateOf(initial.foMode) }
    var aggregateRetention by remember(initial) { mutableStateOf(initial.aggregateRetentionDays.toString()) }
    var forensicRetention by remember(initial) { mutableStateOf(initial.forensicRetentionDays.toString()) }

    GroupCard(
        stringResource(R.string.admin_mail_page_dmarc_group_title),
        stringResource(R.string.admin_mail_page_dmarc_group_desc),
    ) {
        ToggleRow(stringResource(R.string.admin_mail_page_dmarc_ruf_publish_label), null, rufPublish, !working, { rufPublish = it }, Ids.ADMIN_MAIL_DMARC_RUF_PUBLISH_TOGGLE)
        Text(
            stringResource(R.string.admin_mail_page_dmarc_ruf_warning),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_DMARC_RUF_WARNING),
        )
        SelectField(
            stringResource(R.string.admin_mail_page_dmarc_fo_mode_label),
            remember { dmarcForensicModeOptions() }.map { it.value to (localized(it.label) ?: it.value) },
            foMode, { foMode = it }, Ids.ADMIN_MAIL_DMARC_FO_MODE_SELECT,
        )
        NumField(stringResource(R.string.admin_mail_page_dmarc_aggregate_retention_label), null, aggregateRetention, { aggregateRetention = it }, Ids.ADMIN_MAIL_DMARC_AGGREGATE_RETENTION_INPUT)
        NumField(stringResource(R.string.admin_mail_page_dmarc_forensic_retention_label), null, forensicRetention, { forensicRetention = it }, Ids.ADMIN_MAIL_DMARC_FORENSIC_RETENTION_INPUT)
        if (parseCount(forensicRetention)?.let(retentionWarns) == true) {
            Text(
                stringResource(R.string.admin_mail_page_dmarc_forensic_retention_warning),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        SaveButton(
            Ids.ADMIN_MAIL_DMARC_SAVE_BUTTON,
            stringResource(R.string.admin_mail_page_dmarc_save),
            faunaGate("fauna.bridges.put_dmarc_policy", enabled = !working),
        ) {
            onSave(
                initial.copy(
                    rufPublish = rufPublish,
                    foMode = foMode,
                    aggregateRetentionDays = parseCount(aggregateRetention) ?: initial.aggregateRetentionDays,
                    forensicRetentionDays = parseCount(forensicRetention) ?: initial.forensicRetentionDays,
                ),
            )
        }
    }
}

/**
 * The deployment's own Microsoft app registration for the Outlook import
 * (`mailbox-migration.md` § Microsoft sign-in, *An admin-supplied
 * registration*). The two drafts go out as typed; the shared machine checks
 * them (both blank clears) and refuses a bad pair onto the error banner before
 * any round trip.
 */
@Composable
private fun MicrosoftGroup(
    initial: MicrosoftRegistrationView,
    working: Boolean,
    onSave: (clientId: String, tenant: String) -> Unit,
) {
    var clientId by remember(initial) { mutableStateOf(initial.clientId) }
    var tenant by remember(initial) { mutableStateOf(initial.tenant) }

    GroupCard(
        stringResource(R.string.admin_mail_page_microsoft_group_title),
        stringResource(R.string.admin_mail_page_microsoft_group_desc),
    ) {
        Text(
            stringResource(R.string.admin_mail_page_microsoft_requirements),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            stringResource(
                if (initial.isDefault) R.string.admin_mail_page_microsoft_using_default
                else R.string.admin_mail_page_microsoft_using_own,
            ),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        PlainField(stringResource(R.string.admin_mail_page_microsoft_client_id_label), null, clientId, { clientId = it }, Ids.ADMIN_MAIL_MICROSOFT_CLIENT_ID_INPUT)
        PlainField(stringResource(R.string.admin_mail_page_microsoft_tenant_label), stringResource(R.string.admin_mail_page_microsoft_tenant_subtitle), tenant, { tenant = it }, Ids.ADMIN_MAIL_MICROSOFT_TENANT_INPUT)
        SaveButton(
            Ids.ADMIN_MAIL_MICROSOFT_SAVE_BUTTON,
            stringResource(R.string.admin_mail_page_microsoft_save),
            faunaGate("fauna.bridges.set_microsoft_registration", enabled = !working),
        ) {
            onSave(clientId, tenant)
        }
    }
}

// ── Shared widget helpers ───────────────────────────────────────────────────

/** Split a multiline field into trimmed, non-empty lines (full-replace list). */
private fun splitLines(text: String): List<String> =
    text.split("\n").map { it.trim() }.filter { it.isNotEmpty() }

@Composable
private fun GroupCard(title: String, desc: String?, content: @Composable ColumnScope.() -> Unit) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            if (desc != null) {
                Text(desc, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            content()
        }
    }
}

@Composable
private fun NumField(label: String, subtitle: String?, value: String, onChange: (String) -> Unit, testTag: String) {
    OutlinedTextField(
        value = value,
        onValueChange = { onChange(it.filter(Char::isDigit)) },
        label = { Text(label) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        supportingText = subtitle?.let { { Text(it) } },
        modifier = Modifier.fillMaxWidth().testTag(testTag),
    )
}

@Composable
private fun PlainField(label: String, subtitle: String?, value: String, onChange: (String) -> Unit, testTag: String) {
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(label) },
        singleLine = true,
        supportingText = subtitle?.let { { Text(it) } },
        modifier = Modifier.fillMaxWidth().testTag(testTag),
    )
}

@Composable
private fun MultilineField(label: String, subtitle: String?, value: String, onChange: (String) -> Unit, testTag: String) {
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(label) },
        minLines = 2,
        supportingText = subtitle?.let { { Text(it) } },
        modifier = Modifier.fillMaxWidth().testTag(testTag),
    )
}

@Composable
private fun ToggleRow(label: String, subtitle: String?, checked: Boolean, enabled: Boolean, onChange: (Boolean) -> Unit, testTag: String, state: String? = null) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.bodyLarge)
            if (subtitle != null) {
                Text(subtitle, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        Switch(
            checked = checked,
            onCheckedChange = onChange,
            enabled = enabled,
            modifier = Modifier.testTag(testTag).let { m ->
                if (state != null) m.semantics { stateDescription = state } else m
            },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun SelectField(label: String, options: List<Pair<String, String>>, selected: String, onSelect: (String) -> Unit, testTag: String) {
    var expanded by remember { mutableStateOf(false) }
    val selectedLabel = options.firstOrNull { it.first == selected }?.second ?: selected
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = !expanded }) {
        OutlinedTextField(
            value = selectedLabel,
            onValueChange = {},
            readOnly = true,
            label = { Text(label) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.menuAnchor().fillMaxWidth().testTag(testTag),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { (raw, lbl) ->
                DropdownMenuItem(text = { Text(lbl) }, onClick = { onSelect(raw); expanded = false })
            }
        }
    }
}

/**
 * The six policy groups' shared commit button.
 *
 * ⚠ It takes a [FaunaGateVerdict] rather than calling [faunaGate] itself, and
 * that is the point: each group full-PUTs a DIFFERENT policy sub-struct, so the
 * six call sites issue six different wire kinds. A `faunaGate(kind)` in here
 * would be handed a variable, which `check-offline-gate-kinds.py` cannot
 * distinguish from the computed kind it must refuse — so the kind literal stays
 * at the call site where the reader (and the checker) can see which gesture it
 * belongs to. Taking the whole verdict rather than a bare `enabled` is what
 * stops a call site rendering the button and forgetting its reason.
 */
@Composable
private fun SaveButton(testTag: String, label: String, gate: FaunaGateVerdict, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        enabled = gate.enabled,
        modifier = Modifier.fillMaxWidth().testTag(testTag),
    ) { Text(label) }
    DisabledControlReasonText(gate.reason)
}
