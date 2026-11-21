package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.R
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.ForwarderView
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility

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
        formPossibility: Possibility = Possibility.Possible,
        createPossibility: (String) -> Possibility = { Possibility.Possible },
        onOpenDomains: () -> Unit = {},
        createDraftPossibility: (String, String, String) -> Possibility = AdminDraftFakes.forwarderCreate,
        forwardersRead: Possibility = Possibility.Possible,
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
                formPossibility = formPossibility,
                createPossibility = createPossibility,
                onOpenDomains = onOpenDomains,
                createDraftPossibility = createDraftPossibility,
                forwardersRead = forwardersRead,
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

    private val noHostedDomain = Possibility.Blocked(
        LocalizedText(key = "admin.aliases_page.no_hosted_domain", args = emptyMap()),
    )

    /** `admin.md` § 4, *A forwarder needs a hosted domain*: the shared answer
     *  blocks the form and the submit, and the Domains step is offered. */
    @Test
    fun addControlsDisabledWithoutDomains() {
        var opened = 0
        render(
            localDomains = emptyList(),
            formPossibility = noHostedDomain,
            createPossibility = { noHostedDomain },
            onOpenDomains = { opened++ },
        )
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-submit-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-pattern-input").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-domains-step")
            .performScrollTo()
            .assertIsEnabled()
            .performClick()
        composeTestRule.waitForIdle()
        assertEquals(1, opened)
    }

    /** A hosted domain: the form is live and the step is not offered. */
    @Test
    fun aHostedDomainDropsTheDomainsStep() {
        render()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-domains-step").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-pattern-input").assertIsEnabled()
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

    // ── Draft validity (admin.md § Errors & edge cases, the form bullet) ──

    private fun string(id: Int) = RuntimeEnvironment.getApplication().getString(id)

    @Test
    fun anIncompleteDraftBlocksTheSubmitWithTheSharedReason() {
        var created = false
        render(onCreate = { _, _, _ -> created = true })
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-pattern-input").performTextInput("info")
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-submit-button").performScrollTo()
            .assertIsNotEnabled().performClick()
        composeTestRule.onNodeWithText(string(R.string.admin_aliases_page_forwarder_draft_incomplete)).assertExists()
        assertEquals(false, created)
        // The draft stays and the inputs stay editable.
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-pattern-input").assertTextContains("info")
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-target-input").assertIsEnabled()
    }

    @Test
    fun theReadAnswerSpeaksBeforeTheDraftAnswer() {
        render(localDomains = emptyList(), formPossibility = noHostedDomain, createPossibility = { noHostedDomain })
        composeTestRule.onNodeWithText(string(R.string.admin_aliases_page_forwarder_draft_incomplete)).assertDoesNotExist()
        composeTestRule.onNodeWithText(string(R.string.admin_aliases_page_no_hosted_domain)).assertExists()
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


    // ── Controls wait for their read (admin.md § Errors & edge cases) ──

    private val noForwarders get() =
        org.robolectric.RuntimeEnvironment.getApplication().getString(com.fauna.app.R.string.admin_aliases_page_no_forwarders)

    @Test
    fun addFormWaitsForTheForwarderRead() {
        var created = false
        // The shared form/create answers fold `add_possibility` first, so a
        // page still loading answers still loading for both.
        render(
            formPossibility = ReadGateFakes.stillLoading,
            createPossibility = { ReadGateFakes.stillLoading },
            onCreate = { _, _, _ -> created = true },
            forwardersRead = ReadGateFakes.stillLoading,
        )
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-pattern-input")
            .assertIsNotEnabled()
            .assertPaintedReason(ReadGateFakes.stillLoadingText)
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-domain-select")
            .assertPaintedReason(ReadGateFakes.stillLoadingText)
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-submit-button")
            .assertIsNotEnabled()
            .assertPaintedReason(ReadGateFakes.stillLoadingText)
            .performClick()
        assertEquals(false, created)
        // An unread list is not an empty one.
        composeTestRule.onNodeWithText(noForwarders).assertDoesNotExist()
    }

    @Test
    fun failedListReadSaysSoNotEmpty() {
        render(forwardersRead = ReadGateFakes.loadFailed)
        composeTestRule.onNodeWithText(noForwarders).assertDoesNotExist()
        composeTestRule.onNodeWithText(ReadGateFakes.loadFailedText).assertExists()
    }

    @Test
    fun readEmptyListPaintsTheEmptyText() {
        render(forwardersRead = Possibility.Possible)
        composeTestRule.onNodeWithText(noForwarders).assertExists()
    }
}
