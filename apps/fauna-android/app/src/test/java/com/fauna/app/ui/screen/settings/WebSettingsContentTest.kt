package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.ffi.FfiPublishedPost
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_web.SiteLinkDisabledReason
import uniffi.fauna_client_web.SubdomainDisabledReason
import uniffi.fauna_client_web.SubdomainView

/**
 * Compose-level coverage for the stateless [WebSettingsContent] (the user
 * `web-settings` page, web-content-hosting.md § Published-post management): the
 * subdomain opt-in toggle + the live URL / disabled-reason row + the explainer,
 * plus the Published-posts management section —
 * list/copy-link/copy-paywall-link/unpublish per row. Renders with a seeded
 * [SubdomainView] / post list — no Hilt, no VM. The `origin == null` /
 * `disabledReason` rows DO make a real FFI call (the shared
 * `webDisabledReasonText()` key-mapping door); `just android-host-test`'s
 * host-JNA wiring makes it work under Robolectric, same as
 * `FoldersContentTest`'s `conflictPolicyOptions()`/`residencyOptions()`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class WebSettingsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        view: SubdomainView,
        onSetEnabled: (Boolean) -> Unit = {},
        posts: List<FfiPublishedPost> = emptyList(),
        hydrated: Boolean = false,
        renderedPagesDown: Boolean = false,
        origin: String? = null,
        disabledReason: SiteLinkDisabledReason? = null,
        copied: Triple<String, String, String>? = null,
        onCopyWebLink: (FfiPublishedPost) -> Unit = {},
        onCopyPaywallLink: (FfiPublishedPost) -> Unit = {},
        onUnpublish: (FfiPublishedPost) -> Unit = {},
    ) {
        composeTestRule.setContent {
            WebSettingsContent(
                view = view,
                onBack = {},
                onSetEnabled = onSetEnabled,
                posts = posts,
                hydrated = hydrated,
                renderedPagesDown = renderedPagesDown,
                origin = origin,
                disabledReason = disabledReason,
                copied = copied,
                onCopyWebLink = onCopyWebLink,
                onCopyPaywallLink = onCopyPaywallLink,
                onUnpublish = onUnpublish,
            )
        }
    }

    private fun post(slug: String, gatedTier: String? = null, idByte: Byte = 1) =
        FfiPublishedPost(postId = ByteArray(32) { idByte }, slug = slug, gatedTier = gatedTier)

    @Test
    fun rendersHeadingToggleUrlAndInfo() {
        render(SubdomainView(enabled = false, url = "https://alice.example.com/", disabledReason = null))
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("web-settings-subdomain-toggle").assertExists()
        composeTestRule.onNodeWithTag("web-settings-subdomain-url").assertExists()
        composeTestRule.onNodeWithTag("web-settings-content-info").assertExists()
    }

    /** The blanked-site status line paints only while the pages are down —
     *  information only, so no button rides with it. */
    @Test
    fun renderStatusPaintsOnlyWhileTheRenderedPagesAreDown() {
        val view = SubdomainView(enabled = true, url = "https://alice.example.com/", disabledReason = null)
        render(view, hydrated = true, renderedPagesDown = false)
        composeTestRule.onNodeWithTag("web-settings-render-status").assertDoesNotExist()
    }

    @Test
    fun renderStatusShowsWhenTheRenderedPagesAreDown() {
        val view = SubdomainView(enabled = true, url = "https://alice.example.com/", disabledReason = null)
        render(view, hydrated = true, renderedPagesDown = true)
        composeTestRule.onNodeWithTag("web-settings-render-status").assertExists()
    }

    @Test
    fun urlRowShowsLiveUrlWhenServing() {
        render(SubdomainView(enabled = true, url = "https://alice.example.com/", disabledReason = null))
        composeTestRule.onNodeWithTag("web-settings-subdomain-url")
            .assertTextContains("https://alice.example.com/", substring = true)
    }

    @Test
    fun urlRowShowsDisabledReasonWhenNoHandle() {
        render(SubdomainView(enabled = false, url = null, disabledReason = SubdomainDisabledReason.NO_HANDLE))
        // The shared projection yields no URL; the row renders the reason copy
        // (the `no_handle` string contains "handle"), not an empty line.
        composeTestRule.onNodeWithTag("web-settings-subdomain-url")
            .assertTextContains("handle", substring = true)
    }

    @Test
    fun togglingFlipsTheOptIn() {
        var captured: Boolean? = null
        render(
            SubdomainView(enabled = false, url = null, disabledReason = null),
            onSetEnabled = { captured = it },
        )
        composeTestRule.onNodeWithTag("web-settings-subdomain-toggle").performClick()
        assertEquals(true, captured)
    }

    // ── Published-posts management section ─────────────────────────────────

    private val view = SubdomainView(enabled = true, url = "https://alice.example.com/", disabledReason = null)

    @Test
    fun sectionAbsentBeforeHydration() {
        // A pre-read frame must not claim "no published posts" about a list
        // nobody asked for — the section is entirely absent, not an empty state.
        render(view, hydrated = false)
        composeTestRule.onNodeWithTag("web-published-posts-empty").assertDoesNotExist()
        composeTestRule.onNodeWithTag("web-published-posts-list").assertDoesNotExist()
    }

    @Test
    fun emptyStateShownWhenHydratedWithNoPosts() {
        render(view, hydrated = true, posts = emptyList())
        composeTestRule.onNodeWithTag("web-published-posts-empty").assertExists()
        composeTestRule.onNodeWithTag("web-published-posts-list").assertDoesNotExist()
    }

    @Test
    fun publishedRowsListedWithLiveCopyLink() {
        render(view, hydrated = true, posts = listOf(post("my-first-page")), origin = "https://alice.example.com")
        composeTestRule.onNodeWithTag("web-published-posts-list").assertExists()
        composeTestRule.onNodeWithTag("web-published-post-slug").assertTextEquals("my-first-page")
        composeTestRule.onNodeWithTag("web-published-post-copy-link-button").assertIsEnabled()
        // Ungated: no paywall-link verb.
        composeTestRule.onNodeWithTag("web-published-post-copy-paywall-link-button").assertDoesNotExist()
    }

    @Test
    fun copyLinkDisabledWithNoOrigin() {
        // Legal but unreachable: the verb stays PRESENT, disabled, with a
        // reason line — never hidden, never a dead link handed out.
        render(
            view,
            hydrated = true,
            posts = listOf(post("my-first-page")),
            origin = null,
            disabledReason = SiteLinkDisabledReason.SUBDOMAIN_DISABLED,
        )
        composeTestRule.onNodeWithTag("web-published-post-copy-link-button").assertIsNotEnabled()
    }

    @Test
    fun gatedRowOffersPaywallLink() {
        render(
            view,
            hydrated = true,
            posts = listOf(post("supporters-only", gatedTier = "supporter")),
            origin = "https://alice.example.com",
        )
        composeTestRule.onNodeWithTag("web-published-post-copy-paywall-link-button").assertIsEnabled()
    }

    @Test
    fun tappingCopyWebLinkInvokesCallbackWithTheRow() {
        var copiedPost: FfiPublishedPost? = null
        val row = post("my-first-page")
        render(
            view,
            hydrated = true,
            posts = listOf(row),
            origin = "https://alice.example.com",
            onCopyWebLink = { copiedPost = it },
        )
        composeTestRule.onNodeWithTag("web-published-post-copy-link-button").performClick()
        assertEquals("copy web link must be called with the tapped row", row, copiedPost)
    }

    @Test
    fun tappingCopyPaywallLinkInvokesCallbackWithTheRow() {
        var copiedPost: FfiPublishedPost? = null
        val row = post("supporters-only", gatedTier = "supporter")
        render(
            view,
            hydrated = true,
            posts = listOf(row),
            origin = "https://alice.example.com",
            onCopyPaywallLink = { copiedPost = it },
        )
        composeTestRule.onNodeWithTag("web-published-post-copy-paywall-link-button").performClick()
        assertEquals("copy paywall link must be called with the tapped row", row, copiedPost)
    }

    @Test
    fun tappingUnpublishInvokesCallbackWithTheRow() {
        var unpublishedPost: FfiPublishedPost? = null
        val row = post("my-first-page")
        render(
            view,
            hydrated = true,
            posts = listOf(row),
            origin = "https://alice.example.com",
            onUnpublish = { unpublishedPost = it },
        )
        composeTestRule.onNodeWithTag("web-published-post-unpublish-button").performClick()
        assertEquals("unpublish must be called with the tapped row", row, unpublishedPost)
    }

    @Test
    fun copiedLinkPaintsBackUnderTheMatchingRowOnly() {
        val matching = post("my-first-page", idByte = 1)
        val other = post("someone-elses-page", idByte = 2)
        val matchingHex = com.fauna.app.core.HexUtil.bytesToHex(matching.postId)
        render(
            view,
            hydrated = true,
            posts = listOf(matching, other),
            origin = "https://alice.example.com",
            copied = Triple(matchingHex, "web", "https://alice.example.com/post/my-first-page.html"),
        )
        composeTestRule.onNodeWithText("https://alice.example.com/post/my-first-page.html", substring = true)
            .assertExists()
    }
}
