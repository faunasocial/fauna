package com.fauna.app.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import uniffi.fauna_core.VerificationStatus
import social.fauna.generated.Ids

/**
 * The muted "unverified source" badge (`unverified-source-badge`), rendered **iff**
 * THIS client could not verify the post's signed envelope — the shared
 * `fauna_core::render::VerificationStatus` is `FAILED`
 * (`docs/goal/architecture/security.md` § App display of unverified content;
 * review F-CL2/F-CL3). No badge for `UNCHECKED` (the default — a trusted nest-index
 * projection with no envelope to verify) or `VERIFIED`. The post body still renders
 * in full; this badge is the visible caveat (the DKIM-fail analogue), so a transient
 * key-rotation-lag false-negative never makes a legitimate post silently vanish.
 *
 * Mirrors web `UnverifiedSourceBadge.svelte` + linux `build_unverified_badge`; the
 * tertiary (caution) colour matches the codebase's amber-warning tier (the
 * `AdminDnsScreen` cert-health caution badge), distinct from `error` (hard fail).
 */
@Composable
fun UnverifiedSourceBadge(verification: VerificationStatus, modifier: Modifier = Modifier) {
    if (verification != VerificationStatus.FAILED) return
    val tooltip = stringResource(R.string.feed_unverified_source_tooltip)
    Row(
        modifier = modifier
            .testTag(Ids.UNVERIFIED_SOURCE_BADGE)
            .semantics { contentDescription = tooltip }
            .background(
                MaterialTheme.colorScheme.tertiaryContainer,
                RoundedCornerShape(4.dp)
            )
            .padding(horizontal = 6.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp)
    ) {
        Icon(
            Icons.Default.Warning,
            contentDescription = null,
            modifier = Modifier.size(14.dp),
            tint = MaterialTheme.colorScheme.tertiary
        )
        Text(
            stringResource(R.string.feed_unverified_source),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.tertiary
        )
    }
}
