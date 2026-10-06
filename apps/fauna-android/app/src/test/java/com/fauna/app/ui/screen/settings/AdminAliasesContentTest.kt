package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.ForwarderView

/**
 * Compose-level coverage for the stateless [AdminAliasesContent] (the admin
 * `admin-aliases` page, admin.md § 4 / mail-aliases.md § Kind 7): the add form,
 * the dedicated action-error element, and the indexed forwarder list with its
 * per-row delete. Renders with seeded state — no Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminAliasesContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun forwarder(
        id: String = "aa",
        domain: String = "example.com",
        pattern: String = "info",
        target: String = "ops@external.com",
    ) = ForwarderView(
        aliasIdHex = id,
        localDomain = domain,
        pattern = pattern,
        address = "$pattern@$domain",
        forwardTarget = target,
    )

    private fun render(
        forwarders: List<ForwarderView> = emptyList(),
        localDomains: List<String> = listOf("example.com"),
        actionError: String? = null,
        working: Boolean = false,
        onCreate: (String, String, String) -> Unit = { _, _, _ -> },
        onDelete: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            AdminAliasesContent(
                forwarders = forwarders,
                localDomains = localDomains,
                actionError = actionError,
                working = working,
                onBack = {},
                onCreate = onCreate,
                onDelete = onDelete,
            )
        }
    }

    @Test
    fun rendersHeadingAndAddForm() {
        render()
        composeTestRule.onNodeWithTag("admin-aliases-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        composeTestRule.onNodeWithTag("admin-aliases-forwarders-section").assertExists()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-domain-select").assertExists()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-pattern-input").assertExists()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-target-input").assertExists()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-submit-button").assertExists()
    }

    @Test
    fun addControlsDisabledWithoutDomains() {
        render(localDomains = emptyList())
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-submit-button").assertIsNotEnabled()
    }

    @Test
    fun forwarderRowsRenderWithAddressAndTarget() {
        render(
            forwarders = listOf(
                forwarder(id = "aa", pattern = "info"),
                forwarder(id = "bb", pattern = "sales", target = "sales@external.com"),
            ),
        )
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-aliases-forwarder-list").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-aliases-forwarder-row-address").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-aliases-forwarder-row-target").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-aliases-forwarder-row-delete-button").fetchSemanticsNodes().size)
    }

    @Test
    fun createFiresWithTrimmedFields() {
        var created: List<String>? = null
        render(onCreate = { d, p, t -> created = listOf(d, p, t) })
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-pattern-input").performTextInput("info")
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-target-input").performTextInput("ops@external.com")
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-submit-button").performScrollTo().performClick()
        assertEquals(listOf("example.com", "info", "ops@external.com"), created)
    }

    @Test
    fun deleteFiresWithAliasId() {
        var deleted: String? = null
        render(forwarders = listOf(forwarder(id = "deadbeef")), onDelete = { deleted = it })
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-row-delete-button").performScrollTo().performClick()
        assertEquals("deadbeef", deleted)
    }

    @Test
    fun actionErrorRenders() {
        render(actionError = "reserved_local_part")
        composeTestRule.onNodeWithTag("admin-aliases-action-error").assertTextEquals("reserved_local_part")
    }
}
