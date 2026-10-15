package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.fauna.ffi.classifySources
import social.fauna.generated.Ids
import uniffi.fauna_core.BridgeIdentitySnapshot

/**
 * Render one `protocol-badge` per classified post source.
 *
 * The wire [source] field is a comma-separated origin-protocol list
 * (`"fauna, bluesky"`, docs/goal/ui/feed.md § Posts). Classification, deduplication
 * and canonical display labels are **shared Rust** — `fauna_feed::classify_sources`
 * via the `classifySources` UniFFI façade (returning `{ id, label, glyph }`, the same
 * shape web reads). Android keeps only its `SourceGlyph → emoji` map ([sourceGlyphEmoji],
 * the SAME map the conversations rail uses), keyed off the precomputed `glyph` concept —
 * so the feed badge and the rail can't drift (render-model.md § Deltas → D5). An empty or
 * unknown-only-empty field renders no badge; a multi-source post renders one badge per
 * origin.
 *
 * [bridgeRoster] is the feed snapshot's `bridgeRoster`: a token naming a listed bridge is
 * that bridge's badge, which paints its declared label beside the glyph (`paintsLabel`;
 * `ui/feed.md` § Implementation status today, `SourceKind::Bridged`) — a first-party
 * badge stays its glyph alone.
 */
@Composable
fun ProtocolBadge(source: String, bridgeRoster: List<BridgeIdentitySnapshot>, modifier: Modifier = Modifier) {
    val badges = classifySources(source, bridgeRoster)
    if (badges.isEmpty()) return
    Row(modifier = modifier, horizontalArrangement = Arrangement.spacedBy(2.dp)) {
        for (badge in badges) {
            val glyph = sourceGlyphEmoji(badge.glyph)
            Text(
                if (badge.paintsLabel) "$glyph ${badge.label}" else glyph,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.PROTOCOL_BADGE),
            )
        }
    }
}
