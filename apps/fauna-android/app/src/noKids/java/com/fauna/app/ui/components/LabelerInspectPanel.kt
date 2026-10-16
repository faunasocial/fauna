package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import uniffi.fauna_labeler_catalog_machine.LabelerInspectView
import social.fauna.generated.Ids

/**
 * The `labeler-inspect-panel` — the decoded, client-re-verified signed
 * metadata for one labeler (the trust-gate view). Mirrors linux
 * `render_inspect_panel`'s plain field dump.
 */
@Composable
fun LabelerInspectPanel(
    view: LabelerInspectView,
    ngramLabels: (UInt, UInt) -> Pair<String, String>,
    onClose: () -> Unit,
) {
    Card(modifier = Modifier.testTag(Ids.LABELER_INSPECT_PANEL).padding(vertical = 12.dp)) {
        Column(modifier = Modifier.padding(16.dp)) {
            val text = buildString {
                appendLine("labeler_id: ${view.labelerId}")
                appendLine("version: ${view.version}")
                appendLine("artifact_kind: ${view.artifactKind}")
                appendLine("wasm_hash: ${view.wasmHash}")
                appendLine("wasm_size: ${view.wasmSize}")
                appendLine("needs_text: ${view.needsText}")
                appendLine("needs_hashtags: ${view.needsHashtags}")
                appendLine("needs_media_metadata: ${view.needsMediaMetadata}")
                appendLine("needs_author: ${view.needsAuthor}")
                appendLine("needs_attachment_bytes: ${view.needsAttachmentBytes}")
                appendLine("label_abi: ${view.labelAbi}")
                append("verified: ${view.verified}")
                // A revision past this app's own reads inert and says so
                // (content-moderation-and-ranking.md § Tier-3 → *The output
                // half of the `label()` ABI*, position (ii)).
                if (view.newerThanThisApp) {
                    append("\n")
                    append(stringResource(R.string.labeler_catalog_inspect_needs_newer_app))
                }
            }
            Text(text, modifier = Modifier.testTag(Ids.LABELER_INSPECT_METADATA))

            // List-kind inspect section (content-moderation-and-ranking.md §
            // Tier-3 artifact kinds — inspecting a `list` decodes the raw
            // artifact into the publisher-chosen name + the EXACT id→score
            // map). Mirrors linux `render_inspect_panel`.
            if (view.artifactKind == "list") {
                Text(
                    view.listName?.let {
                        stringResource(R.string.labeler_catalog_list_name).replace("{name}", it)
                    } ?: stringResource(R.string.labeler_catalog_unnamed_list),
                    modifier = Modifier
                        .padding(top = 8.dp)
                        .testTag(Ids.LABELER_INSPECT_LIST_NAME),
                )
                Text(
                    stringResource(R.string.labeler_catalog_list_entry_count)
                        .replace("{count}", view.listEntries.size.toString()),
                    modifier = Modifier.testTag(Ids.LABELER_INSPECT_LIST_ENTRY_COUNT),
                )
                Column(modifier = Modifier.testTag(Ids.LABELER_INSPECT_LIST_ENTRIES)) {
                    view.listEntries.forEach { entry ->
                        Row(modifier = Modifier.testTag(Ids.LABELER_INSPECT_LIST_ENTRY)) {
                            Text(
                                entry.contentId,
                                modifier = Modifier
                                    .weight(1f)
                                    .testTag(Ids.LABELER_INSPECT_LIST_ENTRY_ID),
                            )
                            Text(
                                entry.score.toString(),
                                modifier = Modifier.testTag(Ids.LABELER_INSPECT_LIST_ENTRY_SCORE),
                            )
                        }
                    }
                }
            }

            // text-model-kind inspect section (same § — inspect renders the
            // FULL vocabulary, the model's whole matching surface, before any
            // subscribe). An unnamed model is valid, the List's unnamed rule;
            // the name rides inside the artifact, so inspect is where it
            // first becomes visible. Mirrors the apple/linux legs.
            if (view.artifactKind == "text-model") {
                Text(
                    view.modelName?.let {
                        stringResource(R.string.labeler_catalog_model_name).replace("{name}", it)
                    } ?: stringResource(R.string.labeler_catalog_unnamed_model),
                    modifier = Modifier
                        .padding(top = 8.dp)
                        .testTag(Ids.LABELER_INSPECT_MODEL_NAME),
                )
                Text(
                    stringResource(R.string.labeler_catalog_model_ngram_count)
                        .replace("{count}", view.modelNgrams.size.toString()),
                    modifier = Modifier.testTag(Ids.LABELER_INSPECT_MODEL_NGRAM_COUNT),
                )
                Column(modifier = Modifier.testTag(Ids.LABELER_INSPECT_MODEL_ENTRIES)) {
                    view.modelNgrams.forEach { ngram ->
                        val (direction, count) = ngramLabels(ngram.more, ngram.less)
                        Row(modifier = Modifier.testTag(Ids.LABELER_INSPECT_MODEL_ENTRY)) {
                            Text(
                                ngram.ngram,
                                maxLines = 1,
                                modifier = Modifier
                                    .weight(1f)
                                    .testTag(Ids.LABELER_INSPECT_MODEL_ENTRY_TEXT),
                            )
                            Text(
                                direction,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier
                                    .padding(start = 8.dp)
                                    .testTag(Ids.LABELER_INSPECT_MODEL_ENTRY_DIRECTION),
                            )
                            Text(
                                count,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier
                                    .padding(start = 8.dp)
                                    .testTag(Ids.LABELER_INSPECT_MODEL_ENTRY_COUNT),
                            )
                        }
                    }
                }
            }

            OutlinedButton(
                onClick = onClose,
                modifier = Modifier
                    .testTag(Ids.LABELER_INSPECT_CLOSE_BUTTON)
                    .padding(top = 8.dp),
            ) {
                Text(stringResource(R.string.labeler_catalog_close_inspect))
            }
        }
    }
}
