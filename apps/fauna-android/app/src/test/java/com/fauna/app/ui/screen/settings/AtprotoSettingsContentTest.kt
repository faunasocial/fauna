package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiBridgeFollow
import com.fauna.ffi.FfiBridgeLinkField
import com.fauna.ffi.FfiBridgeLinkMode
import com.fauna.ffi.FfiBridgeStatus
import com.fauna.ffi.FfiCborValue
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_atproto_settings_machine.AppCredentialRow
import uniffi.fauna_atproto_settings_machine.AtprotoSettingsSnapshot
import uniffi.fauna_atproto_settings_machine.ContestCardRow
import uniffi.fauna_atproto_settings_machine.ContestConfirmCardModel
import uniffi.fauna_atproto_settings_machine.DelegationRow
import uniffi.fauna_atproto_settings_machine.DeleteConfirmCardModel
import uniffi.fauna_atproto_settings_machine.RetireIdentityOptIn
import uniffi.fauna_atproto_settings_machine.IdentitySummaryRow
import uniffi.fauna_atproto_settings_machine.TransitionCardModel
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the stateless [AtprotoSettingsContent] — the
 * `atproto` page (`docs/goal/ui/atproto.md`): the four-rung integration-depth
 * selector, the transition card, the Linked-account panel (the shared
 * [BridgeCard] embedded verbatim, `atproto.md` § Migration step 2), the hosted
 * panel, and the full-PDS panel (app credentials + connected apps +
 * kill-switch). No Hilt, no VM, no FFI native calls — mirrors
 * [com.fauna.app.ui.screen.personalization.LabelerCatalogContentTest].
 *
 * `composeTestRule.setContent` may only be called ONCE per test, so a
 * before/after comparison across two snapshots is two separate `@Test`
 * functions, never two `render()` calls in one; every `performClick()` on an
 * element that may be scrolled out of the fixed test viewport is preceded by
 * `performScrollTo()` (the `PersonalizationContentTest` idiom).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AtprotoSettingsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val emptySnapshot = AtprotoSettingsSnapshot(
        level = "off",
        hostedAllowed = false,
        hostedGateReason = LocalizedText("atproto_settings.gate_reason", emptyMap()),
        handlePreview = "",
        identity = null,
        link = null,
        pendingTransition = null,
        didMethod = "plc",
        showDidMethodRadio = false,
        historyBackfill = false,
        showDeletePresence = false,
        deleteConfirm = null,
        credentials = emptyList(),
        sessions = emptyList(),
        consents = emptyList(),
        externalAppsEnabled = true,
        delegation = null,
        contest = null,
        contestConfirm = null,
        error = null,
    )

    private fun unlinkedBridge() = FfiBridgeStatus(
        id = "bluesky",
        name = "Bluesky",
        available = true,
        linked = false,
        identity = null,
        mode = null,
        settings = emptyList(),
        supportsFollows = false,
        linkModes = listOf(
            FfiBridgeLinkMode(
                mode = "oauth",
                label = "Link Bluesky",
                clientAction = "oauth_redirect",
                platform = null,
                fields = listOf(FfiBridgeLinkField("handle", "Handle", "text", null)),
            ),
        ),
        error = null,
    )

    private fun render(
        snapshot: AtprotoSettingsSnapshot = emptySnapshot,
        blueskyBridge: FfiBridgeStatus? = unlinkedBridge(),
        blueskyFollows: List<FfiBridgeFollow> = emptyList(),
        onOpenContestConfirm: () -> Unit = {},
        onCancelContest: () -> Unit = {},
        onRequestContest: () -> Unit = {},
        onOpenDeleteConfirm: () -> Unit = {},
        onCancelDelete: () -> Unit = {},
        onConfirmDelete: () -> Unit = {},
        onSelectLevel: (String) -> Unit = {},
        onConfirmTransition: () -> Unit = {},
        onCancelTransition: () -> Unit = {},
        onSetDidMethod: (String) -> Unit = {},
        onSetHistoryBackfill: (Boolean) -> Unit = {},
        onSetExternalAppsEnabled: (Boolean) -> Unit = {},
        onMint: suspend (String, Boolean) -> Pair<String, String>? = { _, _ -> null },
        onRevealSecret: suspend (String) -> String? = { null },
        onRevoke: (String) -> Unit = {},
        onAuthorizeExternalApps: () -> Unit = {},
        onDeauthorizeExternalApps: () -> Unit = {},
        onLinkBridge: (String, Map<String, String>) -> Unit = { _, _ -> },
        onUnlinkBridge: () -> Unit = {},
        onUpdateBridgeSetting: (String, FfiCborValue) -> Unit = { _, _ -> },
        onAddBridgeFollow: (String, String?) -> Unit = { _, _ -> },
        onRemoveBridgeFollow: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            AtprotoSettingsContent(
                snapshot = snapshot,
                blueskyBridge = blueskyBridge,
                blueskyFollows = blueskyFollows,
                onBack = {},
                onOpenContestConfirm = onOpenContestConfirm,
                onCancelContest = onCancelContest,
                onRequestContest = onRequestContest,
                onOpenDeleteConfirm = onOpenDeleteConfirm,
                onCancelDelete = onCancelDelete,
                onConfirmDelete = onConfirmDelete,
                onSelectLevel = onSelectLevel,
                onConfirmTransition = onConfirmTransition,
                onCancelTransition = onCancelTransition,
                onSetDidMethod = onSetDidMethod,
                onSetHistoryBackfill = onSetHistoryBackfill,
                onSetExternalAppsEnabled = onSetExternalAppsEnabled,
                onMint = onMint,
                onRevealSecret = onRevealSecret,
                onRevoke = onRevoke,
                onAuthorizeExternalApps = onAuthorizeExternalApps,
                onDeauthorizeExternalApps = onDeauthorizeExternalApps,
                onLinkBridge = onLinkBridge,
                onUnlinkBridge = onUnlinkBridge,
                onUpdateBridgeSetting = onUpdateBridgeSetting,
                onAddBridgeFollow = onAddBridgeFollow,
                onRemoveBridgeFollow = onRemoveBridgeFollow,
            )
        }
    }

    @Test
    fun rendersStaticSelectorAtOffWithHostedRungsGated() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("atproto-page").assertExists()
        composeTestRule.onNodeWithTag("atproto-depth-selector").assertExists()
        composeTestRule.onNodeWithTag("atproto-depth-off").assertExists()
        composeTestRule.onNodeWithTag("atproto-depth-linked").assertExists()
        composeTestRule.onNodeWithTag("atproto-depth-hosted-visible").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("atproto-depth-hosted-full").assertIsNotEnabled()
        // Nothing level/state-gated renders at Off with no pending transition.
        composeTestRule.onNodeWithTag("atproto-depth-confirm-card").assertDoesNotExist()
        composeTestRule.onNodeWithTag("atproto-external-apps-enable").assertDoesNotExist()
        composeTestRule.onNodeWithTag("atproto-delete-presence").assertDoesNotExist()
    }

    @Test
    fun hostedRungsEnabledWhenGateAllows() {
        render(snapshot = emptySnapshot.copy(hostedAllowed = true))
        composeTestRule.onNodeWithTag("atproto-depth-hosted-visible").assertIsEnabled()
        composeTestRule.onNodeWithTag("atproto-depth-hosted-full").assertIsEnabled()
    }

    @Test
    fun selectingARungFiresOnSelectLevel() {
        var selected: String? = null
        render(onSelectLevel = { selected = it })
        composeTestRule.onNodeWithTag("atproto-depth-linked").performScrollTo().performClick()
        assertEquals("linked", selected)
    }

    @Test
    fun transitionCardRendersComposedLinesVerbatimAndConfirmCancelFire() {
        var confirmed = false
        var cancelled = false
        render(
            snapshot = emptySnapshot.copy(
                pendingTransition = TransitionCardModel(
                    targetLevel = "hosted_visible",
                    lines = listOf(LocalizedText("atproto_settings.card_mint", emptyMap())),
                    showHistoryBackfill = true,
                    inProgress = false,
                ),
            ),
            onConfirmTransition = { confirmed = true },
            onCancelTransition = { cancelled = true },
        )
        composeTestRule.onNodeWithTag("atproto-depth-confirm-card").assertExists()
        composeTestRule.onNodeWithTag("atproto-history-backfill").assertExists()
        composeTestRule.onNodeWithTag("atproto-depth-confirm").performScrollTo().performClick()
        assertEquals(true, confirmed)
        composeTestRule.onNodeWithTag("atproto-depth-cancel").performScrollTo().performClick()
        assertEquals(true, cancelled)
    }

    @Test
    fun transitionCardConfirmDisabledWhileInProgress() {
        render(
            snapshot = emptySnapshot.copy(
                pendingTransition = TransitionCardModel(
                    targetLevel = "hosted_visible",
                    lines = listOf(LocalizedText("atproto_settings.card_mint", emptyMap())),
                    showHistoryBackfill = false,
                    inProgress = true,
                ),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-depth-confirm").assertIsNotEnabled()
    }

    // ── The 72 h recovery-fork contest (`atproto-contest-*`) ────────────
    //
    // Mirrors apps/fauna-tui/src/settings/atproto.rs's `contest_elements`
    // tests — the reference shape every shell copies.

    @Test
    fun noViolationRendersNoContestSurfaceAtAll() {
        render(snapshot = emptySnapshot.copy(contest = null))
        composeTestRule.onNodeWithTag("atproto-contest-card").assertDoesNotExist()
    }

    @Test
    fun contestCardLeadsThePageAboveTheSelectorWithDetailAndDeadlineAndOpensConfirm() {
        var opened = false
        render(
            snapshot = emptySnapshot.copy(
                contest = ContestCardRow(
                    state = "contestable",
                    detail = LocalizedText("atproto_settings.contest_detail_contestable", emptyMap()),
                    deadline = LocalizedText("atproto_settings.contest_deadline", emptyMap()),
                    showContest = true,
                ),
            ),
            onOpenContestConfirm = { opened = true },
        )
        composeTestRule.onNodeWithTag("atproto-contest-card").assertExists()
        composeTestRule.onNodeWithTag("atproto-contest-detail").assertExists()
        composeTestRule.onNodeWithTag("atproto-contest-deadline").assertExists()
        composeTestRule.onNodeWithTag("atproto-contest").performScrollTo().performClick()
        assertEquals(true, opened)
    }

    @Test
    fun contestCardWindowClosedRendersNoDeadButton() {
        render(
            snapshot = emptySnapshot.copy(
                contest = ContestCardRow(
                    state = "window-closed",
                    detail = LocalizedText("atproto_settings.contest_detail_window_closed", emptyMap()),
                    deadline = null,
                    showContest = false,
                ),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-contest-card").assertExists()
        composeTestRule.onNodeWithTag("atproto-contest").assertDoesNotExist()
    }

    @Test
    fun contestCardNotContestableRendersNoDeadButton() {
        render(
            snapshot = emptySnapshot.copy(
                contest = ContestCardRow(
                    state = "not-contestable",
                    detail = LocalizedText("atproto_settings.contest_detail_genesis", emptyMap()),
                    deadline = null,
                    showContest = false,
                ),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-contest-card").assertExists()
        composeTestRule.onNodeWithTag("atproto-contest").assertDoesNotExist()
    }

    @Test
    fun contestConfirmCardRendersLinesVerbatimAndConfirmCancelFireAndDisableWhileInProgress() {
        var confirmed = false
        var cancelled = false
        render(
            snapshot = emptySnapshot.copy(
                contest = ContestCardRow(
                    state = "contestable",
                    detail = LocalizedText("atproto_settings.contest_detail_contestable", emptyMap()),
                    deadline = null,
                    showContest = true,
                ),
                contestConfirm = ContestConfirmCardModel(
                    lines = listOf(
                        LocalizedText("atproto_settings.contest_confirm_undo", emptyMap()),
                        LocalizedText("atproto_settings.contest_confirm_signs", emptyMap()),
                    ),
                    inProgress = false,
                ),
            ),
            onRequestContest = { confirmed = true },
            onCancelContest = { cancelled = true },
        )
        composeTestRule.onNodeWithTag("atproto-contest-confirm-card").assertExists()
        composeTestRule.onNodeWithTag("atproto-contest-confirm").performScrollTo().performClick()
        assertEquals(true, confirmed)
        composeTestRule.onNodeWithTag("atproto-contest-cancel").performScrollTo().performClick()
        assertEquals(true, cancelled)
    }

    @Test
    fun contestConfirmDisabledWhileInProgress() {
        render(
            snapshot = emptySnapshot.copy(
                contest = ContestCardRow(
                    state = "contestable",
                    detail = LocalizedText("atproto_settings.contest_detail_contestable", emptyMap()),
                    deadline = null,
                    showContest = true,
                ),
                contestConfirm = ContestConfirmCardModel(
                    lines = listOf(LocalizedText("atproto_settings.contest_confirm_undo", emptyMap())),
                    inProgress = true,
                ),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-contest-confirm").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("atproto-contest-cancel").assertIsNotEnabled()
    }

    @Test
    fun linkedPanelHiddenAtOff() {
        render(snapshot = emptySnapshot.copy(level = "off"))
        composeTestRule.onNodeWithTag("bridge-action-button").assertDoesNotExist()
    }

    @Test
    fun linkedPanelRendersSharedBridgeCardAtLevelLinked() {
        render(snapshot = emptySnapshot.copy(level = "linked"), blueskyBridge = unlinkedBridge())
        composeTestRule.onNodeWithTag("bridge-action-button").assertExists()
    }

    @Test
    fun linkedPanelFallsBackToUnlinkedSurfaceWhenBridgeStatusAbsent() {
        // No reply yet / nest built without the bluesky provider — the honest
        // state is "nothing is linked" (ui/atproto.md § Layout & flow), not a
        // crash or a blank panel.
        render(snapshot = emptySnapshot.copy(level = "linked"), blueskyBridge = null)
        composeTestRule.onNodeWithTag("bridge-action-button").assertExists()
    }

    @Test
    fun hostedPanelShowsDidMethodRadioPreMint() {
        render(
            snapshot = emptySnapshot.copy(
                level = "hosted_visible",
                showDidMethodRadio = true,
                handlePreview = "alice.example.com",
            ),
        )
        composeTestRule.onNodeWithTag("atproto-did-method").assertExists()
        composeTestRule.onNodeWithTag("atproto-hosted-handle").assertDoesNotExist()
    }

    @Test
    fun hostedPanelShowsHandleAfterMint() {
        render(
            snapshot = emptySnapshot.copy(
                level = "hosted_visible",
                showDidMethodRadio = false,
                identity = IdentitySummaryRow(handle = "alice.example.com", method = "plc", status = "active"),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-did-method").assertDoesNotExist()
        composeTestRule.onNodeWithTag("atproto-hosted-handle").assertExists()
    }

    /** The bug row 7 fixes: `ui/atproto.md` § Errors & edge cases says the
     *  identity summary renders (marked deactivated) at level Off/Linked, so
     *  the user sees what re-enabling restores. It used to sit inside the
     *  hosted panel, gated on `atHosted || targetingHosted` — exactly the two
     *  states this rule is about. */
    @Test
    fun identitySummaryRendersAtLevelOffWhenIdentityDeactivated() {
        render(
            snapshot = emptySnapshot.copy(
                level = "off",
                identity = IdentitySummaryRow(handle = "alice.example.com", method = "plc", status = "deactivated"),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-hosted-handle").assertExists()
    }

    @Test
    fun identityStatusDeletedRendersItsOwnLabelNotTheRawWireWord() {
        render(
            snapshot = emptySnapshot.copy(
                level = "off",
                identity = IdentitySummaryRow(handle = "alice.example.com", method = "plc", status = "deleted"),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-hosted-handle")
            .assertTextContains("deleted", substring = true, ignoreCase = true)
    }

    @Test
    fun identityStatusTombstonedRendersItsOwnLabelNotTheRawWireWord() {
        render(
            snapshot = emptySnapshot.copy(
                level = "off",
                identity = IdentitySummaryRow(handle = "alice.example.com", method = "plc", status = "tombstoned"),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-hosted-handle")
            .assertTextContains("retired", substring = true, ignoreCase = true)
    }

    @Test
    fun deleteConfirmCardRendersLinesVerbatimAndConfirmCancelFireAndDisableWhileInProgress() {
        var confirmed = false
        var cancelled = false
        render(
            snapshot = emptySnapshot.copy(
                level = "hosted_visible",
                showDeletePresence = true,
                deleteConfirm = DeleteConfirmCardModel(
                    lines = listOf(LocalizedText("atproto_settings.delete_confirm_sweeps", emptyMap())),
                    inProgress = false,
                    retireIdentity = RetireIdentityOptIn(available = false, selected = false, unavailableReason = null),
                ),
            ),
            onConfirmDelete = { confirmed = true },
            onCancelDelete = { cancelled = true },
        )
        composeTestRule.onNodeWithTag("atproto-delete-confirm-card").assertExists()
        composeTestRule.onNodeWithTag("atproto-delete-confirm").performScrollTo().performClick()
        assertEquals(true, confirmed)
        composeTestRule.onNodeWithTag("atproto-delete-cancel").performScrollTo().performClick()
        assertEquals(true, cancelled)
    }

    @Test
    fun deleteConfirmDisabledWhileInProgress() {
        render(
            snapshot = emptySnapshot.copy(
                level = "hosted_visible",
                showDeletePresence = true,
                deleteConfirm = DeleteConfirmCardModel(
                    lines = listOf(LocalizedText("atproto_settings.delete_confirm_sweeps", emptyMap())),
                    inProgress = true,
                    retireIdentity = RetireIdentityOptIn(available = false, selected = false, unavailableReason = null),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-delete-confirm").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("atproto-delete-cancel").assertIsNotEnabled()
    }

    @Test
    fun deletePresenceButtonOpensTheConfirmCeremony() {
        var opened = false
        render(
            snapshot = emptySnapshot.copy(level = "hosted_visible", showDeletePresence = true),
            onOpenDeleteConfirm = { opened = true },
        )
        composeTestRule.onNodeWithTag("atproto-delete-presence").performScrollTo().performClick()
        assertEquals(true, opened)
    }

    @Test
    fun didMethodSelectionFiresOnSetDidMethod() {
        var method: String? = null
        render(
            snapshot = emptySnapshot.copy(level = "hosted_visible", showDidMethodRadio = true),
            onSetDidMethod = { method = it },
        )
        composeTestRule.onNodeWithTag("atproto-did-method-web").performScrollTo().performClick()
        assertEquals("web", method)
    }

    @Test
    fun deletePresenceHiddenWhenShowDeletePresenceFalse() {
        render(snapshot = emptySnapshot.copy(level = "linked", showDeletePresence = false))
        composeTestRule.onNodeWithTag("atproto-delete-presence").assertDoesNotExist()
    }

    @Test
    fun deletePresenceVisibleWhenShowDeletePresenceTrue() {
        render(snapshot = emptySnapshot.copy(level = "linked", showDeletePresence = true))
        composeTestRule.onNodeWithTag("atproto-delete-presence").assertExists()
    }

    @Test
    fun fullPdsPanelHiddenBelowHostedFull() {
        render(snapshot = emptySnapshot.copy(level = "hosted_visible"))
        composeTestRule.onNodeWithTag("atproto-external-apps-enable").assertDoesNotExist()
        composeTestRule.onNodeWithTag("atproto-app-credential-mint").assertDoesNotExist()
    }

    @Test
    fun fullPdsPanelVisibleAtHostedFull() {
        render(snapshot = emptySnapshot.copy(level = "hosted_full"))
        composeTestRule.onNodeWithTag("atproto-external-apps-enable").assertExists()
        composeTestRule.onNodeWithTag("atproto-app-credentials-list").assertExists()
    }

    @Test
    fun killSwitchAlwaysRendersAndTogglesFireTargetValue() {
        // mail-settings-enabled-toggle idiom: rendered unconditionally, bound
        // to computed state, never wrapped in an `if`.
        var enabled: Boolean? = null
        render(
            snapshot = emptySnapshot.copy(level = "hosted_full", externalAppsEnabled = true),
            onSetExternalAppsEnabled = { enabled = it },
        )
        composeTestRule.onNodeWithTag("atproto-external-apps-enable").assertIsOn()
        composeTestRule.onNodeWithTag("atproto-external-apps-enable").performScrollTo().performClick()
        assertEquals(false, enabled)
    }

    @Test
    fun killSwitchOffKeepsCredentialRowsListed() {
        render(
            snapshot = emptySnapshot.copy(
                level = "hosted_full",
                externalAppsEnabled = false,
                credentials = listOf(credential("c1")),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-app-credential-item").assertExists()
    }

    @Test
    fun mintButtonFiresOnMintWithAutoLabelAndFalseDmAllowed() {
        var mintedLabel: String? = null
        var mintedDmAllowed: Boolean? = null
        render(
            snapshot = emptySnapshot.copy(level = "hosted_full"),
            onMint = { label, dmAllowed -> mintedLabel = label; mintedDmAllowed = dmAllowed; null },
        )
        composeTestRule.onNodeWithTag("atproto-app-credential-mint").performScrollTo().performClick()
        composeTestRule.waitForIdle()
        assertEquals(false, mintedDmAllowed)
        assertEquals(true, mintedLabel?.isNotBlank())
    }

    @Test
    fun credentialRevealHiddenWhenNotRevealable() {
        render(
            snapshot = emptySnapshot.copy(
                level = "hosted_full",
                credentials = listOf(credential("c1", revealable = false)),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-app-credential-reveal").assertDoesNotExist()
    }

    @Test
    fun credentialRevealShowsSecretInlineOnceRevealed() {
        render(
            snapshot = emptySnapshot.copy(
                level = "hosted_full",
                credentials = listOf(credential("c1", revealable = true)),
            ),
            onRevealSecret = { "s3cr3t" },
        )
        composeTestRule.onNodeWithTag("atproto-app-credential-reveal").performScrollTo().performClick()
        composeTestRule.waitForIdle()
        composeTestRule.onNodeWithTag("atproto-app-credential-reveal").assertTextEquals("s3cr3t")
    }

    @Test
    fun credentialRevokeFiresWithCredentialId() {
        var revoked: String? = null
        render(
            snapshot = emptySnapshot.copy(level = "hosted_full", credentials = listOf(credential("c1"))),
            onRevoke = { revoked = it },
        )
        composeTestRule.onNodeWithTag("atproto-app-credential-revoke").performScrollTo().performClick()
        assertEquals("c1", revoked)
    }

    @Test
    fun pageErrorIsNullByDefault() {
        // The page-level error rides the global error banner (mail-settings
        // idiom) off `snapshot.error`, not a locally-rendered element — assert
        // the seeded default snapshot carries none.
        assertNull(emptySnapshot.error)
    }

    private fun credential(id: String, revealable: Boolean = true) = AppCredentialRow(
        credentialId = id,
        label = "App credential $id",
        dmAllowed = false,
        createdAtMillis = 1_700_000_000_000L,
        lastUsedAtMillis = null,
        revealable = revealable,
    )

    /**
     * The D10 row is WITHHELD when no verified delegation exists — and the
     * `null` snapshot field means BOTH "never authorized" and "the stored cert
     * failed the client-side verify under this account's own identity key".
     * Rendering a row in the second case would present a grant the user cannot
     * be shown to have made, which is the security content of the slice, so the
     * absence is asserted rather than assumed.
     *
     * `-authorize` still renders: it is the way OUT of this state.
     */
    @Test
    fun withholdsTheDelegationRowUntilOneIsVerified() {
        render(snapshot = emptySnapshot.copy(level = "hosted_full", hostedAllowed = true, delegation = null))
        composeTestRule.onNodeWithTag("atproto-delegation-row").assertDoesNotExist()
        composeTestRule.onNodeWithTag("atproto-delegation-scope").assertDoesNotExist()
        composeTestRule.onNodeWithTag("atproto-delegation-status").assertDoesNotExist()
        composeTestRule.onNodeWithTag("atproto-delegation-last-used").assertDoesNotExist()
        // Revoke has nothing to act on; authorize is the remedy and must be there.
        composeTestRule.onNodeWithTag("atproto-delegation-revoke").assertDoesNotExist()
        composeTestRule.onNodeWithTag("atproto-delegation-authorize").assertExists()
    }

    /**
     * A live delegation renders all four leaves — and `-authorize` STAYS,
     * because re-authorizing IS the renewal gesture (provisioning overwrites the
     * cert). A page that hid it once authorized would force the
     * revoke-then-re-mint flow the ruling forbids, so its presence on a live row
     * is the assertion that matters, not the row's mere existence.
     */
    @Test
    fun rendersEveryDelegationLeafAndKeepsAuthorizeOnALiveRow() {
        render(snapshot = emptySnapshot.copy(level = "hosted_full", hostedAllowed = true, delegation = delegation()))
        composeTestRule.onNodeWithTag("atproto-delegation-row").assertExists()
        composeTestRule.onNodeWithTag("atproto-delegation-scope").assertExists()
        composeTestRule.onNodeWithTag("atproto-delegation-lasts-until").assertExists()
        composeTestRule.onNodeWithTag("atproto-delegation-status").assertExists()
        composeTestRule.onNodeWithTag("atproto-delegation-last-used").assertExists()
        composeTestRule.onNodeWithTag("atproto-delegation-authorize").assertExists()
        composeTestRule.onNodeWithTag("atproto-delegation-revoke").assertExists()
    }

    /**
     * `-status` carries the liveness WIRE spelling, not its prose — that is what
     * an e2e asserts, so a wording change never breaks a test and a test never
     * pins prose. Checked on `expired` specifically: a lapsed grant must stay
     * VISIBLE with its remedy, since silent feature loss is exactly what the
     * lapse journey forbids.
     */
    @Test
    fun theStatusLeafCarriesTheLivenessWireSpellingAndALapseKeepsItsRemedy() {
        render(
            snapshot = emptySnapshot.copy(
                level = "hosted_full",
                hostedAllowed = true,
                delegation = delegation(liveness = "expired"),
            ),
        )
        composeTestRule.onNodeWithTag("atproto-delegation-status")
            .assertContentDescriptionEquals("expired")
        composeTestRule.onNodeWithTag("atproto-delegation-row").assertExists()
        composeTestRule.onNodeWithTag("atproto-delegation-authorize").assertExists()
    }

    /** MICROseconds on this row (cert-derived), milliseconds on `lastUsedAt`
     *  (wire-derived) — the mismatch is real and worth pinning in a fixture. */
    private fun delegation(
        liveness: String = "active",
        lastUsedAtMillis: Long? = null,
    ) = DelegationRow(
        deviceKeyHex = "aa".repeat(32),
        capabilities = listOf("Post", "UpdateProfile"),
        authorizedAtMicros = 1_700_000_000_000_000UL,
        expiresAtMicros = 1_707_776_000_000_000UL,
        liveness = liveness,
        lastUsedAtMillis = lastUsedAtMillis,
    )
}
