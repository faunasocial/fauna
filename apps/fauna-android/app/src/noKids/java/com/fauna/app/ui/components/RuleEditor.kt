package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.ffi.FfiRuleEditor
import com.fauna.ffi.ruleKeywordsInline
import kotlinx.coroutines.launch
import social.fauna.generated.Ids
import uniffi.fauna_client_policy.RuleEditorView
import uniffi.fauna_client_policy.RuleKind

/**
 * The shared `rule-editor` component's paint — ONE composable for every
 * mounting (`nest-rules.md` § The shared rule editor → *Three mountings, one
 * shape*): the admin Nest page hosts it in `admin-nest-rules-section`, the
 * Family page in `family-policy-rules-section` for the selected ward. The twin
 * of tui `rule_editor.rs` and web `RuleEditor.svelte`.
 *
 * **This composable paints; it decides nothing.** The rule list, the kinds, the
 * categories, the verdicts, the level scale, every label and defect sentence and
 * whether the document may be saved are [FfiRuleEditor]'s (shared Rust,
 * `fauna_client_policy`), so the editor cannot differ between hosts or across the
 * 7 apps. A host owns the editor handle, the save and the status line; this
 * forwards the edits and calls [onSave].
 *
 * [writeKind] is the write the host's save sends — the offline gate greys Save
 * from it (`fauna.admin.content_policy.set` | `fauna.family.policy.update`).
 * [status] is `rule-editor-status`'s text — the host's last verdict; empty shows
 * the editor's own defects. [onChange] fires on any edit, so the host clears a
 * stale "saved". [loadLabelers] / [loadModule] are the labeler picker's reads.
 */
