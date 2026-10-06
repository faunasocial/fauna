package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.util.CaptureGuard
import com.fauna.app.ui.util.LocalCaptureGuard
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_atproto_settings_machine.ConsentCardRow
import uniffi.fauna_client_connected_apps.BlockedAppRow
import uniffi.fauna_client_connected_apps.ConnectedAppRow
import uniffi.fauna_client_connected_apps.ConnectedAppsSnapshot
import uniffi.fauna_client_connected_apps.MailAppPassword
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the stateless [ConnectedAppsContent] (the
 * "Connected apps" settings page, `docs/goal/ui/connected-apps.md`): the four
 * regions (Requests tray, Connect an app, the roster, Blocked apps), the
 * three-state roster list, a mail app-password row's own leaves, the
 * hidden-secret-stays-empty rule, and the burned row's `revoked` attr. Renders
 * with hand-built snapshot fixtures — no Hilt, no VM, no machine
 * (`resolveUsername` / `formatTime` are injected stubs, the `TaskDelegationContentTest`
 * FFI-free-injection pattern). The cross-app `test_connected_apps.py` is the
 * standing gate once the host emulator lands.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ConnectedAppsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun verbatim(text: String) = LocalizedText("connected_apps.verbatim", mapOf("text" to text))

    private fun row(
        key: String,
        name: String = "An app",
        cls: String = "device",
        clientId: String? = null,
        publisher: String? = null,
        scopes: List<String> = emptyList(),
        connected: Boolean = true,
        mail: MailAppPassword? = null,
    ) = ConnectedAppRow(
        key = key,
        `class` = cls,
        name = verbatim(name),
        clientId = clientId,
        publisher = publisher,
        scopeDescriptions = scopes.map { verbatim(it) },
        createdAtMillis = 1_700_000_000_000L,
        lastUsedAtMillis = null,
        lastsUntilMillis = null,
        connected = connected,
        mail = mail,
    )

    private fun mailRow(key: String = "mail:c1", revoked: Boolean = false) = row(
        key = key,
        name = "iPhone Mail",
        cls = "app_password",
        mail = MailAppPassword(
            muaUsername = "{handle}+c1@example.com",
            kind = LocalizedText("settings.mail.kind_password", emptyMap()),
            revoked = revoked,
        ),
    )

    private fun request(id: String = "c1", code: String = "AAA-BBB") = ConsentCardRow(
        consentIdHex = id,
        code = code,
        clientId = "https://example.com/oauth",
        clientName = "Example App",
        scopeDescriptions = listOf("Read your posts"),
        sets = emptyList(),
    )

    private fun snapshot(
        loaded: Boolean = true,
        requests: List<ConsentCardRow> = emptyList(),
        principals: List<ConnectedAppRow> = emptyList(),
        blocked: List<BlockedAppRow> = emptyList(),
    ) = ConnectedAppsSnapshot(
        loaded = loaded,
        requests = requests,
        principals = principals,
        blocked = blocked,
        error = null,
    )

    private fun render(
        snapshot: ConnectedAppsSnapshot? = snapshot(),
        code: String = "",
        revokeArmed: String? = null,
        revealed: Map<String, String> = emptyMap(),
        onSubmitCode: () -> Unit = {},
        onResolveRequest: (String, Boolean) -> Unit = { _, _ -> },
        onBlockRequest: (String) -> Unit = {},
        onUnblock: (String) -> Unit = {},
        onArmRevoke: (String) -> Unit = {},
        onConfirmRevoke: (String) -> Unit = {},
        onCancelRevoke: () -> Unit = {},
        onToggleReveal: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            ConnectedAppsContent(
                snapshot = snapshot,
                code = code,
                revokeArmed = revokeArmed,
                revealed = revealed,
                onBack = {},
                onCodeChange = {},
                onSubmitCode = onSubmitCode,
                onResolveRequest = onResolveRequest,
                onBlockRequest = onBlockRequest,
                onUnblock = onUnblock,
                onArmRevoke = onArmRevoke,
                onConfirmRevoke = onConfirmRevoke,
                onCancelRevoke = onCancelRevoke,
                onToggleReveal = onToggleReveal,
                readSecret = { null },
                resolveUsername = { it.replace("{handle}", "alice") },
                formatTime = { "2023-11-14 22:13" },
            )
        }
    }

    // ── Chrome and the three-state list ───────────────────────────────────

    @Test
    fun rendersPageChromeAndTheConnectStart() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("connected-apps").assertExists()
        composeTestRule.onNodeWithTag("connected-apps-connect-code").assertExists()
        composeTestRule.onNodeWithTag("connected-apps-connect-submit").assertExists()
    }

    @Test
    fun aVisitPaintsNeitherRowsNorTheEmptyStateUntilItsReadReturned() {
        render(snapshot = snapshot(loaded = false, principals = listOf(row("a"))))
        composeTestRule.onNodeWithTag("connected-apps-item").assertDoesNotExist()
        composeTestRule.onNodeWithTag("connected-apps-empty").assertDoesNotExist()
    }

    @Test
    fun anUnbuiltMachinePaintsNeitherRowsNorTheEmptyState() {
        render(snapshot = null)
        composeTestRule.onNodeWithTag("connected-apps-item").assertDoesNotExist()
        composeTestRule.onNodeWithTag("connected-apps-empty").assertDoesNotExist()
    }

    @Test
    fun aLoadedEmptyRosterPaintsTheEmptyState() {
        render(snapshot = snapshot(loaded = true))
        composeTestRule.onNodeWithTag("connected-apps-empty").assertExists()
        composeTestRule.onNodeWithTag("connected-apps-item").assertDoesNotExist()
    }

    // ── Connect an app ────────────────────────────────────────────────────

    @Test
    fun connectSubmitIsDisabledWithoutACode() {
        render(code = "")
        composeTestRule.onNodeWithTag("connected-apps-connect-submit").assertIsNotEnabled()
    }

    @Test
    fun connectSubmitFiresOnceACodeIsTyped() {
        var submitted = 0
        render(code = "ABCD", onSubmitCode = { submitted++ })
        composeTestRule.onNodeWithTag("connected-apps-connect-submit").assertIsEnabled().performClick()
        assertEquals(1, submitted)
    }

    // ── Requests tray ─────────────────────────────────────────────────────

    @Test
    fun theTrayIsAbsentWhenNoRequestIsLive() {
        render(snapshot = snapshot(requests = emptyList()))
        composeTestRule.onNodeWithTag("connected-apps-request-card").assertDoesNotExist()
    }

    @Test
    fun oneCardPerLiveRequestEachCarryingItsOwnCodeAndControls() {
        render(snapshot = snapshot(requests = listOf(request("c1", "AAA-BBB"), request("c2", "CCC-DDD"))))
        composeTestRule.onAllNodesWithTag("connected-apps-request-card").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("connected-apps-request-approve").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("connected-apps-request-decline").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("connected-apps-request-block").assertCountEquals(2)
        // The controls are CHILDREN of their own card — the e2e reads them
        // `scope="connected-apps-request-card[i]"`.
        composeTestRule.onAllNodes(
            hasTestTag("connected-apps-request-card") and
                hasAnyDescendant(hasTestTag("connected-apps-request-code")) and
                hasAnyDescendant(hasTestTag("connected-apps-request-approve")) and
                hasAnyDescendant(hasTestTag("connected-apps-request-decline")) and
                hasAnyDescendant(hasTestTag("connected-apps-request-block")),
        ).assertCountEquals(2)
    }

    @Test
    fun theBindingCodeValueRidesTheStateDescriptionAttr() {
        render(snapshot = snapshot(requests = listOf(request("c1", "AAA-BBB"))))
        composeTestRule.onNodeWithTag("connected-apps-request-code")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "AAA-BBB"))
            .assertTextContains("AAA-BBB", substring = true)
    }

    @Test
    fun theCardNamesWhoIsAskingAndWhatItWants() {
        render(snapshot = snapshot(requests = listOf(request())))
        composeTestRule.onNodeWithText("Example App", substring = true).assertExists()
        composeTestRule.onNodeWithText("https://example.com/oauth", substring = true).assertExists()
        composeTestRule.onNodeWithText("Read your posts", substring = true).assertExists()
    }

    @Test
    fun approveDeclineAndBlockForwardTheConsentId() {
        val seen = mutableListOf<String>()
        render(
            snapshot = snapshot(requests = listOf(request("c9"))),
            onResolveRequest = { id, approved -> seen += "$id:$approved" },
            onBlockRequest = { seen += "block:$it" },
        )
        composeTestRule.onNodeWithTag("connected-apps-request-approve").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("connected-apps-request-decline").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("connected-apps-request-block").performScrollTo().performClick()
        assertEquals(listOf("c9:true", "c9:false", "block:c9"), seen)
    }

    // ── The roster ────────────────────────────────────────────────────────

    @Test
    fun aRowCarriesItsNameBadgeScopesAndFactsAsItsOwnText() {
        render(
            snapshot = snapshot(
                principals = listOf(
                    row(
                        "a",
                        name = "Graysky",
                        cls = "device",
                        clientId = "https://graysky.example/client",
                        publisher = "graysky.example",
                        scopes = listOf("Post as you"),
                    ),
                ),
            ),
        )
        val text = composeTestRule.onNodeWithTag("connected-apps-item")
            .fetchSemanticsNode().config[SemanticsProperties.Text].joinToString("") { it.text }
        assert(text.lines().first().startsWith("Graysky · ")) { text }
        assert("graysky.example" in text && "https://graysky.example/client" in text) { text }
        assert("  • Post as you" in text) { text }
    }

    @Test
    fun anOrdinaryRowHasNoMailLeaves() {
        render(snapshot = snapshot(principals = listOf(row("a"))))
        composeTestRule.onNodeWithTag("connected-apps-item-username").assertDoesNotExist()
        composeTestRule.onNodeWithTag("connected-apps-item-secret").assertDoesNotExist()
    }

    @Test
    fun revokeArmsTheRowByItsKey() {
        val seen = mutableListOf<String>()
        render(
            snapshot = snapshot(principals = listOf(row("k1"))),
            onArmRevoke = { seen += "arm:$it" },
        )
        composeTestRule.onNodeWithTag("connected-apps-item-revoke").performScrollTo().performClick()
        assertEquals(listOf("arm:k1"), seen)
    }

    @Test
    fun confirmAndCancelForwardTheArmedRow() {
        val confirmed = mutableListOf<String>()
        var cancelled = 0
        render(
            snapshot = snapshot(principals = listOf(row("k1"))),
            revokeArmed = "k1",
            onConfirmRevoke = { confirmed += it },
            onCancelRevoke = { cancelled++ },
        )
        composeTestRule.onNodeWithTag("connected-apps-item-revoke-confirm").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("connected-apps-item-revoke-cancel").performScrollTo().performClick()
        assertEquals(listOf("k1"), confirmed)
        assertEquals(1, cancelled)
    }

    @Test
    fun anArmedRowShowsTheConfirmPairInsteadOfRevoke() {
        render(snapshot = snapshot(principals = listOf(row("k1"))), revokeArmed = "k1")
        composeTestRule.onNodeWithTag("connected-apps-item-revoke").assertDoesNotExist()
        composeTestRule.onNodeWithTag("connected-apps-item-revoke-confirm").assertExists()
        composeTestRule.onNodeWithTag("connected-apps-item-revoke-cancel").assertExists()
    }

    // ── A mail app password's own leaves ──────────────────────────────────

    @Test
    fun aMailRowCarriesItsKindLoginAndSecretControls() {
        render(snapshot = snapshot(principals = listOf(mailRow())))
        composeTestRule.onNodeWithTag("connected-apps-item-type").assertExists()
        composeTestRule.onNodeWithTag("connected-apps-item-username").assertTextEquals("alice+c1@example.com")
        composeTestRule.onNodeWithTag("connected-apps-item-copy-username").assertExists()
        composeTestRule.onNodeWithTag("connected-apps-item-reveal-secret").assertExists()
        composeTestRule.onNodeWithTag("connected-apps-item-copy-secret").assertExists()
    }

    /** ⚠ The hidden secret's text MUST stay EMPTY — never a mask: the cross-app
     *  driver polls it until it turns non-empty and returns that AS the secret. */
    @Test
    fun theHiddenSecretLeafIsPresentAndEmptyUntilRevealed() {
        render(snapshot = snapshot(principals = listOf(mailRow())))
        composeTestRule.onNodeWithTag("connected-apps-item-secret").assertExists().assertTextEquals("")
    }

    @Test
    fun aRevealedSecretFillsTheLeafAndKeysByRowKey() {
        render(
            snapshot = snapshot(principals = listOf(mailRow("mail:c1"), mailRow("mail:c2"))),
            revealed = mapOf("mail:c2" to "pw-two"),
        )
        composeTestRule.onAllNodesWithTag("connected-apps-item-secret")[0].assertTextEquals("")
        composeTestRule.onAllNodesWithTag("connected-apps-item-secret")[1].assertTextEquals("pw-two")
    }

    @Test
    fun revealForwardsTheRowKey() {
        var toggled: String? = null
        render(
            snapshot = snapshot(principals = listOf(mailRow("mail:c1"))),
            onToggleReveal = { toggled = it },
        )
        composeTestRule.onNodeWithTag("connected-apps-item-reveal-secret").performScrollTo().performClick()
        assertEquals("mail:c1", toggled)
    }

    @Test
    fun aBurnedRowSaysSoUnderTheNameAndCarriesTheRevokedAttr() {
        render(snapshot = snapshot(principals = listOf(mailRow(revoked = true))))
        composeTestRule.onNodeWithTag("connected-apps-item")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "true"))
        // Words sit directly under the name: on the item's own text and as a Text of their own.
        assert(composeTestRule.onAllNodesWithText("Access revoked", substring = true).fetchSemanticsNodes().isNotEmpty())
    }

    @Test
    fun aLiveRowCarriesNoRevokedAttr() {
        render(snapshot = snapshot(principals = listOf(mailRow(revoked = false))))
        composeTestRule.onNodeWithTag("connected-apps-item")
            .assert(SemanticsMatcher.keyNotDefined(SemanticsProperties.StateDescription))
    }

    /** security.md § On-screen secret exposure, rule 2: a minted, revocable
     *  secret suppresses capture for exactly the reveal window — held while any
     *  secret is revealed, released when none is. */
    @Test
    fun aRevealedSecretSuppressesCaptureForExactlyTheRevealWindow() {
        class Guard : CaptureGuard {
            var held = 0
            override fun acquire() { held++ }
            override fun release() { held-- }
        }
        val guard = Guard()
        var revealed by androidx.compose.runtime.mutableStateOf<Map<String, String>>(emptyMap())
        composeTestRule.setContent {
            CompositionLocalProvider(LocalCaptureGuard provides guard) {
                ConnectedAppsContent(
                    snapshot = snapshot(principals = listOf(mailRow())),
                    code = "",
                    revokeArmed = null,
                    revealed = revealed,
                    onBack = {},
                    onCodeChange = {},
                    onSubmitCode = {},
                    onResolveRequest = { _, _ -> },
                    onBlockRequest = {},
                    onUnblock = {},
                    onArmRevoke = {},
                    onConfirmRevoke = {},
                    onCancelRevoke = {},
                    onToggleReveal = {},
                    readSecret = { null },
                    resolveUsername = { it },
                    formatTime = { "" },
                )
            }
        }
        composeTestRule.runOnIdle { assertEquals("nothing revealed — capture must not be suppressed", 0, guard.held) }
        revealed = mapOf("mail:c1" to "pw")
        composeTestRule.runOnIdle { assertEquals("a revealed secret must suppress capture", 1, guard.held) }
        revealed = emptyMap()
        composeTestRule.runOnIdle { assertEquals("hiding the secret must lift suppression", 0, guard.held) }
    }

    // ── Blocked apps ──────────────────────────────────────────────────────

    @Test
    fun blockedAppsAreAbsentWhileNothingIsBlocked() {
        render(snapshot = snapshot(blocked = emptyList()))
        composeTestRule.onNodeWithTag("connected-apps-blocked-item").assertDoesNotExist()
    }

    @Test
    fun aBlockedClientShowsVerbatimWithUnblockForwardingItsId() {
        var unblocked: String? = null
        render(
            snapshot = snapshot(blocked = listOf(BlockedAppRow("https://spam.example/client", 1_700_000_000_000L))),
            onUnblock = { unblocked = it },
        )
        composeTestRule.onNodeWithTag("connected-apps-blocked-item").assertExists()
        // The node's own semantics text and the visible Text both carry it.
        assert(composeTestRule.onAllNodesWithText("https://spam.example/client", substring = true).fetchSemanticsNodes().isNotEmpty())
        composeTestRule.onNodeWithTag("connected-apps-blocked-item-unblock").performScrollTo().performClick()
        assertEquals("https://spam.example/client", unblocked)
    }
}
