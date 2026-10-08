package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.core.ReportSheetStore
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.ReportVM
import com.fauna.ffi.FfiReportSheetView
import com.fauna.ffi.reportSheetView
import social.fauna.generated.Ids

/**
 * The shared report sheet and its acknowledgement line (`report-sheet`,
 * `report-status` — `moderation.md` § User-initiated reporting → *App surface*),
 * mounted ONCE in the shell over whatever page is showing: the three verbs (the
 * feed post ⋯, the conversation message ⋯, an OTHER profile) only call
 * [ReportVM.open], and this paints what is open. The android twin of apple's
 * `ReportHost` and web's `ReportHost.svelte` (priority #2/#3).
 *
 * **Inline, not a `Dialog`** — the ⋯ menus' precedent on apple: a `Dialog` is a
 * separate window that does not inherit the shell's `testTagsAsResourceId`, so
 * its ids would be invisible to the automation bridge. Inline, every id attaches
 * to an element in the one tree. The acknowledgement stays OUTSIDE the closed
 * sheet, so a report filed from a card the reporter-side hide then replaces
 * still paints `report-status`.
 *
 * Every decision is shared Rust's — the reason list, the submit gate, the
 * include-text rule and the words come from `reportSheetView` (folded here per
 * keystroke); this paints the fold and forwards the gestures. A failed send
 * keeps the sheet and lands on the page's `error-message`; a failed block/hide
 * lands there BESIDE the acknowledgement.
 */
@Composable
fun ReportHost(vm: ReportVM = hiltViewModel()) {
    val state by vm.state.collectAsState()
    val context = LocalContext.current
    val appMessages = LocalAppMessages.current

    // The shared per-keystroke fold, `null` while the sheet is closed.
    val view = remember(state.target, state.form) {
        state.target?.let { reportSheetView(it, state.form) }
    }

    // `error-message` is the shell's one banner (cross-app contract): a failed
    // send, or a follow-up that failed after a landed one, goes there.
    LaunchedEffect(state.error, state.followupError) {
        val text = resolveLocalized(context, state.error) ?: state.followupError
        if (text != null) appMessages.showError(text)
    }

    ReportHostContent(
        state = state,
        view = view,
        onEdit = vm::edit,
        onSubmit = vm::submit,
        onCancel = vm::cancel,
    )
}

/**
 * The stateless body — the `*Content` split every sibling screen has, so the
 * sheet is renderable under Robolectric with a seeded [ReportSheetStore.State]
 * and a hand-built [FfiReportSheetView] (no Hilt, no VM, no FFI).
 */
@Composable
fun ReportHostContent(
    state: ReportSheetStore.State,
    view: FfiReportSheetView?,
    onEdit: ((com.fauna.ffi.FfiReportForm) -> com.fauna.ffi.FfiReportForm) -> Unit,
    onSubmit: () -> Unit,
    onCancel: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (view != null) {
            ReportSheet(state = state, view = view, onEdit = onEdit, onSubmit = onSubmit, onCancel = onCancel)
        }
        val status = localized(state.status)
        if (status != null) {
            Text(
                status,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.REPORT_STATUS),
            )
        }
    }
}

@Composable
private fun ReportSheet(
    state: ReportSheetStore.State,
    view: FfiReportSheetView,
    onEdit: ((com.fauna.ffi.FfiReportForm) -> com.fauna.ffi.FfiReportForm) -> Unit,
    onSubmit: () -> Unit,
    onCancel: () -> Unit,
) {
    val form = state.form
    val canSend = view.canSubmit && !state.sending
    val reasonLabel = localized(view.reasonLabel).orEmpty()
    // The picker round-trips the reason TOKEN (`TokenSelect`'s contract); the
    // blank first option is "no reason chosen" — the sheet cannot send until one is.
    val options = listOf("" to reasonLabel) + view.reasons.map { it.reason to (localized(it.label) ?: it.reason) }
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.REPORT_SHEET)) {
        Column(
            modifier = Modifier
                .heightIn(max = 420.dp)
                .verticalScroll(rememberScrollState())
                .padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(localized(view.title).orEmpty(), style = MaterialTheme.typography.titleMedium)

            TokenSelect(
                testTagValue = Ids.REPORT_REASON_SELECT,
                selected = form.reason.orEmpty(),
                options = options,
                onSelect = { token -> onEdit { it.copy(reason = token.ifEmpty { null }) } },
            )

            OutlinedTextField(
                value = form.note,
                onValueChange = { note -> onEdit { it.copy(note = note) } },
                label = { Text(localized(view.noteLabel).orEmpty()) },
                minLines = 2,
                maxLines = 4,
                modifier = Modifier.fillMaxWidth().testTag(Ids.REPORT_NOTE_INPUT),
            )

            // Rendered only for a sealed subject (a message; a gated post) — the
            // shared fold says so, the app never re-derives it.
            if (view.showIncludeText) {
                LabeledCheckbox(
                    checked = form.includeText,
                    label = localized(view.includeTextLabel).orEmpty(),
                    tag = Ids.REPORT_INCLUDE_TEXT_CHECKBOX,
                    onCheckedChange = { on -> onEdit { it.copy(includeText = on) } },
                )
            }

            LabeledCheckbox(
                checked = form.blockAuthor,
                label = localized(view.blockAuthorLabel).orEmpty(),
                tag = Ids.REPORT_BLOCK_AUTHOR_CHECKBOX,
                onCheckedChange = { on -> onEdit { it.copy(blockAuthor = on) } },
            )

            if (!view.canSubmit) {
                localized(view.blockedReason)?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = onSubmit,
                    enabled = canSend,
                    modifier = Modifier.testTag(Ids.REPORT_SUBMIT_BUTTON),
                ) { Text(localized(view.submitLabel).orEmpty()) }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.REPORT_CANCEL_BUTTON),
                ) { Text(localized(view.cancelLabel).orEmpty()) }
            }
        }
    }
}

@Composable
private fun LabeledCheckbox(
    checked: Boolean,
    label: String,
    tag: String,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Checkbox(
            checked = checked,
            onCheckedChange = onCheckedChange,
            modifier = Modifier.testTag(tag),
        )
        Text(label, style = MaterialTheme.typography.bodyMedium)
    }
}
