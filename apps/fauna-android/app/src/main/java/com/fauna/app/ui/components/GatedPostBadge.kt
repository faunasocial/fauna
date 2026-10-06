package com.fauna.app.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.fauna.app.R
import social.fauna.generated.Ids

/**
 * The gated-post badge (`gated-post-badge`) — a compact pill naming the
 * subscription tier a post is gated to (feed.md § Encryption at rest;
 * monetization.md § Pillars 2+3). Rendered on the feed card + post detail
 * whenever `PostSummary.gated_tier` is set; the reader sees the public teaser
 * until they open the post and the shared `FeedManager` unseals the full body
 * (author custody, or the reader's KeyBlob wrap entry).
 *
 * The text is the tier name itself, not a localized string — the tier name is
 * user-chosen — matching web/linux; `gated_badge_tooltip` is the only localized
 * badge copy and is a hover affordance the touch UI omits.
 *
 * [roomLabel] is `PostSummary.room_label` (`Some` exactly when this reader
 * sits on the room's floor, `ui/feed.md` § Encryption at rest →
 * *Room-restricted — the app half*, the card bullet): when set, it overrides
 * the tier text with the composer's own "Room: ‹label›" string
 * (`feed_post_gate_room`) — the member's reading of a room post, versus the
 * reserved tier `room` every other reader's card still shows. The touch UI has
 * no hover for `gated_badge_room_tooltip`, so it lands as the badge's
 * accessibility `contentDescription` instead — same idiom as
 * [UnverifiedSourceBadge]'s `tooltip`.
 */
@Composable
fun GatedPostBadge(tier: String, roomLabel: String? = null, modifier: Modifier = Modifier) {
    val text = if (roomLabel != null) {
        stringResource(R.string.feed_post_gate_room).replace("{room}", roomLabel)
    } else {
        tier
    }
    val badgeModifier = if (roomLabel != null) {
        val tooltip = stringResource(R.string.feed_post_gated_badge_room_tooltip)
            .replace("{room}", roomLabel)
        modifier.semantics { contentDescription = tooltip }
    } else {
        modifier
    }
    Text(
        text = text,
        fontSize = 10.sp,
        fontWeight = FontWeight.Medium,
        color = MaterialTheme.colorScheme.onSecondaryContainer,
        modifier = badgeModifier
            .testTag(Ids.GATED_POST_BADGE)
            .clip(RoundedCornerShape(999.dp))
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .padding(horizontal = 6.dp, vertical = 2.dp),
    )
}
