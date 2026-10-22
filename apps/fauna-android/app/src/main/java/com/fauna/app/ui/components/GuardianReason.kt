package com.fauna.app.ui.components

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import social.fauna.generated.Ids

/**
 * The guardian rule's reason, verbatim, beside a family-policy placeholder's
 * fixed frame (`content-policy-blocked-reason`; `nest-rules.md` § The shared
 * rule editor → *The reason is the member's to read*). Paints nothing when no
 * rule of the guardian's document drove the verdict ([reason] `null`). The
 * block and collapse arms of the feed card, the post detail and the
 * conversation bubble all paint through here, so the five surfaces cannot
 * drift on the id or the placement.
 */
@Composable
fun GuardianReason(reason: String?, modifier: Modifier = Modifier) {
    if (reason == null) return
    Text(
        reason,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = modifier.testTag(Ids.CONTENT_POLICY_BLOCKED_REASON),
    )
}
