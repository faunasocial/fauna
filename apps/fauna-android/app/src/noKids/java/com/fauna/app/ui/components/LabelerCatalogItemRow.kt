package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogEntry
import social.fauna.generated.Ids

/**
 * One `labeler-catalog-item` row (`tests/e2e-unified/ui.yaml` shared component,
 * `used_in: [labeler-catalog, personalization]`): publisher / content-kind /
 * version / factor, plus inspect + subscribe/unsubscribe gestures gated by
 * [showInspectSubscribe] — on the labeler-catalog page (all published
 * labelers) inspect + subscribe render; on the personalization home (only
 * [entry].subscribed rows) they're hidden and only unsubscribe shows. Mirrors
 * linux `build_labeler_row` (`apps/fauna-linux/src/views/personalization/mod.rs`).
 *
 * [kindBadge] is the `labeler-catalog-item-kind` text — the raw
 * `artifact_kind` discriminator, EXCEPT for a `text-model` whose version this
 * build cannot score, which says so instead (`fauna_core::format::text_model_needs_newer_app`,
 * the unknown-version contract's "says so" half). The shared face is the
 * screen's to call (FFI stays off the testable content, the
 * `BackupDestinationsSection.destinationLabel` shape); the default paints the
 * discriminator unchanged.
 */
@Composable
fun LabelerCatalogItemRow(
    entry: LabelerCatalogEntry,
    index: Int,
    showInspectSubscribe: Boolean,
    onInspect: (Int) -> Unit,
    onSubscribe: (Int) -> Unit,
    onUnsubscribe: (Int) -> Unit,
    modifier: Modifier = Modifier,
    kindBadge: (LabelerCatalogEntry) -> String = { it.artifactKind },
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp)
            .testTag(Ids.LABELER_CATALOG_ITEM),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(entry.publisherActor, modifier = Modifier.testTag(Ids.LABELER_CATALOG_ITEM_PUBLISHER))
            // The artifact kind (list | wasm | text-model, machine-normalized)
            // — a curated List is distinguishable from an executable module
            // BEFORE inspect (content-moderation-and-ranking.md § Tier-3
            // artifact kinds). Rendered verbatim, no localization, save the
            // "needs a newer app" override — mirrors linux/web/apple.
            Text(kindBadge(entry), modifier = Modifier.testTag(Ids.LABELER_CATALOG_ITEM_KIND))
            Text(entry.contentKind, modifier = Modifier.testTag(Ids.LABELER_CATALOG_ITEM_CONTENT_KIND))
            Text(
                entry.version.toString(),
                modifier = Modifier.testTag(Ids.LABELER_CATALOG_ITEM_VERSION),
            )
            Text(
                entry.factor,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.LABELER_CATALOG_ITEM_FACTOR),
            )
        }

        if (showInspectSubscribe) {
            OutlinedButton(
                onClick = { onInspect(index) },
                modifier = Modifier.testTag(Ids.LABELER_CATALOG_ITEM_INSPECT_BUTTON),
            ) {
                Text(stringResource(R.string.labeler_catalog_inspect))
            }
            if (!entry.subscribed) {
                Button(
                    onClick = { onSubscribe(index) },
                    modifier = Modifier.testTag(Ids.LABELER_CATALOG_ITEM_SUBSCRIBE_BUTTON),
                ) {
                    Text(stringResource(R.string.labeler_catalog_subscribe))
                }
            }
        }

        if (entry.subscribed) {
            OutlinedButton(
                onClick = { onUnsubscribe(index) },
                modifier = Modifier.testTag(Ids.LABELER_CATALOG_ITEM_UNSUBSCRIBE_BUTTON),
            ) {
                Text(stringResource(R.string.labeler_catalog_unsubscribe))
            }
        }
    }
}
