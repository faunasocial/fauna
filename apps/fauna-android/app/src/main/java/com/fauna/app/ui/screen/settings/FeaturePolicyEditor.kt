package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.editorNoteText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.localizedNested
import com.fauna.app.ui.viewmodel.FeatureAuthoringState
import com.fauna.app.ui.viewmodel.FeatureEditorActions
import com.fauna.ffi.FfiPolicyEditorView
import social.fauna.generated.Ids

/**
 * The shared `feature-policy-editor` family's paint — ONE composable for all
 * three of its hosts (`dynamic-features.md` § Authoring surfaces): the admin
 * Nest page opens it at the admin tier, a Status feature-limits row at the self
 * tier, the Family page's feature-limits section at the guardian tier. The twin of tui's, linux's and web's editor paints.
 *
 * **This paints; it decides nothing.** The draft, the cell list and its order,
 * the labels, the no-effect notes, whether *Remove limit* renders and the
 * parser behind *Save* are all `FfiPolicyEditor`'s (shared Rust), so the
 * editor cannot differ between the two hosts or across the 7 apps. The hosts
 * differ only in the tier they open it at, which decides the write kind the
 * offline gate reads. Plain parameters + callbacks only, so it renders under
 * Robolectric with no Hilt, no VM and no FFI.
 *
 * Save and Remove are `OnlineOnly` (`dynamic-features.md` § Authoring
 * surfaces): [faunaGate] greys them from the tier's own write kind, declared
 * as literals so `offline-gate-check` can grade them against the shared table.
 * Cancel is local and stays live.
 */
@Composable
fun FeaturePolicyEditorPane(
    view: FfiPolicyEditorView,
    status: String?,
    working: Boolean,
    onSetOn: (Boolean) -> Unit,
    onSetCell: (index: Int, text: String) -> Unit,
    onSave: () -> Unit,
    onRemove: () -> Unit,
    onCancel: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    // One declaration per tier, each a literal first argument (the checker
    // grades a literal, not a computed kind); the view's tier picks the verdict.
    val adminGate = faunaGate("fauna.features.policy.update", enabled = !working)
    val selfGate = faunaGate("fauna.features.self_limits.update", enabled = !working)
    val guardianGate = faunaGate("fauna.family.policy.update", enabled = !working)
    val gate = when (view.tier) {
        "admin" -> adminGate
        "guardian" -> guardianGate
        else -> selfGate
    }

    Card(
        modifier = modifier.fillMaxWidth().testTag(Ids.FEATURE_POLICY_EDITOR),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant),
    ) {
        Column(
            modifier = Modifier.padding(12.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(
                localizedNested(view.title).orEmpty(),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.FEATURE_POLICY_EDITOR_TITLE),
            )

            // A one-of-two radio pair — exactly two choices, never a third
            // "Limit": whether On carries bounds is the cells' business.
            EditorRadioRow(
                testTag = Ids.FEATURE_POLICY_EDITOR_ON_RADIO,
                selected = view.on,
                label = stringResource(R.string.features_editor_on),
                onClick = { onSetOn(true) },
            )
            EditorRadioRow(
                testTag = Ids.FEATURE_POLICY_EDITOR_OFF_RADIO,
                selected = !view.on,
                label = stringResource(R.string.features_editor_off),
                onClick = { onSetOn(false) },
            )

            // An outer tier already denies: whatever is chosen here changes
            // nothing, and the editor says so rather than refusing anything.
            view.offNote?.let { note ->
                Text(
                    localized(note).orEmpty(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (view.cells.isNotEmpty()) {
                Text(
                    localized(view.hint).orEmpty(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            view.cells.forEachIndexed { index, cell ->
                Column(
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FEATURE_POLICY_EDITOR_CELL),
                    verticalArrangement = Arrangement.spacedBy(2.dp),
                ) {
                    Text(
                        localizedNested(cell.label).orEmpty(),
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.testTag(Ids.FEATURE_POLICY_EDITOR_CELL_LABEL),
                    )
                    OutlinedTextField(
                        value = cell.text,
                        onValueChange = { onSetCell(index, it) },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth().testTag(Ids.FEATURE_POLICY_EDITOR_CELL_INPUT),
                    )
                    // Present only when the ceiling already binds at or below
                    // the typed value — the note is the shared editor's.
                    editorNoteText(context, cell)?.let { note ->
                        Text(
                            note,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.testTag(Ids.FEATURE_POLICY_EDITOR_CELL_NOTE),
                        )
                    }
                }
            }

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = onSave,
                    enabled = gate.enabled,
                    modifier = Modifier.testTag(Ids.FEATURE_POLICY_EDITOR_SAVE_BUTTON),
                ) { Text(stringResource(R.string.features_editor_save)) }
                if (view.canRemove) {
                    OutlinedButton(
                        onClick = onRemove,
                        enabled = gate.enabled,
                        modifier = Modifier.testTag(Ids.FEATURE_POLICY_EDITOR_REMOVE_BUTTON),
                    ) { Text(stringResource(R.string.features_editor_remove)) }
                }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.FEATURE_POLICY_EDITOR_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.features_editor_cancel)) }
            }
            DisabledControlReasonText(gate.reason)

            status?.let {
                Text(it, modifier = Modifier.testTag(Ids.FEATURE_POLICY_EDITOR_STATUS))
            }
        }
    }
}

