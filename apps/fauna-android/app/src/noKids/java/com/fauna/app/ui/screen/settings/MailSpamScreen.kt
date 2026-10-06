package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.MailSpamVM
import com.fauna.ffi.FfiReportShareEntry
import uniffi.fauna_client_mail_settings.SpamStatus
import uniffi.fauna_client_mail_settings.SpamTrainingView
import uniffi.fauna_client_mail_settings.TrainingLabel
import uniffi.fauna_client_mail_settings.TrainingSource
import uniffi.fauna_client_mail_settings.trainingLabelBadge
import uniffi.fauna_client_mail_settings.trainingSourceBadge
import java.text.DateFormat
import java.util.Date
import social.fauna.generated.Ids

/**
 * The per-account `mail-spam` page (`mail-spam.md`): reset the per-user Bayesian
 * classifier, opt in/out of the deployment baseline, opt in/out of distributed
 * report sharing (with a read-only transparency list of what this nest
 * publishes — `report-sharing.md` § Client wire), and an indexed training-
 * history list with per-row Undo. A sub-page of the mail-settings hub, reached
 * from its "Spam" nav row.
 *
 * Stateless [MailSpamContent] is split out so it renders under the Compose test
 * harness with seeded state; the VM-bound [MailSpamScreen] is the thin wrapper
 * the NavHost mounts. The per-user feedback loop is not yet built nest-side, so
 * every action surfaces the seam's `unimplemented` rejection via the global
 * error-message banner — the list is never faked green. Report sharing is a
 * separate, already-implemented seam (`FfiModerationClient::report_share_*`),
 * not part of that pending feedback loop. Mirrors the Linux lead
 * (apps/fauna-linux/src/settings/mail_spam.rs); `page-heading` is this screen's
 * local TopAppBar title, `error-message` the global MessageBanner.
 */
