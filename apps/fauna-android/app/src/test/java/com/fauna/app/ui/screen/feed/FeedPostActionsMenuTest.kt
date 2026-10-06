package com.fauna.app.ui.screen.feed

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_feed.TrainVerb

/**
 * Compose-level coverage for [PostActionsMenu] — the per-card ⋯ overflow that
 * hosts the trained-topic-factor training verbs (topic-factors.md § Authoring
 * surface, shown on EVERY post) and own-post delete (feed.md § State & data
 * shape → *Post deletion*, IDs user-approved 2026-07-16). It is a pure
 * Compose leaf taking plain `isOwn`/`markedVerb` values + `onTrainVerbTapped`/
 * `onDelete` lambdas — no FFI seam, no `PostSummary`, no `.so` (the `is_own`
 * gate and the `trainTargetFactor`/`example_label_for` reads are all resolved
 * one layer up in [PostCard]) — so it runs on the host JVM exactly like
 * [FeedInteractionBarTest].
 *
 * Verifies:
 *   1. the ⋯ button + training verbs render on EVERY post, own or not;
 *   2. delete is hidden on someone else's post, shown on the caller's own;
 *   3. tapping a verb invokes `onTrainVerbTapped` with that verb;
 *   4. the destructive delete is a two-step inside the same flyout
 *      (`feed-post-delete-button` reveals `feed-post-delete-confirm-button`);
 *   5. only the confirm step invokes `onDelete`.
 *   6. the own-post web-publishing verbs (`web-content-hosting.md` §
 *      Published-post management) — publish/copy/unpublish state-derived off
 *      `webSlug`/`gatedTier`/`webLinkOrigin`, never shown for someone else's
 *      post (mirrors linux's `build_web_publish_verbs`/web's `PostCard.svelte`
 *      precedent).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FeedPostActionsMenuTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        isOwn: Boolean,
        markedVerb: TrainVerb? = null,
        onTrainVerbTapped: (TrainVerb) -> Unit = {},
        onDelete: () -> Unit = {},
        webSlug: String? = null,
        gatedTier: String? = null,
        webLinkOrigin: String? = null,
        webLinkCopied: Triple<String, String, String>? = null,
        onPublishWeb: () -> Unit = {},
        onUnpublishWeb: () -> Unit = {},
        onCopyWebLink: () -> Unit = {},
        onCopyPaywallLink: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            PostActionsMenu(
                isOwn = isOwn,
                markedVerb = markedVerb,
                onTrainVerbTapped = onTrainVerbTapped,
                onDelete = onDelete,
                webSlug = webSlug,
                gatedTier = gatedTier,
                webLinkOrigin = webLinkOrigin,
                webLinkCopied = webLinkCopied,
                onPublishWeb = onPublishWeb,
                onUnpublishWeb = onUnpublishWeb,
                onCopyWebLink = onCopyWebLink,
                onCopyPaywallLink = onCopyPaywallLink,
            )
        }
    }

    @Test
    fun trainingVerbsShownOnEveryPost() {
        render(isOwn = false)
        composeTestRule.onNodeWithTag("feed-post-actions-button").assertExists()
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-more-like-this").assertExists()
        composeTestRule.onNodeWithTag("feed-post-less-like-this").assertExists()
        // Delete is NOT offered on someone else's post.
        composeTestRule.onNodeWithTag("feed-post-delete-button").assertDoesNotExist()
    }

    @Test
    fun deleteShownOnOwnPost() {
        render(isOwn = true)
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-more-like-this").assertExists()
        composeTestRule.onNodeWithTag("feed-post-less-like-this").assertExists()
        composeTestRule.onNodeWithTag("feed-post-delete-button").assertExists()
    }

    @Test
    fun tappingMoreLikeThisInvokesCallback() {
        var tapped: TrainVerb? = null
        render(isOwn = false, onTrainVerbTapped = { tapped = it })
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-more-like-this").performClick()
        assertEquals(TrainVerb.MORE_LIKE_THIS, tapped)
    }

    @Test
    fun tappingLessLikeThisInvokesCallback() {
        var tapped: TrainVerb? = null
        render(isOwn = false, onTrainVerbTapped = { tapped = it })
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-less-like-this").performClick()
        assertEquals(TrainVerb.LESS_LIKE_THIS, tapped)
    }

    @Test
    fun deleteIsTwoStep() {
        var deleted = false
        render(isOwn = true, onDelete = { deleted = true })
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        // First step: only the delete-button is shown, not the confirm.
        composeTestRule.onNodeWithTag("feed-post-delete-button").assertExists()
        composeTestRule.onNodeWithTag("feed-post-delete-confirm-button").assertDoesNotExist()
        // Tapping delete reveals the confirm and hides the first step; the
        // callback has NOT fired yet (a single tap must never destroy the post).
        composeTestRule.onNodeWithTag("feed-post-delete-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-delete-confirm-button").assertExists()
        composeTestRule.onAllNodesWithTag("feed-post-delete-button").assertCountEquals(0)
        assertFalse("delete must not fire on the first tap", deleted)
    }

    @Test
    fun confirmInvokesDelete() {
        var deleteCount = 0
        render(isOwn = true, onDelete = { deleteCount += 1 })
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-delete-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-delete-confirm-button").performClick()
        assertEquals("confirm should invoke onDelete exactly once", 1, deleteCount)
    }

    // ── Own-post web-publishing verbs ──────────────────────────────────────

    @Test
    fun webPublishVerbsAbsentOnSomeoneElsesPost() {
        // isOwn = false: the verbs must not appear regardless of webSlug —
        // an own-post-only affordance a viewer must never see, let alone tap.
        render(isOwn = false, webSlug = "my-page", webLinkOrigin = "https://alice.example.com")
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-publish-web-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feed-post-copy-web-link-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feed-post-unpublish-web-button").assertDoesNotExist()
    }

    @Test
    fun unpublishedOwnPostOffersOnlyPublish() {
        render(isOwn = true, webSlug = null)
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-publish-web-button").assertExists()
        composeTestRule.onNodeWithTag("feed-post-copy-web-link-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feed-post-unpublish-web-button").assertDoesNotExist()
    }

    @Test
    fun tappingPublishInvokesCallback() {
        var published = false
        render(isOwn = true, webSlug = null, onPublishWeb = { published = true })
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-publish-web-button").performClick()
        assertEquals("tapping publish must invoke onPublishWeb", true, published)
    }

    @Test
    fun publishedOwnPostWithOriginOffersLiveCopyAndUnpublish() {
        render(isOwn = true, webSlug = "my-page", webLinkOrigin = "https://alice.example.com")
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-publish-web-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feed-post-copy-web-link-button").assertExists().assertIsEnabled()
        composeTestRule.onNodeWithTag("feed-post-unpublish-web-button").assertExists()
        // Ungated: no paywall-link verb.
        composeTestRule.onNodeWithTag("feed-post-copy-paywall-link-button").assertDoesNotExist()
    }

    @Test
    fun publishedOwnPostWithNoOriginDisablesCopyButKeepsUnpublishLive() {
        // Legal but unreachable (web-content-hosting.md § Published-post
        // management): the verb stays PRESENT, disabled, with a reason — a
        // dead link on the clipboard is worse than none. Unpublish needs no
        // origin, so it stays live throughout.
        render(isOwn = true, webSlug = "my-page", webLinkOrigin = null)
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-copy-web-link-button").assertExists().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("feed-post-unpublish-web-button").assertExists()
    }

    @Test
    fun gatedPublishedPostOffersPaywallLink() {
        render(isOwn = true, webSlug = "my-page", gatedTier = "supporter", webLinkOrigin = "https://alice.example.com")
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-copy-paywall-link-button").assertExists().assertIsEnabled()
    }

    @Test
    fun tappingCopyWebLinkInvokesCallback() {
        var copied = false
        render(
            isOwn = true,
            webSlug = "my-page",
            webLinkOrigin = "https://alice.example.com",
            onCopyWebLink = { copied = true },
        )
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-copy-web-link-button").performClick()
        assertEquals("tapping copy web link must invoke onCopyWebLink", true, copied)
    }

    @Test
    fun tappingCopyPaywallLinkInvokesCallback() {
        var copied = false
        render(
            isOwn = true,
            webSlug = "my-page",
            gatedTier = "supporter",
            webLinkOrigin = "https://alice.example.com",
            onCopyPaywallLink = { copied = true },
        )
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-copy-paywall-link-button").performClick()
        assertEquals("tapping copy paywall link must invoke onCopyPaywallLink", true, copied)
    }

    @Test
    fun tappingUnpublishInvokesCallback() {
        var unpublished = false
        render(
            isOwn = true,
            webSlug = "my-page",
            webLinkOrigin = "https://alice.example.com",
            onUnpublishWeb = { unpublished = true },
        )
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-unpublish-web-button").performClick()
        assertEquals("tapping unpublish must invoke onUnpublishWeb", true, unpublished)
    }

    @Test
    fun copiedLinkPaintsBackTheConfirmation() {
        render(
            isOwn = true,
            webSlug = "my-page",
            webLinkOrigin = "https://alice.example.com",
            webLinkCopied = Triple("deadbeef", "web", "https://alice.example.com/post/my-page.html"),
        )
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithText("https://alice.example.com/post/my-page.html", substring = true)
            .assertExists()
    }
}
