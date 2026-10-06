package com.fauna.app.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Link
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
import uniffi.fauna_core.AuthoringOriginStatus
import social.fauna.generated.Ids

/**
 * The "via connected app" badge (`delegated-origin-badge`), rendered **iff** an
 * EXTERNAL APP authored this post as the account, through the D10 delegated
 * authoring sub-key — the shared `fauna_core::render::AuthoringOriginStatus` is
 * `DELEGATED` (`docs/goal/behavior/atproto-pds-full.md` § Problem 1 → D10 →
 * *Audit*, ratified 2026-07-29). This is what makes the grant *audited* rather
 * than merely revocable: the signed bytes **are** the log, read client-side, so a
 * user scrolling their own feed can tell which posts they did not personally write.
 *
 * The structural twin of [UnverifiedSourceBadge], one field over; mirrors web
 * `DelegatedOriginBadge.svelte` + linux `build_delegated_origin_badge`.
 *
 * The gate is `== DELEGATED`, deliberately narrower than "not DIRECT". `UNKNOWN`
 * covers **both** the undecoded nest-index list card *and* the
 * verification-FAILED case: an unverified wire's `signer_auth` cert is exactly
 * the part nothing authenticated, so badging it would let a forgery paint itself
 * as "merely delegated" — the inversion of an audit surface.
 *
 * The badge names the FACT, never an app: one authoring sub-key is minted per
 * account, so nothing in the signed bytes says WHICH app wrote the post. Neutral
 * (surface-variant) chrome, not the amber caution tier — this is an audit answer,
 * not a warning.
 */
@Composable
fun DelegatedOriginBadge(
    authoringOrigin: AuthoringOriginStatus,
    modifier: Modifier = Modifier
) {
    if (authoringOrigin != AuthoringOriginStatus.DELEGATED) return
    val tooltip = stringResource(R.string.feed_delegated_origin_tooltip)
    Row(
        modifier = modifier
            .testTag(Ids.DELEGATED_ORIGIN_BADGE)
            .semantics { contentDescription = tooltip }
            .background(
                MaterialTheme.colorScheme.surfaceVariant,
                RoundedCornerShape(4.dp)
            )
            .padding(horizontal = 6.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp)
    ) {
        Icon(
            Icons.Default.Link,
            contentDescription = null,
            modifier = Modifier.size(14.dp),
            tint = MaterialTheme.colorScheme.onSurfaceVariant
        )
        Text(
            stringResource(R.string.feed_delegated_origin),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant
        )
    }
}
