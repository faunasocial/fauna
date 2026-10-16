package com.fauna.app.ui.screen.moderation

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.key
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.ContentLabelBadge
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.AutomationAttributePack
import com.fauna.app.ui.util.FaunaGateVerdict
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.ModerationQueueVM
import com.fauna.ffi.FfiReportLedgerRow
import com.fauna.ffi.appealFormView
import com.fauna.ffi.obligationActionLabel
import com.fauna.ffi.rowIsAppealable
import com.fauna.ffi.shortId
import uniffi.fauna_client_moderation.QueueRow
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_client_moderation.QueueRowSource
import social.fauna.generated.Ids

/**
 * The standalone **Moderation queue** page (android's top-level `moderation`
 * drawer entry → `moderation-tab`) — the user's view onto why their *own* content
 * was labeled / quarantined / rejected, and their lever to correct it
 * (moderation.md § Goal). Presentation over the merged queue (server
 * `fauna.moderation.actions` ∪ the session's local detections): one `moderation-queue`
 * row per [QueueRow], each carrying a `content-label-badge` (the category, via the
 * shared `contentLabelStyle` map) + the enforcement action (shared
 * `obligationActionLabel`, **server rows only** — a local detection's action column is
 * blank) + a truncated content ref + confidence, with a `train-correction-button`.
 * Empty queue → empty state.
 *
 * Unified shape: a standalone page on standalone clients, the **same IDs** either
 * way (moderation.md § Architectural rules 1) — mirrors linux `views/moderation.rs`,
 * apple `ModerationQueueView`, windows `ModerationPage`. Spam *preferences*
 * (`spam-moderation-controls`) live on the Settings page — the queue consumes them
 * but does not host them. As a top-level drawer page the outer scaffold supplies
 * the title bar + the global `error-message` banner, so this renders content only.
 */
@Composable
fun ModerationQueueScreen(
    navController: NavController? = null,
    vm: ModerationQueueVM = hiltViewModel(),
) {
    val queue by vm.queue.collectAsState()
    val isLoading by vm.isLoading.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    // Training-submission failures surface via the global `error-message` banner
    // (moderation.md § Errors & edge cases) — the queue is never faked green.
    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    val reports by vm.reports.collectAsState()
    val reportsLoaded by vm.reportsLoaded.collectAsState()
    val reportStatus by vm.reportStatus.collectAsState()
    val appealDraft by vm.appealDraft.collectAsState()
    val appealStatus by vm.appealStatus.collectAsState()

    ModerationQueueContent(
        queue = queue,
        isLoading = isLoading,
        onCorrect = { row -> vm.correct(row.contentId, row.source == QueueRowSource.LOCAL) },
        reports = reports,
        reportsLoaded = reportsLoaded,
        reportStatus = reportStatus,
        onWithdrawReport = vm::withdrawReport,
        appealDraft = appealDraft,
        appealStatus = appealStatus,
        onOpenAppeal = { row -> vm.openAppeal(row.contentId) },
        onAppealReasonChange = vm::setAppealReason,
        onSubmitAppeal = vm::submitAppeal,
        onCancelAppeal = vm::cancelAppeal,
    )
}

/**
 * The stateless queue body — the `*Content` split every sibling settings screen
 * has, so the page is renderable under Robolectric with seeded rows (no Hilt, no
 * VM, no FFI). Added when the offline gate reached `train-correction-button`,
 * whose local-vs-server discriminant is only provable by rendering both kinds of
 * row side by side.
 */
