package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.ui.viewmodel.ActorOption
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [AdminWebContent] (the admin
 * `admin-web` page, web-content-hosting.md § Admin apex hosting): the apex-actor
 * picker ("none" clears → info page) + the apex-URL explainer. Renders with seeded
 * state — no Hilt, no VM, no FFI native calls. The picker is the catch-all picker's
 * twin, so it exercises the same expand → select → callback path.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminWebContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        currentActorHex: String? = null,
        actors: List<ActorOption> = emptyList(),
        apexUrl: String = "https://example.com/",
        onSetApex: (String?) -> Unit = {},
    ) {
        composeTestRule.setContent {
            AdminWebContent(
                currentActorHex = currentActorHex,
                actors = actors,
                apexUrl = apexUrl,
                onBack = {},
                onSetApex = onSetApex,
            )
        }
    }

    @Test
    fun rendersHeadingNavBackPickerAndInfo() {
        render(apexUrl = "https://example.com/")
        composeTestRule.onNodeWithTag("admin-web-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        composeTestRule.onNodeWithTag("admin-web-apex-actor-select").assertExists()
        composeTestRule.onNodeWithTag("admin-web-apex-info")
            .assertTextContains("https://example.com/", substring = true)
    }

    @Test
    fun pickerReflectsCurrentDesignation() {
        render(currentActorHex = "aabb", actors = listOf(ActorOption(idHex = "aabb", label = "Alice")))
        composeTestRule.onNodeWithTag("admin-web-apex-actor-select")
            .assertTextContains("Alice", substring = true)
    }

    @Test
    fun selectingAnActorDesignatesIt() {
        var captured: String? = null
        render(
            actors = listOf(ActorOption(idHex = "aabb", label = "Alice")),
            onSetApex = { captured = it },
        )
        composeTestRule.onNodeWithTag("admin-web-apex-actor-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Alice").performClick()
        assertEquals("aabb", captured)
    }

    @Test
    fun selectingNoneClearsToInfoPage() {
        var called = false
        var value: String? = "sentinel"
        render(
            currentActorHex = "aabb",
            actors = listOf(ActorOption(idHex = "aabb", label = "Alice")),
            onSetApex = { called = true; value = it },
        )
        composeTestRule.onNodeWithTag("admin-web-apex-actor-select").performScrollTo().performClick()
        // Index 0 = "None (info page)" (admin_web_page_apex_none).
        composeTestRule.onNodeWithText("None (info page)").performClick()
        assertTrue(called)
        assertNull(value)
    }
}
