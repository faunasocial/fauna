package com.fauna.app.ui.components

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for [GatedPostBadge] — the `gated-post-badge` pill that
 * names the subscription tier a post is gated to (feed.md § Encryption at rest).
 * A pure leaf taking a plain tier `String` (the tier ships on the shared snapshot
 * one layer up), so it runs on the host JVM with no FFI seam / `.so`, exactly like
 * [com.fauna.app.ui.screen.feed.FeedInteractionBarTest]. The e2e asserts the tier
 * name is a substring of the badge text (`tier in gated_badge_text(0)`); the badge
 * text IS the tier name, matching web/linux.
 *
 * The [roomLabel] cases cover the room-restricted post card
 * (`ui/feed.md` § Encryption at rest → *Room-restricted — the app half*, the
 * card bullet): a member sees "Room: ‹label›" over the reserved tier text.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class GatedPostBadgeTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun rendersTierNameWithTestTag() {
        composeTestRule.setContent { GatedPostBadge(tier = "Gold") }
        composeTestRule.onNodeWithTag("gated-post-badge").assertTextEquals("Gold")
    }

    @Test
    fun rendersDistinctTierName() {
        composeTestRule.setContent { GatedPostBadge(tier = "Patrons") }
        composeTestRule.onNodeWithTag("gated-post-badge").assertTextEquals("Patrons")
    }

    @Test
    fun roomLabelSet_showsRoomTextOverTheReservedTier() {
        composeTestRule.setContent { GatedPostBadge(tier = "room", roomLabel = "Fam Chat") }
        composeTestRule.onNodeWithTag("gated-post-badge").assertTextEquals("Room: Fam Chat")
    }

    @Test
    fun roomLabelAbsent_fallsBackToTheReservedTierText() {
        // The outsider's card: not on the room's floor, so `room_label` is null
        // and the reserved tier `room` is the honest degrade.
        composeTestRule.setContent { GatedPostBadge(tier = "room", roomLabel = null) }
        composeTestRule.onNodeWithTag("gated-post-badge").assertTextEquals("room")
    }
}
