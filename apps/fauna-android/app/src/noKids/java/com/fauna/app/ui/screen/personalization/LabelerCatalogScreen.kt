package com.fauna.app.ui.screen.personalization

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.LabelerCatalogItemRow
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.LabelerCatalogVM
import com.fauna.ffi.ngramDirectionLabel
import com.fauna.ffi.ngramDocCountLabel
import com.fauna.ffi.textModelNeedsNewerApp
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogEntry
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogSnapshot
import uniffi.fauna_labeler_catalog_machine.LabelerInspectView
import social.fauna.generated.Ids

/**
 * The **Community labelers** catalog (`ui.yaml` page `labeler-catalog`;
 * `content-moderation-and-ranking.md` § Tier-3 community models — browse +
 * inspect-before-subscribe over the tier_3-proven
 * `fauna.labelers.{list,inspect,subscribe,unsubscribe}` wire). Every published
 * labeler, each with an inspect gesture (fetches + client-side re-verifies the
 * full signed metadata — the transparency trust gate, holds even against a
 * lying nest) and subscribe/unsubscribe. Reached from the personalization
 * home's browse-catalog button. Backed by the shared [LabelerCatalogVM]
 * (mirrors linux `CatalogShell`). Stateless [LabelerCatalogContent] renders
 * under the Compose test harness FFI-free.
 */
@Composable
fun LabelerCatalogScreen(
    navController: NavController,
    vm: LabelerCatalogVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(Unit) { vm.start() }

    val errorText = localized(snapshot?.error)
    LaunchedEffect(errorText) {
        if (errorText != null) appMessages.showError(errorText)
    }

    LabelerCatalogContent(
        snapshot = snapshot,
        onBack = { navController.popBackStack() },
        onInspect = vm::inspect,
        onSubscribe = vm::subscribe,
        onUnsubscribe = vm::unsubscribe,
        onCloseInspect = vm::closeInspect,
        // The two shared faces, off the testable content (FFI lives here):
        // the kind badge's "needs a newer app" override, and the model-inspect
        // row labels — the SAME faces the publisher's review rows render, so
        // the two ends cannot disagree about what a row means.
        kindBadge = { entry -> kindBadgeText(context, entry) },
        ngramLabels = { more, less ->
            (resolveLocalized(context, ngramDirectionLabel(more, less)) ?: "") to
                (resolveLocalized(context, ngramDocCountLabel(more, less)) ?: "")
        },
    )
}

/**
 * `labeler-catalog-item-kind`: the raw `artifact_kind` discriminator, or the
 * shared face's "needs a newer app" override for a `text-model` at a version
 * this build does not implement (content-moderation-and-ranking.md § Tier-3
 * artifact kinds, the unknown-version contract's "says so" half). `null`
 * from the face means paint the discriminator unchanged — never re-derived
 * here. Shared by the catalog page and the personalization home's
 * subscribed rows.
 */
fun kindBadgeText(context: android.content.Context, entry: LabelerCatalogEntry): String =
    textModelNeedsNewerApp(entry.artifactKind, entry.artifactVersion)
        ?.let { resolveLocalized(context, it) }
        ?: entry.artifactKind

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LabelerCatalogContent(
    snapshot: LabelerCatalogSnapshot?,
    onBack: () -> Unit,
    onInspect: (Int) -> Unit,
    onSubscribe: (Int) -> Unit,
    onUnsubscribe: (Int) -> Unit,
    onCloseInspect: () -> Unit,
    kindBadge: (LabelerCatalogEntry) -> String = { it.artifactKind },
    ngramLabels: (UInt, UInt) -> Pair<String, String> = { _, _ -> "" to "" },
) {
    val entries = snapshot?.entries ?: emptyList()

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.labeler_catalog_title),
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
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 16.dp)
                .verticalScroll(rememberScrollState())
                .testTag(Ids.LABELER_CATALOG),
        ) {
            snapshot?.inspecting?.let { view ->
                InspectPanel(view = view, ngramLabels = ngramLabels, onClose = onCloseInspect)
            }

            // Three renderable states, not two (README.md § List pages: loading
            // is not empty): `entries` is empty both before the first
            // `fauna.labelers.list` returns and after one that found nothing.
            // While unloaded compose NEITHER element — not the empty text and
            // not the (then-empty) list container, whose bare presence would
            // otherwise be a fourth state nobody specified. The ABSENCE of
            // `labeler-catalog-empty` beside zero `labeler-catalog-item` rows
            // is what identifies loading; no `*-loading` id is minted.
            val loaded = snapshot?.loaded == true
            if (loaded && entries.isEmpty()) {
                Text(
                    stringResource(R.string.labeler_catalog_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier
                        .testTag(Ids.LABELER_CATALOG_EMPTY)
                        .padding(top = 12.dp),
                )
            } else if (loaded) {
                Column(modifier = Modifier.testTag(Ids.LABELER_CATALOG_LIST)) {
                    entries.withIndex().forEach { (index, entry) ->
                        LabelerCatalogItemRow(
                            entry = entry,
                            index = index,
                            showInspectSubscribe = true,
                            onInspect = onInspect,
                            onSubscribe = onSubscribe,
                            onUnsubscribe = onUnsubscribe,
                            kindBadge = kindBadge,
                        )
                    }
                }
            }
        }
    }
}

/**
 * The `labeler-inspect-panel` — the decoded, client-re-verified signed
 * metadata for one labeler (the trust-gate view). Mirrors linux
 * `render_inspect_panel`'s plain field dump.
 */
@Composable
private fun InspectPanel(
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
                append("verified: ${view.verified}")
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
