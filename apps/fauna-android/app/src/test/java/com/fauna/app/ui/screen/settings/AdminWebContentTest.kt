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
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility

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
        apexInfo: LocalizedText? = LocalizedText("admin.web_page.apex_info", mapOf("url" to "https://example.com/")),
        pickerAnswer: Possibility = Possibility.Possible,
        onSetApex: (String?) -> Unit = {},
    ) {
        composeTestRule.setContent {
            AdminWebContent(
                currentActorHex = currentActorHex,
                actors = actors,
                apexInfo = apexInfo,
                pickerAnswer = pickerAnswer,
                onBack = {},
                onSetApex = onSetApex,
            )
        }
    }

    @Test
    fun rendersHeadingNavBackPickerAndInfo() {
        render()
        composeTestRule.onNodeWithTag("admin-web-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        composeTestRule.onNodeWithTag("admin-web-apex-actor-select").assertExists()
        composeTestRule.onNodeWithTag("admin-web-apex-info")
            .assertTextContains("https://example.com/", substring = true)
    }

    /** A nest that serves no web content paints the reason, never `https:///`. */
    @Test
    fun infoLineExplainsANestWithNoServingDomain() {
        render(apexInfo = LocalizedText("admin.web_page.apex_no_serving_domain", emptyMap()))
        composeTestRule.onNodeWithTag("admin-web-apex-info")
            .assertTextContains("no web address yet", substring = true)
    }

    /** Rule 5: before the read lands the picker is disabled with its reason. */
    @Test
    fun pickerIsBlockedBeforeItsRead() {
        render(pickerAnswer = Possibility.Blocked(LocalizedText("common.still_loading", emptyMap())))
        composeTestRule.onNodeWithTag("admin-web-apex-actor-select")
            .performScrollTo().assertIsNotEnabled()
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
