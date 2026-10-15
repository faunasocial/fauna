package com.fauna.app.ui.screen.status

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.screen.settings.FeaturePolicyEditorPane
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.localizedNested
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.FeatureAuthoringState
import com.fauna.app.ui.viewmodel.FeatureEditorActions
import social.fauna.generated.Ids
import uniffi.fauna_core.LocalizedText

/**
 * The Status sub-page's two "what bounds me" sections — Quota
 * (`quota-section`, `settings.md` § Layout & flow item 2) and Feature limits
 * (`feature-limits-section`, item 2b), the second directly after the first.
 * Both live here, on every sub-paged shell's Status page (linux's
 * `views/status.rs`, apple's `MacStatusView`, web's Status sub-page) — android
 * painted them on the Account page until moved them.
 *
 * Stateless, plain parameters, the FFI folds injected, so they render under
 * Robolectric with no Hilt, no VM and no native library.
 */
@Composable
fun QuotaSection(
    quota: com.fauna.ffi.FfiQuotaGetReply,
    // Shared `fauna_core::format::quota_fraction`/`quota_percent`
    // (value-formatting.md § Quota fraction) — the 0.0..=1.0 bar-fill fraction
    // (guards maxBytes <= 0 → 0, clamps over-quota → 1) and the whole percent,
    // single-sourced with linux/web/windows; injected FFI-free.
    quotaFraction: (Long, Long) -> Double = { used, max -> com.fauna.ffi.quotaFraction(used, max) },
    quotaPercent: (Long, Long) -> UInt = { used, max -> com.fauna.ffi.quotaPercent(used, max) },
    // `ValueFormat.byteSize`'s own native half (`fauna_core::format::byte_size`),
    // injected FFI-free; the Context-dependent resolution half touches no
    // native code and stays inline.
    byteSize: (kotlin.ULong) -> LocalizedText = { com.fauna.ffi.byteSize(it) },
) {
    val context = LocalContext.current
    val byteSizeText = { bytes: Long -> resolveLocalized(context, byteSize(bytes.toULong())) ?: "" }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.QUOTA_SECTION)) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(stringResource(R.string.common_storage), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(12.dp))

            // Headline bar tracks the storage resource; the rows below break
            // out the distinct inbox / storage / device quotas the rich
            // fauna.quota.get reply carries.
            val storageUsed = quota.storage.usedBytes
            val storageLimit = quota.storage.maxBytes
            val progress = quotaFraction(storageUsed, storageLimit).toFloat()
            LinearProgressIndicator(
                progress = { progress },
                modifier = Modifier.fillMaxWidth().height(8.dp).testTag(Ids.SETTINGS_STORAGE_BAR),
            )
            Spacer(Modifier.height(8.dp))

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    "${byteSizeText(storageUsed)} / ${byteSizeText(storageLimit)}",
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.testTag(Ids.SETTINGS_STORAGE_TEXT),
                )
                Text(
                    "${quotaPercent(storageUsed, storageLimit)}%",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            Spacer(Modifier.height(8.dp))

            Text(
                "${stringResource(R.string.common_inbox)}: ${byteSizeText(quota.inbox.usedBytes)} / ${byteSizeText(quota.inbox.maxBytes)}",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.QUOTA_INBOX),
            )
            Text(
                "${stringResource(R.string.common_storage)}: ${byteSizeText(storageUsed)} / ${byteSizeText(storageLimit)}",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.QUOTA_STORAGE),
            )
            Text(
                "${stringResource(R.string.common_devices)}: ${quota.devices.used} / ${quota.devices.max}",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.QUOTA_DEVICES),
            )
        }
    }
}

/**
 * The Feature limits section — the gated-feature plane's transparency read
 * (`feature-limits-section`, `dynamic-features.md` § Transparency &
 * auditability, boundary 4: "no silent gates"), plus the self-limits control
 * on each row (`settings.md` § Layout & flow item 2b; the editor is
 * `dynamic-features.md` § Authoring surfaces'). This block paints; it decides
 * nothing — every judgement (which cells survived the tier meet, per-cell tier
 * attribution, available/restricted/hidden) is `FfiFeaturesClient.rows()`'s
 * output, mirroring tui's `feature_limits_elements` and linux's
 * `views::status::update_features` field-for-field.
 *
 * [authoring] is the self host's state: a row gains its `-own-summary` /
 * `-own-edit-button` only once the authored read has answered and the nest
 * carries the member (a summary painted before that would say "No limit set"
 * with no basis), and the shared editor opens right under the row.
 */