@Composable
private fun EditorRadioRow(
    testTag: String,
    selected: Boolean,
    label: String,
    onClick: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .selectable(selected = selected, role = Role.RadioButton, onClick = onClick)
            .testTag(testTag),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        RadioButton(selected = selected, onClick = null)
        Text(label, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
    }
}

/**
 * The Family page's *Feature limits* section for the SELECTED ward
 * (`family-policy-feature-limits-*`; `family-safety.md` § App surface →
 * *Feature limits — the guardian host*): one row per gated feature — its name,
 * the guardian's AUTHORED document for this ward in words, and an Edit button
 * that opens the shared editor at the guardian tier right under the row. The
 * section label paints with the ward; the rows only once the authored read has
 * answered (a summary painted before that would say "No limit set" with no
 * basis).
 */
@Composable
fun FeatureLimitsGuardianSection(
    authoring: FeatureAuthoringState,
    editor: FeatureEditorActions,
) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
        Text(
            stringResource(R.string.features_guardian_section_title),
            style = MaterialTheme.typography.titleSmall,
            modifier = Modifier.testTag(Ids.FAMILY_POLICY_FEATURE_LIMITS_SECTION),
        )
        Text(
            stringResource(R.string.features_guardian_section_desc),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        authoring.rows?.forEach { row ->
            Column(
                modifier = Modifier.fillMaxWidth().testTag(Ids.FAMILY_POLICY_FEATURE_LIMITS_ROW),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            localized(row.name).orEmpty(),
                            style = MaterialTheme.typography.titleSmall,
                            modifier = Modifier.testTag(Ids.FAMILY_POLICY_FEATURE_LIMITS_NAME),
                        )
                        Text(
                            localized(row.summary).orEmpty(),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.testTag(Ids.FAMILY_POLICY_FEATURE_LIMITS_SUMMARY),
                        )
                    }
                    OutlinedButton(
                        onClick = { editor.onOpen(row.feature) },
                        modifier = Modifier.testTag(Ids.FAMILY_POLICY_FEATURE_LIMITS_EDIT_BUTTON),
                    ) { Text(stringResource(R.string.features_admin_edit)) }
                }
                val view = authoring.view
                if (authoring.openFeature == row.feature && view != null) {
                    FeaturePolicyEditorPane(
                        view = view,
                        status = authoring.status,
                        working = authoring.working,
                        onSetOn = editor.onSetOn,
                        onSetCell = editor.onSetCell,
                        onSave = editor.onSave,
                        onRemove = editor.onRemove,
                        onCancel = editor.onCancel,
                    )
                }
            }
        }
    }
}

/**
 * The admin Nest page's *Feature limits for everyone* section
 * (`admin-nest-feature-limits-*`; `admin.md` § N Nest → *Feature limits*): one
 * row per gated feature the nest carries — its name, the admin's AUTHORED
 * document in words, and an Edit button that opens the shared editor at the
 * admin tier right under the row. Absent until the authored read has answered
 * (a summary painted before that would say "No limit set" with no basis).
 */
@Composable
fun FeatureLimitsAdminSection(
    authoring: FeatureAuthoringState,
    editor: FeatureEditorActions,
) {
    val rows = authoring.rows ?: return
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_FEATURE_LIMITS_SECTION)) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.features_admin_section_title),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.features_admin_section_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            rows.forEach { row ->
                Column(
                    modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_FEATURE_LIMITS_ROW),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                localized(row.name).orEmpty(),
                                style = MaterialTheme.typography.titleSmall,
                                modifier = Modifier.testTag(Ids.ADMIN_NEST_FEATURE_LIMITS_NAME),
                            )
                            Text(
                                localized(row.summary).orEmpty(),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.testTag(Ids.ADMIN_NEST_FEATURE_LIMITS_SUMMARY),
                            )
                        }
                        OutlinedButton(
                            onClick = { editor.onOpen(row.feature) },
                            modifier = Modifier.testTag(Ids.ADMIN_NEST_FEATURE_LIMITS_EDIT_BUTTON),
                        ) { Text(stringResource(R.string.features_admin_edit)) }
                    }
                    val view = authoring.view
                    if (authoring.openFeature == row.feature && view != null) {
                        FeaturePolicyEditorPane(
                            view = view,
                            status = authoring.status,
                            working = authoring.working,
                            onSetOn = editor.onSetOn,
                            onSetCell = editor.onSetCell,
                            onSave = editor.onSave,
                            onRemove = editor.onRemove,
                            onCancel = editor.onCancel,
                        )
                    }
                }
            }
        }
    }
}