@Composable
fun ModerationQueueContent(
    queue: List<QueueRow>,
    isLoading: Boolean,
    onCorrect: (QueueRow) -> Unit,
    // The reporter's ledger (defaults keep the queue-only harnesses unchanged).
    reports: List<FfiReportLedgerRow> = emptyList(),
    reportsLoaded: Boolean = false,
    reportStatus: LocalizedText? = null,
    onWithdrawReport: (String) -> Unit = {},
    // The appeal form (defaults keep the queue-only harnesses unchanged).
    appealDraft: ModerationQueueVM.AppealDraft? = null,
    appealStatus: LocalizedText? = null,
    onOpenAppeal: (QueueRow) -> Unit = {},
    onAppealReasonChange: (String) -> Unit = {},
    onSubmitAppeal: () -> Unit = {},
    onCancelAppeal: () -> Unit = {},
) {
    // One scrolling page: the enforcement queue, then the reporter's own ledger
    // beneath it (moderation.md § User-initiated reporting → *What the reporter is
    // told*). The queue is the user's OWN flagged content, so it is short enough
    // to lay out inline rather than as a virtualized list.
    Column(modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        // Section caption above the queue (mirrors linux + windows'
        // "Enforcement Actions").
        Text(
            stringResource(R.string.moderation_enforcement_title),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(start = 16.dp, top = 12.dp, bottom = 6.dp),
        )

        if (queue.isEmpty()) {
            // An empty queue is the empty state, not an error. The container keeps
            // its `moderation-queue` testTag so the e2e asserts it renders even
            // when empty.
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 96.dp)
                    .testTag(Ids.MODERATION_QUEUE),
                contentAlignment = Alignment.Center,
            ) {
                if (isLoading) {
                    CircularProgressIndicator()
                } else {
                    Text(
                        stringResource(R.string.moderation_no_actions),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        } else {
            Column(modifier = Modifier.fillMaxWidth().testTag(Ids.MODERATION_QUEUE)) {
                queue.forEach { row ->
                    key(row.contentId) {
                        ModerationQueueRow(
                            row = row,
                            onCorrect = { onCorrect(row) },
                            onAppeal = { onOpenAppeal(row) },
                        )
                        // The open form paints under the row it was opened against.
                        if (appealDraft != null && appealDraft.contentId == row.contentId) {
                            AppealForm(
                                draft = appealDraft,
                                onReasonChange = onAppealReasonChange,
                                onSubmit = onSubmitAppeal,
                                onCancel = onCancelAppeal,
                            )
                        }
                        HorizontalDivider()
                    }
                }
                localized(appealStatus)?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier
                            .padding(horizontal = 16.dp, vertical = 8.dp)
                            .testTag(Ids.APPEAL_STATUS),
                    )
                }
            }
        }

        ReportLedgerSection(
            reports = reports,
            loaded = reportsLoaded,
            status = reportStatus,
            onWithdraw = onWithdrawReport,
        )
    }
}

/**
 * The reporter's own ledger (`moderation-reports-section`, moderation.md §
 * User-initiated reporting → *What the reporter is told*): one flat
 * `moderation-report-item[i]` per report filed, each carrying
 * `reason · status · short subject id · outcome — where it went` (the line web's
 * `ledgerLine`, tui's `ledger_elements` and apple's `ledgerLine` paint — the e2e
 * reads this text on every app), with a `moderation-report-withdraw-button[i]` on
 * an OPEN row. The empty line paints only off the `loaded` bit, so a slow first
 * read never claims "You have not reported anything" (`ui/README.md` § List
 * pages: loading is not empty).
 */
@Composable
private fun ReportLedgerSection(
    reports: List<FfiReportLedgerRow>,
    loaded: Boolean,
    status: LocalizedText?,
    onWithdraw: (String) -> Unit,
) {
    val context = LocalContext.current
    Column(modifier = Modifier.fillMaxWidth().testTag(Ids.MODERATION_REPORTS_SECTION)) {
        Text(
            stringResource(R.string.moderation_report_ledger_title),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 6.dp),
        )
        localized(status)?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
        if (loaded && reports.isEmpty()) {
            Text(
                stringResource(R.string.moderation_report_ledger_empty),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
        reports.forEach { row ->
            key(row.reportId) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        ModerationQueueVM.ledgerLine(row) { resolveLocalized(context, it).orEmpty() },
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.weight(1f).testTag(Ids.MODERATION_REPORT_ITEM),
                    )
                    if (row.canWithdraw) {
                        TextButton(
                            onClick = { onWithdraw(row.reportId) },
                            modifier = Modifier.testTag(Ids.MODERATION_REPORT_WITHDRAW_BUTTON),
                        ) { Text(stringResource(R.string.moderation_report_withdraw)) }
                    }
                }
                HorizontalDivider()
            }
        }
    }
}

/**
 * The open appeal form (moderation.md § Legal takedown — the transparency
 * triple's second leg). Every decision is the shared
 * `fauna_client_moderation::appeal` fold ([appealFormView]): the submit control
 * renders disabled with the fold's stated reason until the reason is typed, and
 * the summary names the content the appeal is filed against. No offline gate:
 * `fauna.moderation.appeal` is `OfflineQueued`, which the gate never desensitizes.
 */