@Composable
fun FeatureLimitsSection(
    rows: List<com.fauna.ffi.FfiFeatureRow>,
    authoring: FeatureAuthoringState = FeatureAuthoringState(),
    editor: FeatureEditorActions = FeatureEditorActions.None,
) {
    // A `hidden` row means this nest build does not carry the feature at all
    // (its capability token is absent) — filtered client-side, not in the
    // shared crate, same as every other app.
    val visible = rows.filter { it.affordance != "hidden" }
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.FEATURE_LIMITS_SECTION)) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                stringResource(R.string.features_section_title),
                style = MaterialTheme.typography.titleMedium,
            )
            Spacer(Modifier.height(12.dp))
            if (visible.isEmpty()) {
                Text(
                    stringResource(R.string.features_empty),
                    modifier = Modifier.testTag(Ids.FEATURE_LIMITS_EMPTY),
                )
            } else {
                visible.forEach { row ->
                    FeatureLimitRow(row) {
                        authoring.rows?.firstOrNull { it.feature == row.feature }?.let { own ->
                            OwnLimitControl(own, authoring, editor)
                        }
                    }
                }
            }
        }
    }
}

/**
 * One `feature-limits-row` — a registry member's name, status, restriction (if
 * any), and its bound cells. Mirrors tui's `feature_limits_elements` /
 * linux's `views::status::update_features` field-for-field so the answer to
 * "why can't I do this" cannot differ across apps (priority #1). [own] is the
 * row's authoring slot.
 */
@Composable
private fun FeatureLimitRow(row: com.fauna.ffi.FfiFeatureRow, own: @Composable ColumnScope.() -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp)
            .testTag(Ids.FEATURE_LIMITS_ROW),
    ) {
        Text(
            localized(row.name) ?: "",
            style = MaterialTheme.typography.titleSmall,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_NAME),
        )
        Text(
            localized(row.status) ?: "",
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_STATUS),
        )
        // Only when something actually blocks — boundary 4's "no silent
        // gates" half. `localizedNested`, not `localized`: the sentence's
        // `{window}` is itself an i18n key.
        row.restriction?.let { reason ->
            Text(
                localizedNested(reason) ?: "",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.FEATURE_LIMITS_RESTRICTION),
            )
        }
        row.cells.forEach { cell -> FeatureLimitQuotaCell(cell) }
        own()
    }
}

/**
 * One `feature-limits-quota` cell — per CELL, not per row: the meet takes the
 * MIN per (dimension, window), so two bounds on one feature can come from
 * different tiers.
 */
@Composable
private fun FeatureLimitQuotaCell(cell: com.fauna.ffi.FfiLimitCell) {
    val context = LocalContext.current
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 12.dp)
            .testTag(Ids.FEATURE_LIMITS_QUOTA),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            localizedNested(cell.label) ?: "",
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_QUOTA_LABEL),
        )
        Text(
            com.fauna.app.ui.util.cellValueText(context, cell),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_QUOTA_VALUE),
        )
        Text(
            localized(cell.tierLabel) ?: "",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_QUOTA_TIER),
        )
    }
}

/**
 * The user's OWN limit for one member, in words, and the control that opens
 * the shared editor at the self tier right under the row. Both writes are
 * `OnlineOnly`, so the buttons inside the editor are gated; this control only
 * opens a local draft.
 */
@Composable
private fun OwnLimitControl(
    own: com.fauna.ffi.FfiAuthoredRow,
    authoring: FeatureAuthoringState,
    editor: FeatureEditorActions,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(start = 12.dp, top = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(
                stringResource(R.string.features_own_label),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                localized(own.summary) ?: "",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.FEATURE_LIMITS_OWN_SUMMARY),
            )
        }
        OutlinedButton(
            onClick = { editor.onOpen(own.feature) },
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_OWN_EDIT_BUTTON),
        ) { Text(stringResource(R.string.features_own_edit)) }
    }
    val view = authoring.view
    if (authoring.openFeature == own.feature && view != null) {
        FeaturePolicyEditorPane(
            view = view,
            status = authoring.status,
            working = authoring.working,
            onSetOn = editor.onSetOn,
            onSetCell = editor.onSetCell,
            onSave = editor.onSave,
            onRemove = editor.onRemove,
            onCancel = editor.onCancel,
            modifier = Modifier.padding(start = 12.dp, top = 4.dp),
        )
    }
}