@Composable
fun RuleEditor(
    editor: FfiRuleEditor,
    writeKind: String,
    status: String,
    saving: Boolean,
    onSave: () -> Unit,
    onChange: () -> Unit,
    loadLabelers: suspend () -> List<String>,
    loadModule: suspend (String) -> ByteArray?,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var view by remember(editor) { mutableStateOf(editor.view()) }
    // The keywords inputs as typed: a single-line input re-painted from the
    // stored terms would eat a trailing comma mid-typing, so the typed text is
    // kept per row while the editor holds the parsed terms (web's twin).
    val typedKeywords = remember(editor) { mutableStateMapOf<Int, String>() }
    // The publisher's subscribed labelers a rule may name, read beside the document.
    var subscribed by remember(editor) { mutableStateOf(emptyList<String>()) }
    LaunchedEffect(editor) {
        subscribed = try {
            loadLabelers()
        } catch (e: Exception) {
            android.util.Log.w("RuleEditor", "subscribed labelers unavailable: ${e.message}")
            emptyList()
        }
    }
    val labelerChoices = remember(view, subscribed) { editor.labelerChoices(subscribed) }
    fun label(text: uniffi.fauna_core.LocalizedText?): String = resolveLocalized(context, text).orEmpty()
    fun refresh() {
        view = editor.view()
        onChange()
    }
    val editable = view.editable

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (!editable) {
            Text(
                stringResource(R.string.rule_editor_newer_version),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (view.rows.isEmpty()) {
            Text(stringResource(R.string.rule_editor_no_rules), color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        view.rows.forEachIndexed { i, row ->
            val index = i.toUInt()
            Column(
                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp).testTag(Ids.RULE_EDITOR_ITEM),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    stringResourceFmt(R.string.rule_editor_item_label, row.number.toString()),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (row.readOnly) {
                    Text(label(row.kindLabel))
                } else {
                    // Every picker's option IS the label the crate painted (the
                    // cross-app select contract), so a pick maps back through the
                    // view's own lists by position.
                    LabelPicker(
                        testId = Ids.RULE_EDITOR_ITEM_KIND_SELECT,
                        title = stringResource(R.string.rule_editor_kind_label),
                        options = view.kinds.map { label(it.label) },
                        selected = label(row.kindLabel),
                        enabled = editable,
                    ) { k ->
                        if (editor.setKind(index, view.kinds[k].kind)) {
                            typedKeywords.clear()
                            refresh()
                        }
                    }
                    RuleSubject(editor, row, index, view, editable, typedKeywords, labelerChoices.map { it.name }, ::label, ::refresh) { name ->
                        val choice = labelerChoices.firstOrNull { it.name == name } ?: return@RuleSubject
                        scope.launch {
                            val id = choice.labelerId
                            val ok = if (id != null) {
                                // A subscribed labeler the document does not bundle
                                // yet: fetch its module, hand it over under the name.
                                val module = try {
                                    loadModule(id)
                                } catch (e: Exception) {
                                    android.util.Log.w("RuleEditor", "labeler module unavailable: ${e.message}")
                                    null
                                }
                                module != null && editor.setLabeler(index, choice.name, module)
                            } else {
                                editor.chooseBundledLabeler(index, choice.name)
                            }
                            if (ok) refresh()
                        }
                    }
                    LabelPicker(
                        testId = Ids.RULE_EDITOR_ITEM_VERDICT_SELECT,
                        title = stringResource(R.string.rule_editor_verdict_label),
                        options = view.verdicts.map { label(it.label) },
                        selected = label(view.verdicts.firstOrNull { it.verdict == row.verdict }?.label),
                        enabled = editable,
                    ) { v -> if (editor.setVerdict(index, view.verdicts[v].verdict)) refresh() }
                    row.levelIndex?.let { level ->
                        LabelPicker(
                            testId = Ids.RULE_EDITOR_ITEM_LEVEL_SELECT,
                            title = stringResource(R.string.rule_editor_level_label),
                            options = view.levels.map { label(it.label) },
                            selected = label(view.levels.getOrNull(level.toInt())?.label),
                            enabled = editable,
                        ) { l -> if (editor.setLevel(index, l.toUInt())) refresh() }
                    }
                    OutlinedTextField(
                        value = row.reasons.firstOrNull { it.lang == view.reasonDefaultKey }?.text.orEmpty(),
                        onValueChange = { text ->
                            editor.setReason(index, view.reasonDefaultKey, text)
                            refresh()
                        },
                        enabled = editable,
                        singleLine = true,
                        label = { Text(stringResource(R.string.rule_editor_reason_label)) },
                        modifier = Modifier.fillMaxWidth().testTag(Ids.RULE_EDITOR_ITEM_REASON_INPUT),
                    )
                }
                row.inert?.let { inert ->
                    Text(
                        label(inert),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                OutlinedButton(
                    onClick = {
                        if (editor.removeRule(index)) {
                            // Rows after this one shift up; their typed text is
                            // re-read from the stored terms.
                            typedKeywords.clear()
                            refresh()
                        }
                    },
                    modifier = Modifier.testTag(Ids.RULE_EDITOR_ITEM_REMOVE_BUTTON),
                ) { Text(stringResource(R.string.rule_editor_remove_button)) }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedButton(
                onClick = {
                    editor.addRule(RuleKind.CATEGORY)
                    refresh()
                },
                enabled = editable,
                modifier = Modifier.testTag(Ids.RULE_EDITOR_ADD_BUTTON),
            ) { Text(stringResource(R.string.rule_editor_add_button)) }
            // The gate's kind is spelled out per host: `check-offline-gate-kinds.py`
            // grades each literal against the shared table.
            val gate = if (writeKind == "fauna.admin.content_policy.set") {
                faunaGate("fauna.admin.content_policy.set", enabled = !saving && view.canSave)
            } else {
                faunaGate("fauna.family.policy.update", enabled = !saving && view.canSave)
            }
            Button(
                onClick = onSave,
                enabled = gate.enabled,
                modifier = Modifier.testTag(Ids.RULE_EDITOR_SAVE_BUTTON),
            ) {
                Text(stringResource(if (saving) R.string.rule_editor_saving else R.string.rule_editor_save_button))
            }
        }
        DisabledControlReasonText(
            if (writeKind == "fauna.admin.content_policy.set") {
                faunaGate("fauna.admin.content_policy.set").reason
            } else {
                faunaGate("fauna.family.policy.update").reason
            },
        )
        Text(
            status.ifEmpty { view.defects.joinToString(" ") { label(it) } },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.RULE_EDITOR_STATUS),
        )
    }
}

/** The row's one subject control, by kind: a category picker, the keywords or
 *  prompt input, or the labeler picker. */
@Composable
private fun RuleSubject(
    editor: FfiRuleEditor,
    row: uniffi.fauna_client_policy.RuleRow,
    index: UInt,
    view: RuleEditorView,
    editable: Boolean,
    typedKeywords: MutableMap<Int, String>,
    labelerNames: List<String>,
    label: (uniffi.fauna_core.LocalizedText?) -> String,
    refresh: () -> Unit,
    onPickLabeler: (String) -> Unit,
) {

    val category = row.category
    val keywords = row.keywords
    val labeler = row.labeler
    val prompt = row.prompt
    when {
        category != null -> LabelPicker(
            testId = Ids.RULE_EDITOR_ITEM_CATEGORY_SELECT,
            title = stringResource(R.string.rule_editor_category_label),
            options = view.categories.map { label(it.label) },
            selected = label(view.categories.firstOrNull { it.category == category }?.label),
            enabled = editable,
        ) { c -> if (editor.setCategory(index, view.categories[c].category)) refresh() }
        keywords != null -> OutlinedTextField(
            value = typedKeywords[index.toInt()] ?: ruleKeywordsInline(keywords),
            onValueChange = { text ->
                typedKeywords[index.toInt()] = text
                editor.setKeywordsInline(index, text)
                refresh()
            },
            enabled = editable,
            singleLine = true,
            label = { Text(stringResource(R.string.rule_editor_keywords_label)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RULE_EDITOR_ITEM_KEYWORDS_INPUT),
        )
        labeler != null -> LabelPicker(
            testId = Ids.RULE_EDITOR_ITEM_LABELER_SELECT,
            title = stringResource(R.string.rule_editor_labeler_label),
            options = labelerNames,
            selected = labeler,
            enabled = editable,
        ) { l -> onPickLabeler(labelerNames[l]) }
        prompt != null -> OutlinedTextField(
            value = prompt,
            onValueChange = { text ->
                editor.setPrompt(index, text)
                refresh()
            },
            enabled = editable,
            singleLine = true,
            label = { Text(stringResource(R.string.rule_editor_prompt_label)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RULE_EDITOR_ITEM_PROMPT_INPUT),
        )
    }
}

/**
 * A cross-app label picker (ui.yaml types `rule-editor-item-*-select` as
 * `select`): the anchor shows the crate's painted label, and each option is one
 * — the e2e `RuleEditorActions.pick` drives `driver.select(id, label)` and
 * `picked` reads the anchor back. `FamilyScreen`'s `PolicySelectDropdown` shape.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LabelPicker(
    testId: String,
    title: String,
    options: List<String>,
    selected: String,
    enabled: Boolean,
    onPick: (Int) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { if (enabled) expanded = !expanded }) {
        OutlinedTextField(
            value = selected,
            onValueChange = {},
            readOnly = true,
            enabled = enabled,
            label = { Text(title) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.menuAnchor().fillMaxWidth().testTag(testId),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEachIndexed { i, option ->
                DropdownMenuItem(
                    text = { Text(option) },
                    onClick = { expanded = false; onPick(i) },
                )
            }
        }
    }
}