@Composable
private fun AppealForm(
    draft: ModerationQueueVM.AppealDraft,
    onReasonChange: (String) -> Unit,
    onSubmit: () -> Unit,
    onCancel: () -> Unit,
) {
    val view = appealFormView(draft.contentId, draft.reason)
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(6.dp),
    ) {
        Text(
            localized(view.summary).orEmpty(),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        OutlinedTextField(
            value = draft.reason,
            onValueChange = onReasonChange,
            label = { Text(stringResource(R.string.moderation_appeal_reason_label)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.APPEAL_REASON_INPUT),
        )
        DisabledControlReasonText(localized(view.blockedReason))
        Row(horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(12.dp)) {
            OutlinedButton(
                onClick = onCancel,
                modifier = Modifier.testTag(Ids.APPEAL_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.moderation_appeal_cancel)) }
            OutlinedButton(
                onClick = onSubmit,
                enabled = view.canSubmit,
                modifier = Modifier.testTag(Ids.APPEAL_SUBMIT_BUTTON),
            ) { Text(localized(view.submitLabel).orEmpty()) }
        }
    }
}

/**
 * One `moderation-queue` row: category badge + enforcement action (server rows only) +
 * content ref + confidence, with a single `train-correction-button` (mirrors linux
 * `build_queue_row`). Branches only on `row.action` being present — a local detection
 * carries none, so its action column stays blank (never fabricated — moderation.md
 * § Don't do these).
 */
@Composable
private fun ModerationQueueRow(row: QueueRow, onCorrect: () -> Unit, onAppeal: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            // The enforcement action taken, via the shared discriminant→label map
            // (co-located with the enum in `fauna_core::obligation`). Server rows
            // only — a local detection carries none, so its action column stays blank.
            val actionText = row.action?.let { localized(obligationActionLabel(it)).orEmpty() }
            val confidence = "${ModerationQueueVM.confidencePercent(row.confidencePerMille)}%"
            Row(verticalAlignment = Alignment.CenterVertically) {
                // `content-label-badge` — the category, presented entirely through
                // the shared `content_label_style` map (icon + accent colour + i18n
                // label) so no styling drifts per client (moderation.md § Where
                // logic lives, drift #157). It also publishes the confidence and
                // action this row paints as `confidence` / `action` attributes
                // (moderation.md § Element IDs → *Queue row facts*), packed into
                // the node's one string carrier. A local detection packs an
                // empty `action`, which the bridge reads as not set — packing
                // even then keeps a bare confidence from answering every name.
                ContentLabelBadge(
                    label = row.category,
                    modifier = Modifier.semantics {
                        stateDescription = AutomationAttributePack.pack(
                            "confidence" to confidence,
                            "action" to actionText.orEmpty(),
                        )
                    },
                )
                actionText?.let { text ->
                    Spacer(Modifier.width(8.dp))
                    Text(
                        text,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            // Content ref (type + truncated id) — an opaque correlation handle.
            Text(
                "${row.contentType} · ${shortId(row.contentId)}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                "$confidence " + stringResource(R.string.moderation_confidence),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        // A DISCRIMINANT site, and the two branches differ in whether they
        // touch the nest at all — so the gate is handed the same `isLocal` the
        // action takes (`ModerationQueueVM.correct`), never a re-derivation:
        //
        //   local row  → remove the client-side false-positive flag and feed the
        //                ham correction to the sealed model in-process. The
        //                content is MLS-sealed at rest and the nest cannot read
        //                it, so there is NO wire kind and nothing to gate — this
        //                correction must keep working with no nest.
        //   server row → `fauna.moderation.train` (or, on the 1d sealed-write
        //                switch, `fauna.bridges.put_spam_model`). Both are
        //                OnlineOnly, so declaring the fallback is exact for the
        //                gate's purposes: there is no reachable branch that
        //                stays available.
        //
        // The payoff is the pairing: in one queue, a local row's Correct stays
        // live beside a server row's dead one — a blanket disable cannot fake it.
        val gate = if (row.source == QueueRowSource.LOCAL) {
            FaunaGateVerdict(enabled = true, reason = null)
        } else {
            faunaGate("fauna.moderation.train")
        }
        TextButton(
            onClick = onCorrect,
            enabled = gate.enabled,
            modifier = Modifier.testTag(Ids.TRAIN_CORRECTION_BUTTON),
        ) {
            Text(stringResource(R.string.moderation_correct))
        }
        DisabledControlReasonText(gate.reason)
        // The appeal handle: nest-issued (server) rows only, per the shared
        // `row_is_appealable` rule — a local detection records nobody's decision,
        // so its lever is the correction button above.
        if (rowIsAppealable(row)) {
            TextButton(
                onClick = onAppeal,
                modifier = Modifier.testTag(Ids.APPEAL_BUTTON),
            ) { Text(stringResource(R.string.moderation_appeal)) }
        }
    }
}
