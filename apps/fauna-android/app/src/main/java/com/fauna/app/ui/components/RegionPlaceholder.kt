package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.core.RegionPlaneHost
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.ffi.FfiRegionPlaceholder
import androidx.compose.ui.res.stringResource
import social.fauna.generated.Ids

/**
 * The region placeholder painted **in place of** a region-withheld item
 * (`region-blocking.md` § The blocked render and the transparency surface): the
 * app's frame naming the region and its authority (`region-blocked-notice`), the
 * authority's name (`region-blocked-authority`), and its reason verbatim
 * (`region-blocked-reason`); a `collapse` adds the reveal
 * (`region-collapsed-reveal-button`), which runs [onReveal] — the surface's own
 * family reveal, so one reveal set serves both arms. linux's
 * `region::placeholder_box`, web's `RegionPlaceholder.svelte`.
 *
 * [witnessKey] names the surface + item for convention 17's painted side
 * ([RegionPlaneHost.witnessPainted]): a `block` placeholder registers while
 * composed.
 */
@Composable
fun RegionPlaceholder(
    placeholder: FfiRegionPlaceholder,
    witnessKey: String,
    onReveal: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val isBlock = placeholder.verb == "block"
    DisposableEffect(witnessKey, isBlock) {
        RegionPlaneHost.witnessPainted(witnessKey, isBlock)
        onDispose { RegionPlaneHost.witnessPainted(witnessKey, false) }
    }
    Column(
        modifier = modifier.padding(vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            stringResourceFmt(
                if (isBlock) R.string.region_blocked_notice else R.string.region_collapsed_notice,
                placeholder.region,
                placeholder.authorityName,
            ),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.REGION_BLOCKED_NOTICE),
        )
        Text(
            placeholder.authorityName,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.REGION_BLOCKED_AUTHORITY),
        )
        Text(
            placeholder.reason,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.REGION_BLOCKED_REASON),
        )
        if (!isBlock) {
            TextButton(
                onClick = onReveal,
                modifier = Modifier.testTag(Ids.REGION_COLLAPSED_REVEAL_BUTTON),
            ) {
                Text(stringResource(R.string.region_reveal_button))
            }
        }
    }
}

/**
 * Convention 17's verdict side for one on-screen item: registers [key] in the
 * blocked set while composed and the region blocks it
 * ([RegionPlaneHost.witnessBlocked]). Call it from the item's composable with the
 * verdict it computed off its snapshot — before any arm branches, so an arm that
 * drops the placeholder still counts here.
 */
@Composable
fun RegionBlockWitness(key: String, isBlocked: Boolean) {
    DisposableEffect(key, isBlocked) {
        RegionPlaneHost.witnessBlocked(key, isBlocked)
        onDispose { RegionPlaneHost.witnessBlocked(key, false) }
    }
}