@Composable
fun MailSpamScreen(
    navController: NavController,
    vm: MailSpamVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    val reportShare by vm.reportShare.collectAsState()
    val reportSharePublished by vm.reportSharePublished.collectAsState()
    val thresholdOverride by vm.thresholdOverride.collectAsState()

    MailSpamContent(
        events = snapshot.events,
        contributeBaseline = snapshot.contributeBaseline,
        reportShare = reportShare,
        reportSharePublished = reportSharePublished,
        thresholdOverride = thresholdOverride,
        working = snapshot.status == SpamStatus.WORKING,
        // Training-history badges via shared `training_label_badge` /
        // `training_source_badge` (mail-spam.md § Shared formatters); FFI lives
        // here, off the testable Content.
        labelBadge = { label -> resolveLocalized(context, trainingLabelBadge(label)).orEmpty() },
        sourceBadge = { source -> resolveLocalized(context, trainingSourceBadge(source)).orEmpty() },
        onBack = { navController.popBackStack() },
        onResetModel = vm::resetModel,
        onSetContributeBaseline = vm::setContributeBaseline,
        onSetReportShare = vm::setReportShare,
        onSetThresholdOverride = vm::setThresholdOverride,
        onUndo = vm::undo,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MailSpamContent(
    events: List<SpamTrainingView>,
    contributeBaseline: Boolean,
    reportShare: Boolean,
    reportSharePublished: List<FfiReportShareEntry>,
    thresholdOverride: UInt?,
    working: Boolean,
    labelBadge: (TrainingLabel) -> String,
    sourceBadge: (TrainingSource) -> String,
    onBack: () -> Unit,
    onResetModel: () -> Unit,
    onSetContributeBaseline: (Boolean) -> Unit,
    onSetReportShare: (Boolean) -> Unit,
    onSetThresholdOverride: (UInt?) -> Unit,
    onUndo: (String) -> Unit,
    parseCount: (String) -> UInt? = { com.fauna.ffi.parseCount(it) },
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.mail_spam_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
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
                stringResource(R.string.mail_spam_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // ── Reset model (destructive; two-click inline confirm) ──
            ResetModelRow(working = working, onResetModel = onResetModel)

            // ── Contribute-to-baseline toggle (default off) ──
            // A dispatch-on-change toggle IS the commit — there is no separate
            // save for it to buffer into, so it declares (the `TierDropdown`
            // users-row split, reached the same way).
            val contributeGate = faunaGate(
                "fauna.bridges.set_baseline_contribution",
                enabled = !working,
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.mail_spam_contribute_baseline_label),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    Text(
                        stringResource(R.string.mail_spam_contribute_baseline_subtitle),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    DisabledControlReasonText(contributeGate.reason)
                }
                Switch(
                    checked = contributeBaseline,
                    onCheckedChange = onSetContributeBaseline,
                    enabled = contributeGate.enabled,
                    modifier = Modifier.testTag(Ids.MAIL_SPAM_CONTRIBUTE_BASELINE_TOGGLE),
                )
            }

            // ── Share-reports toggle (distributed, k-anonymous report sharing;
            //    default off) ──
            // ⚠ THE GATE QUESTION WAS ASKED HERE AND THE ANSWER IS "NO GATE".
            // This toggle is NOT a `MailSpamMachine`/bridge call but a plain
            // write on the shared moderation manager
            // (`ModerationClient::report_share_set`) — `fauna.moderation
            // .report_share.set`, **OfflineSafe** — per tui's
            // `MailSpamToggleShareReports` ruling. So it stays live with no
            // nest, and it is this page's live sibling beside four dead
            // controls: a blanket grey cannot pass the test below.
            //
            // ⚠ And it must carry NO `faunaGate` call, even though linux
            // declares the kind on its own toggle (`settings/mail_spam.rs:189`).
            // The two surfaces differ for a real reason: linux's declaration
            // feeds a registry that does nothing at all with a non-OnlineOnly
            // kind, whereas `faunaGate` here RETURNS an `enabled` a reader will
            // believe is doing work. `check-offline-gate-kinds.py` rejects such
            // a call outright — "no declared kind is OnlineOnly … so this gate
            // can never desensitize anything" — and it is right to: the call
            // would read as the control having been gated when nothing gates it.
            // Only a DISCRIMINANT may name an OfflineSafe kind here, and only
            // because its other arm is OnlineOnly.
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.mail_spam_share_reports_label),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    Text(
                        stringResource(R.string.mail_spam_share_reports_subtitle),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(
                    checked = reportShare,
                    onCheckedChange = onSetReportShare,
                    modifier = Modifier.testTag(Ids.MAIL_SPAM_SHARE_REPORTS_TOGGLE),
                )
            }

            // ── Per-account spam-folder threshold override
            //    (mail-policy-config.md § Tier 3) ──
            ThresholdOverrideField(thresholdOverride, onSetThresholdOverride, parseCount)

            // ── "What this nest publishes" transparency list
            //    (report-share-published-list; read-only, no action) ──
            Text(
                stringResource(R.string.mail_spam_published_title),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.mail_spam_published_description),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (reportSharePublished.isEmpty()) {
                Text(
                    stringResource(R.string.mail_spam_published_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                reportSharePublished.forEach { entry -> ReportSharePublishedRow(entry) }
            }

            // ── Training-history list (indexed; one row per training event) ──
            Text(
                stringResource(R.string.mail_spam_history_title),
                style = MaterialTheme.typography.titleMedium,
            )

            if (events.isEmpty()) {
                Text(
                    stringResource(R.string.mail_spam_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                events.forEach { event ->
                    SpamTrainingRow(
                        event = event,
                        working = working,
                        labelBadge = labelBadge,
                        sourceBadge = sourceBadge,
                        onUndo = onUndo,
                    )
                }
            }
        }
    }
}

/**
 * `mail-spam-threshold-override-input` — empty follows the admin default,
 * "0" is a real setting (turns automatic Junk filing off for this account),
 * never collapsed into empty. Commits on the keyboard's Done action — no
 * separate save button, mirroring tui's `Element::input_commit` shape (the
 * ratified reference: linux `settings/mail_spam.rs`'s Enter-commit entry).
 * Local edit state re-seeds from [thresholdOverride] on every prop change, so
 * the field always reflects the persisted value after a commit — never an
 * assumed outcome.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ThresholdOverrideField(
    thresholdOverride: UInt?,
    onSetThresholdOverride: (UInt?) -> Unit,
    parseCount: (String) -> UInt?,
) {
    var input by remember(thresholdOverride) {
        mutableStateOf(thresholdOverride?.toString() ?: "")
    }
    // ⚠ THE ONE PLACE ON THIS PAGE WHERE "the commit gates, not the buffer"
    // gates the buffer — because here they are the same widget. This field has
    // no Save button: the IME `Done` action IS the dispatch, exactly as tui
    // carries a dedicated `MailSpamCommitThreshold` gesture and linux commits on
    // `connect_activate`. So the control that issues the kind *is* the field,
    // and linux declares it on the `Entry` itself
    // (`settings/mail_spam.rs`, `threshold_input`). Matching that. A disabled
    // Material text field still renders its current value, so the persisted
    // override stays readable with no nest — only editing-to-commit closes.
    val thresholdGate = faunaGate("fauna.bridges.set_spam_threshold_override")
    Column {
        Text(
            stringResource(R.string.mail_spam_threshold_override_label),
            style = MaterialTheme.typography.bodyLarge,
        )
        Text(
            stringResource(R.string.mail_spam_threshold_override_subtitle),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        OutlinedTextField(
            value = input,
            onValueChange = { input = it },
            singleLine = true,
            enabled = thresholdGate.enabled,
            keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                keyboardType = androidx.compose.ui.text.input.KeyboardType.Number,
                imeAction = androidx.compose.ui.text.input.ImeAction.Done,
            ),
            keyboardActions = androidx.compose.foundation.text.KeyboardActions(
                onDone = { onSetThresholdOverride(parseCount(input)) },
            ),
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT),
        )
        DisabledControlReasonText(thresholdGate.reason)
    }
}

@Composable
private fun ResetModelRow(working: Boolean, onResetModel: () -> Unit) {
    // Two-click inline confirm (no separate ui.yaml confirm element prescribed),
    // mirroring the Linux mail_spam.rs reset affordance.
    var armed by remember { mutableStateOf(false) }
    // ⚠ THE WHOLE BUTTON GATES — arm click included — and that is NOT the
    // "arming is local, the confirm declares" rule being broken. That rule
    // exists because an *opener* reveals something worth reading offline (a
    // dialog's text, a roster resolved by a `Read`). Here there is no opener and
    // nothing revealed: one button relabels itself, so its arm click reveals
    // exactly nothing and the only thing it can lead to is the dispatch. Both
    // the lead app and linux gate the single control outright — tui maps the one
    // `MailSpamReset` action (arm *and* confirm) to the kind
    // (`settings/mod.rs:3214`), and linux declares on `reset_button` itself
    // (`settings/mail_spam.rs:147`), with the two-click wiring layered on top.
    // Arming a control that cannot fire would be pure theatre.
    val resetGate = faunaGate("fauna.bridges.reset_spam_model", enabled = !working)
    Column {
        OutlinedButton(
            onClick = {
                if (armed) {
                    armed = false
                    onResetModel()
                } else {
                    armed = true
                }
            },
            enabled = resetGate.enabled,
            colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
            modifier = Modifier.testTag(Ids.MAIL_SPAM_RESET_MODEL_BUTTON),
        ) {
            Text(
                if (armed) stringResource(R.string.common_confirm)
                else stringResource(R.string.mail_spam_reset_button),
            )
        }
        Text(
            stringResource(R.string.mail_spam_reset_subtitle),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        DisabledControlReasonText(resetGate.reason)
    }
}

/** One `report-share-published-list-item` row — a published ≥k report
 *  aggregate. Pure transparency, read-only, no action. */
@Composable
private fun ReportSharePublishedRow(entry: FfiReportShareEntry) {
    val reporters = stringResource(R.string.mail_spam_published_reporters)
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.REPORT_SHARE_PUBLISHED_LIST_ITEM)) {
        Row(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                // The count element carries the BARE number — the word rides an
                // adjacent Text, so the row still reads "3 reporters" while
                // `report-share-published-list-item-count` stays the machine-
                // readable value every other app renders (linux puts the
                // count in a value_marker, the phrase in the row title;
                // test_mail_spam.py asserts the element's text == "3"). This
                // client folded the word into the element, which no android e2e
                // has ever run to catch.
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text(
                        entry.count.toString(),
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.testTag(Ids.REPORT_SHARE_PUBLISHED_LIST_ITEM_COUNT),
                    )
                    Text(reporters, style = MaterialTheme.typography.bodyMedium)
                }
                Text(
                    entry.factor,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.REPORT_SHARE_PUBLISHED_LIST_ITEM_FACTOR),
                )
                Text(
                    entry.contentHash,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.REPORT_SHARE_PUBLISHED_LIST_ITEM_HASH),
                )
            }
        }
    }
}

