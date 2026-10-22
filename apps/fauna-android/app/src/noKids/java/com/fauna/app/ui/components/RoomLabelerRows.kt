package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.LabelerCatalogVM
import com.fauna.ffi.ngramDirectionLabel
import com.fauna.ffi.ngramDocCountLabel
import social.fauna.generated.Ids
import uniffi.fauna_conversations.RoomSettingsDraft
import uniffi.fauna_conversations.roomMayNameLabelerKind
import uniffi.fauna_conversations.roomSettingsLabelerStaged
import uniffi.fauna_conversations.roomSettingsLabelerToggleLive
import uniffi.fauna_conversations.roomSettingsToggleLabeler
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogSnapshot

// The community room's labeler set inside the room policy editor
// (`ui/conversations.md` § Element IDs → *The room's labeler set*) — **the
// built half**. The labeler catalog (machine, view-model, inspect panel) lives
// in the `noKids` source set because the community-labeler catalog is
// third-party anything, which the kids build type compiles out
// (`family-safety.md` § The account age band; `dynamic-features.md` §
// Compile-time excision); the main-source-set `RoomSettingsDialog` therefore
// reaches it through this seam, the way `FaunaNavHost` reaches
// `kidsExcisedDestinations`. The `kids` build type compiles the empty twin
// under `src/kids/java/com/fauna/app/ui/components/RoomLabelerRows.kt`.

/**
 * The labeler rows of the room editor: one `room-labeler-toggle[i]` +
 * `room-labeler-inspect-button[i]` per catalog labeler a room may name, read
 * off the same shared `LabelerCatalogMachine` Settings → Community labelers
 * drives. Painted only while [draft]'s `labelers` is non-null (a community room
 * whose answer this device has read). Every decision — which kinds a room may
 * name, whether a row is staged, whether it may act — is shared Rust's; this
 * paints. Inspect opens the catalog's inspect panel IN PLACE so the editor's
 * draft survives; its Close returns to the rows.
 *
 * Thin VM-backed wrapper over the stateless [RoomLabelerRowsContent]. The
 * view-model is obtained only for a draft that paints the rows, so an editor
 * over a non-community room never builds a catalog machine.
 */
@Composable
fun RoomLabelerRows(
    draft: RoomSettingsDraft,
    canSetPolicy: Boolean,
    onDraftChange: (RoomSettingsDraft) -> Unit,
) {
    if (draft.labelers == null) return
    val vm: LabelerCatalogVM = hiltViewModel()
    val snapshot by vm.snapshot.collectAsState()
    val context = LocalContext.current
    // Opening the editor re-reads the catalog, best-effort: a failed read
    // leaves whatever the machine already holds (or no rows) and never blocks
    // Save.
    LaunchedEffect(Unit) { vm.start() }
    RoomLabelerRowsContent(
        snapshot = snapshot,
        draft = draft,
        canSetPolicy = canSetPolicy,
        onDraftChange = onDraftChange,
        onInspect = vm::inspect,
        onCloseInspect = vm::closeInspect,
        ngramLabels = { more, less ->
            (resolveLocalized(context, ngramDirectionLabel(more, less)) ?: "") to
                (resolveLocalized(context, ngramDocCountLabel(more, less)) ?: "")
        },
    )
}

/** Stateless rendering — see [RoomLabelerRows]. */
@Composable
fun RoomLabelerRowsContent(
    snapshot: LabelerCatalogSnapshot?,
    draft: RoomSettingsDraft,
    canSetPolicy: Boolean,
    onDraftChange: (RoomSettingsDraft) -> Unit,
    onInspect: (Int) -> Unit,
    onCloseInspect: () -> Unit,
    ngramLabels: (UInt, UInt) -> Pair<String, String> = { _, _ -> "" to "" },
) {
    if (draft.labelers == null) return
    snapshot?.inspecting?.let { view ->
        LabelerInspectPanel(view = view, ngramLabels = ngramLabels, onClose = onCloseInspect)
        return
    }
    // Each row keeps its catalog SNAPSHOT index (what inspect addresses), in
    // catalog order.
    val rows = (snapshot?.entries ?: emptyList())
        .withIndex()
        .filter { (_, entry) -> roomMayNameLabelerKind(entry.artifactKind) }
    if (rows.isEmpty()) return
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            stringResource(R.string.conversations_unified_room_labelers_label),
            style = MaterialTheme.typography.labelMedium,
        )
        rows.forEach { (index, entry) ->
            val staged = roomSettingsLabelerStaged(draft, entry.labelerId)
            val live = canSetPolicy && roomSettingsLabelerToggleLive(draft, entry.labelerId)
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                // The toggle's text names the labeler's kind and a prefix of its
                // id (ui.yaml `room-labeler-toggle`; as linux/tui): the catalog
                // row carries no name, and the full id is one inspect away.
                val mark = stringResource(
                    if (staged) {
                        R.string.conversations_unified_room_labeler_on
                    } else {
                        R.string.conversations_unified_room_labeler_off
                    },
                )
                TextButton(
                    onClick = {
                        onDraftChange(roomSettingsToggleLabeler(draft, entry.labelerId))
                    },
                    enabled = live,
                    modifier = Modifier
                        .weight(1f)
                        .testTag(Ids.ROOM_LABELER_TOGGLE)
                        .semantics { stateDescription = if (staged) "true" else "false" },
                ) {
                    Text("${entry.artifactKind} · ${entry.labelerId.take(16)}… · $mark")
                }
                OutlinedButton(
                    onClick = { onInspect(index) },
                    modifier = Modifier.testTag(Ids.ROOM_LABELER_INSPECT_BUTTON),
                ) {
                    Text(stringResource(R.string.labeler_catalog_inspect))
                }
            }
        }
    }
}