/** One `mail-spam-training-history-list-item` row, projected from a [SpamTrainingView]. */
@Composable
private fun SpamTrainingRow(
    event: SpamTrainingView,
    working: Boolean,
    labelBadge: (TrainingLabel) -> String,
    sourceBadge: (TrainingSource) -> String,
    onUndo: (String) -> Unit,
) {
    val labelText = labelBadge(event.label)
    val sourceText = sourceBadge(event.source)
    val createdAt = remember(event.createdAtMs) {
        DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT)
            .format(Date(event.createdAtMs))
    }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM)) {
        Row(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    event.message,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.testTag(Ids.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_MESSAGE),
                )
                Text(
                    labelText,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_LABEL),
                )
                Text(
                    sourceText,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_SOURCE),
                )
                Text(
                    createdAt,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_CREATED_AT),
                )
            }
            // Per-row undo — one kind for every row, so the declaration sits
            // here inside the row composable rather than at each call site (the
            // `TierDropdown` caveat does not apply: this composable has exactly
            // one call site and every row issues the same gesture).
            val undoGate = faunaGate("fauna.bridges.put_spam_model", enabled = !working)
            Column(horizontalAlignment = Alignment.End) {
                OutlinedButton(
                    onClick = { onUndo(event.historyIdHex) },
                    enabled = undoGate.enabled,
                    modifier = Modifier.testTag(Ids.MAIL_SPAM_TRAINING_HISTORY_LIST_ITEM_UNDO_BUTTON),
                ) {
                    Text(stringResource(R.string.mail_spam_undo))
                }
                DisabledControlReasonText(undoGate.reason)
            }
        }
    }
}
