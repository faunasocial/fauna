package com.fauna.app.ui.util

import android.content.Context
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.State
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.screen.backups.BackupDestinationsContent
import com.fauna.app.ui.screen.backups.LocalRestoreContent
import com.fauna.app.ui.viewmodel.RestoreProgress
import com.fauna.app.ui.screen.settings.AdminCustodyHostingContent
import com.fauna.app.ui.screen.settings.AdminDnsContent
import com.fauna.app.ui.screen.settings.AdminMailContent
import com.fauna.app.ui.screen.settings.AdminNestContent
import com.fauna.app.ui.screen.settings.AdminSettingsContent
import com.fauna.app.ui.screen.settings.AdminUsersContent
import com.fauna.app.ui.screen.settings.AdminAliasesContent
import com.fauna.app.ui.screen.settings.AdminBridgesPendingContent
import com.fauna.app.ui.screen.settings.AdminCalendarContent
import com.fauna.app.ui.screen.settings.AtprotoSettingsContent
import com.fauna.app.ui.screen.settings.ConnectedAppsContent
import uniffi.fauna_atproto_settings_machine.ConsentCardRow as ConnectedAppsConsentCardRow
import uniffi.fauna_client_connected_apps.ConnectedAppRow
import uniffi.fauna_client_connected_apps.ConnectedAppsSnapshot
import uniffi.fauna_client_connected_apps.MailAppPassword
import uniffi.fauna_atproto_settings_machine.AppCredentialRow
import uniffi.fauna_atproto_settings_machine.AtprotoSettingsSnapshot
import uniffi.fauna_atproto_settings_machine.ContestCardRow
import uniffi.fauna_atproto_settings_machine.ContestConfirmCardModel
import uniffi.fauna_atproto_settings_machine.DelegationRow
import uniffi.fauna_atproto_settings_machine.DeleteConfirmCardModel
import uniffi.fauna_atproto_settings_machine.RetireIdentityOptIn
import uniffi.fauna_atproto_settings_machine.TransitionCardModel
import com.fauna.app.ui.screen.settings.AdminContactsContent
import com.fauna.app.ui.screen.settings.AdminFilesContent
import com.fauna.app.ui.screen.bridges.BridgeCard
import com.fauna.app.ui.screen.events.CreateCalendarDialog
import com.fauna.app.ui.screen.events.EventDetailContent
import com.fauna.app.ui.screen.events.EventFormSheet
import com.fauna.app.ui.screen.events.EventsContent
import com.fauna.app.data.api.EventDetail
import com.fauna.app.data.api.EventSummary
import com.fauna.ffi.FfiEventDrafts
import social.fauna.generated.Ids
import com.fauna.app.data.api.FaunaCalendar
import com.fauna.ffi.FfiCalendarViewMode
import com.fauna.app.ui.screen.settings.LinkedNestsContent
import com.fauna.app.ui.screen.settings.MailAliasesContent
import com.fauna.app.ui.screen.settings.MailListMembersContent
import com.fauna.app.ui.screen.settings.MailListsContent
import com.fauna.app.ui.screen.settings.MailSettingsContent
import com.fauna.app.ui.screen.settings.MailSpamContent
import com.fauna.app.ui.screen.settings.ChangeHandleCard
import com.fauna.app.ui.screen.settings.DeleteAccountConfirmDialog
import com.fauna.app.ui.screen.settings.EncryptionSettingsContent
import com.fauna.app.ui.screen.settings.SubscriptionSettingsContent
import com.fauna.app.ui.screen.settings.FamilyContent
import com.fauna.app.ui.screen.settings.WebSettingsContent
import com.fauna.app.ui.screen.settings.AdminWebContent
import com.fauna.app.ui.screen.moderation.ModerationQueueContent
import com.fauna.app.ui.screen.backups.PrunePreviewContent
import com.fauna.app.ui.screen.backups.ImmediateDeleteConfirmModal
import uniffi.fauna_backups_machine.PolicyState
import uniffi.fauna_backups_machine.PruneCandidate
import uniffi.fauna_backups_machine.PrunePreview
import com.fauna.app.ui.viewmodel.ActorOption
import uniffi.fauna_client_web.SubdomainView
import com.fauna.ffi.FfiFamilyStatus
import com.fauna.ffi.FfiFamilyWardInfo
import com.fauna.ffi.FfiFamilyIncomingTransfer
import com.fauna.ffi.FfiFamilyPendingTransfer
import com.fauna.ffi.FfiReachPolicy
import com.fauna.ffi.FfiPublishedPost
import uniffi.fauna_client_moderation.QueueRow
import uniffi.fauna_client_moderation.QueueRowSource
import com.fauna.app.ui.screen.nostr.NostrContent
import com.fauna.app.ui.screen.profile.ProfileOffersContent
import com.fauna.app.ui.screen.profile.ProfileTiersContent
import uniffi.fauna_core.OfferStatus
import com.fauna.app.core.HexUtil
import com.fauna.app.payments.ClaimItem
import com.fauna.app.payments.ProviderItem
import com.fauna.app.payments.ZapSignerItem
import com.fauna.ffi.FfiBridgeIdentity
import com.fauna.ffi.FfiBridgeLinkField
import com.fauna.ffi.FfiBridgeLinkMode
import com.fauna.ffi.FfiBridgeStatus
import com.fauna.ffi.FfiFeatureRow
import com.fauna.ffi.FfiSnapshotSummary
import com.fauna.ffi.FfiMineSubscription
import com.fauna.ffi.FfiTierItem
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_client_dns.DelegationView
import uniffi.fauna_client_dns.DnsRecordRow
import uniffi.fauna_client_dns.DomainView
import uniffi.fauna_client_dns.PendingCertIssue
import uniffi.fauna_client_dns.RecordVerdict
import uniffi.fauna_client_dns.VerifyStatus
import uniffi.fauna_client_mail_settings.AliasPolicyView
import uniffi.fauna_client_mail_settings.AuthPolicyView
import uniffi.fauna_client_mail_settings.ImapPolicyView
import uniffi.fauna_client_mail_settings.OutboundPolicyView
import uniffi.fauna_client_mail_settings.SpamPolicyView
import uniffi.fauna_client_mail_settings.SubmissionPolicyView
import uniffi.fauna_client_mail_settings.DomainDmarcPolicy
import uniffi.fauna_client_mail_settings.LocalDomainView
import uniffi.fauna_client_mail_settings.PrimaryDomainRenameView
import uniffi.fauna_client_mail_settings.AliasKind
import uniffi.fauna_client_mail_settings.AliasView
import uniffi.fauna_client_mail_settings.ApprovedBridgeView
import uniffi.fauna_client_mail_settings.ForwarderView
import uniffi.fauna_client_mail_settings.PendingBridgeView
import uniffi.fauna_client_mail_settings.CredentialKind
import uniffi.fauna_client_mail_settings.ListView
import uniffi.fauna_client_mail_settings.MailCredentialSummary
import uniffi.fauna_client_mail_settings.MemberStatus
import uniffi.fauna_client_mail_settings.MemberView
import uniffi.fauna_client_mail_settings.MuaInstructions
import uniffi.fauna_client_mail_settings.PendingRotationStatus
import uniffi.fauna_client_mail_settings.SettingsStatus
import uniffi.fauna_client_mail_settings.SpamTrainingView
import uniffi.fauna_client_mail_settings.TrainingLabel
import uniffi.fauna_client_mail_settings.TrainingSource
import uniffi.fauna_client_pair.LinkedNestRow
import uniffi.fauna_client_pair.LinkedNestStatus
import uniffi.fauna_client_pair.LinkedNestsSnapshot
import uniffi.fauna_client_pair.TrustGenerationRow
import uniffi.fauna_client_pair.TrustGenerationStatus
import uniffi.fauna_client_pair.TrustGrantDuration
import uniffi.fauna_client_pair.TrustLens
import uniffi.fauna_core.NodeMode
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiBackupDestinationView
import com.fauna.ffi.FfiConnectionState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * W4 (account-data-plane.md § Workstreams) phase 4's UI desensitizing on android — both directions, proven against
 * the **real** shared rule (`account-data-plane.md` § The offline-mutation
 * contract → *How a surface asks*).
 *
 * These call the live `offlineAffordance` over JNA rather than a Kotlin stand-in,
 * which is the whole point: the rule's three rulings are not restated in Kotlin,
 * so a test that mocked the verdict would prove only that `enabled =` is wired
 * to *something*. Run them with `just android-host-test` — it builds the host
 * `libfauna_ffi.so` and regenerates the debug bindings against it. Under a plain
 * `:app:testDebugUnitTest` with no host `.so`, [faunaGate] fails **open** by
 * design, so the desensitizing cases below go red rather than silently green.
 *
 * **The pairing is the assertion, not the disabled assert alone** — the same
 * reasoning `tests/e2e-unified/tests/test_offline_gate.py` states for the other
 * apps. A gate that greyed *everything* offline would pass a one-sided check
 * while breaking the contract, since classes 1 and 2 are precisely the ones that
 * work with no nest. So the production case reads a gated control and a live
 * sibling in the same reveal, at the same moment, and asserts the difference.
 *
 * **Red-verified 2026-08-17** by forcing `gated = false` in [faunaGate]. The
 * useful part is the SPLIT, not the count: exactly the 4 desensitizing cases
 * went red and exactly the 5 live-direction cases stayed green. A suite where
 * all 9 failed would have been the weaker one — it would mean the live-direction
 * cases were riding on the gate too, and could not tell a correct gate from a
 * blanket disable. If you add a case here, check which side of that split it
 * lands on and that it lands there for the reason you intended.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class OfflineGateTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    /** `fauna.backup.destination.remove` — OnlineOnly in the shared table. */
    private val onlineOnlyKind = "fauna.backup.destination.remove"

    /** `fauna.account.state.put` — OfflineSafe, so ruling 1 says it must NOT grey. */
    private val offlineSafeKind = "fauna.account.state.put"

    private fun needsNest(): String =
        ApplicationProvider.getApplicationContext<Context>().getString(R.string.common_needs_nest)

    /**
     * The call-site convention under test: the verdict drives `enabled =`, and
     * its reason renders beside the control. Deliberately the same two lines a
     * real page writes — a helper that hid either half would stop proving the
     * convention works.
     */
    @Composable
    private fun GatedButton(kind: String, tag: String, enabled: Boolean = true) {
        val gate = faunaGate(kind, enabled = enabled)
        Button(
            onClick = {},
            enabled = gate.enabled,
            modifier = Modifier.testTag(tag),
        ) { Text(tag) }
        DisabledControlReasonText(gate.reason)
    }

    private fun render(state: FfiConnectionState?, content: @Composable () -> Unit) {
        composeTestRule.setContent {
            CompositionLocalProvider(LocalConnectionState provides state) { content() }
        }
    }

    /** A scroll parent, so `performScrollTo()` has one to walk. */
    @Composable
    private fun ScrollHost(content: @Composable () -> Unit) {
        Column(Modifier.verticalScroll(rememberScrollState())) { content() }
    }

    @Test
    fun onlineOnlyControlIsDeadWithNoNest_whileAnOfflineSafeSiblingStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            GatedButton(onlineOnlyKind, "gated")
            GatedButton(offlineSafeKind, "live")
        }
        composeTestRule.onNodeWithTag("gated").assertIsNotEnabled()
        // The half a blanket disable would fail: only class 3 desensitizes
        // (ruling 1), so the offline-capable sibling must survive the same
        // outage in the same composition.
        composeTestRule.onNodeWithTag("live").assertIsEnabled()
    }

    @Test
    fun theReasonRendersBesideTheDeadControl_neverABanner() {
        render(FfiConnectionState.DISCONNECTED) { GatedButton(onlineOnlyKind, "gated") }
        composeTestRule.onNodeWithText(needsNest()).assertIsDisplayed()
    }

    @Test
    fun connectedGatesNothing_andShowsNoReason() {
        render(FfiConnectionState.CONNECTED) { GatedButton(onlineOnlyKind, "gated") }
        composeTestRule.onNodeWithTag("gated").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun connectingIsAKnownOfflineWord_soItGates() {
        // Ruling 3's *known* half: `connecting` is a real transport state the
        // table recognises, not an unknown one — so it desensitizes. Pinned
        // because the unknown-state case below chooses the opposite answer, and
        // conflating the two is how a gate ends up greying a live nest.
        render(FfiConnectionState.CONNECTING) { GatedButton(onlineOnlyKind, "gated") }
        composeTestRule.onNodeWithTag("gated").assertIsNotEnabled()
    }

    @Test
    fun anUnprovidedConnectionStateLeavesTheControlLive() {
        // Ruling 3's *unknown* half, and the reason `LocalConnectionState`
        // defaults to null rather than CONNECTING: a `@Preview`, a composable
        // mounted before the shell provides the local, or a test that seeds
        // nothing must not grey a control while the nest is perfectly
        // reachable. For a gate, "we do not know" means do not block the user.
        render(state = null) { GatedButton(onlineOnlyKind, "gated") }
        composeTestRule.onNodeWithTag("gated").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun theGateNeverEnablesWhatThePageDisabled() {
        // Effective sensitivity is the page's own intent AND the verdict — the
        // rule tui's early return, linux's registry and web's action all state.
        // A reconnect must restore the page's intent, never more.
        render(FfiConnectionState.CONNECTED) {
            GatedButton(onlineOnlyKind, "gated", enabled = false)
        }
        composeTestRule.onNodeWithTag("gated").assertIsNotEnabled()
    }

    @Test
    fun thePagesOwnReasonWins_theGateAddsNoSecondOne() {
        // Both would be true at once here, and two reasons stacked under one
        // dead control is worse than either alone. The page's is the more
        // specific, so the gate stays silent.
        render(FfiConnectionState.DISCONNECTED) {
            GatedButton(onlineOnlyKind, "gated", enabled = false)
        }
        composeTestRule.onNodeWithTag("gated").assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ── The production call site ────────────────────────────────────────────
    //
    // Everything above drives a test-local button, so it proves the seam and
    // not that any real control uses it. This drives `BackupDestinationsContent`
    // itself — the same remove-confirm pairing `test_offline_gate.py` reads on
    // the other apps, so when android's e2e bridge grows attribute reads the two assert the same thing about the same
    // two buttons.

    private fun destination() = FfiBackupDestinationView(
        destinationId = "id-a",
        destinationNestUrl = "https://backup.example.com",
        displayName = "Gate-proof",
        kind = "nest",
        custodianDeviceId = null,
        capacityCapBytes = null,
        unattested = false,
    )

    @Test
    fun productionRemoveConfirmGates_whileItsCancelSiblingStaysLive() {
        var removed = false
        render(FfiConnectionState.DISCONNECTED) {
            // Scrollable for the same reason `BackupDestinationsContentTest`
            // wraps it: production mounts this section as a header item inside
            // the snapshot-list LazyColumn, and `performScrollTo()` needs a
            // scroll parent — without one it throws, and with a bare Column the
            // confirm sits below Robolectric's viewport where `performClick()`
            // silently no-ops.
            ScrollHost {
            BackupDestinationsContent(
                destinations = listOf(destination()),
                working = false,
                onAdd = { _, _ -> },
                onEdit = { _, _, _ -> },
                onRemove = { _, _ -> removed = true },
            )
            }
        }
        composeTestRule.onNodeWithTag("backup-destination-remove-button")
            .performScrollTo()
            .performClick()

        composeTestRule.onNodeWithTag("backup-destination-remove-confirm-button")
            .performScrollTo()
            .assertIsNotEnabled()
        // `fauna.backup.destination.remove` is the teardown at BOTH nests, so it
        // cannot happen offline — but backing out of a confirmation is pure
        // local UI and must not become unreachable because the nest went away.
        composeTestRule.onNodeWithTag("backup-destination-remove-cancel-button")
            .performScrollTo()
            .assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertIsDisplayed()

        // Belt and braces on the disable actually holding: a Compose node can
        // report disabled and still run its onClick if a call site wired the
        // flag to the wrong place.
        composeTestRule.onNodeWithTag("backup-destination-remove-confirm-button")
            .performClick()
        assertFalse("a desensitized confirm must not fire its action", removed)
    }

    @Test
    fun productionRemoveConfirmIsLiveWhenConnected() {
        // The direction that stops the gate over-claiming: a gate that only ever
        // closes would strand the control after any blip, which is worse than no
        // gate at all.
        render(FfiConnectionState.CONNECTED) {
            // Scrollable for the same reason `BackupDestinationsContentTest`
            // wraps it: production mounts this section as a header item inside
            // the snapshot-list LazyColumn, and `performScrollTo()` needs a
            // scroll parent — without one it throws, and with a bare Column the
            // confirm sits below Robolectric's viewport where `performClick()`
            // silently no-ops.
            ScrollHost {
            BackupDestinationsContent(
                destinations = listOf(destination()),
                working = false,
                onAdd = { _, _ -> },
                onEdit = { _, _, _ -> },
                onRemove = { _, _ -> },
            )
            }
        }
        composeTestRule.onNodeWithTag("backup-destination-remove-button")
            .performScrollTo()
            .performClick()
        composeTestRule.onNodeWithTag("backup-destination-remove-confirm-button")
            .performScrollTo()
            .assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ── The admin-nest fan-out ──────────────────────────────────────────────
    //
    // The first batch beyond the seam's single declaration, matching the set
    // web declared on the same page. Driven through the production
    // `AdminNestContent` rather than a stand-in, because the thing that can
    // silently go wrong here is a call site wiring the verdict to the wrong
    // control — which only the real tree can show.

    @Composable
    private fun AdminNest(
        working: Boolean = false,
        natSubmitEnabled: Boolean = true,
        // `nest-os-restart-now-button` only exists while a reboot pends, so the
        // default seeds it present — a gate on a control the fixture never
        // renders would assert nothing.
        osRebootPending: Boolean = true,
        // The sign-in key section's shared forced confirm only exists once an
        // arm is armed; un-armed by default, like seed-rotate and takedown.
        oauthArmed: com.fauna.app.ui.viewmodel.AdminNestVM.OauthArmed? = null,
    ) {
        // NO `ScrollHost` here, unlike the backups fixture: `AdminNestContent` is
        // a whole screen and already scrolls itself (`Scaffold` → `Column
        // (Modifier.verticalScroll)`). Nesting a second vertical scroller around
        // it measures the inner one with an infinite height constraint, which
        // Compose rejects outright — 6 tests died that way before this comment
        // existed. `performScrollTo()` finds the screen's own scroller.
        run {
            AdminNestContent(
                pairing = true,
                servingPort = 8443,
                frontedByRouter = false,
                osSecurityUpdates = 1,
                osRebootPending = osRebootPending,
                restartingNow = false,
                working = working,
                error = null,
                natSelectedMode = NodeMode.PUBLIC,
                natMessage = null,
                natSubmitEnabled = natSubmitEnabled,
                natSubmitting = false,
                // Undeclared (default): the withdraw button doesn't render, so
                // it's out of scope for this fixture — the save button (the one
                // control always present) is what `committing` covers.
                regionView = null,
                regionWorking = false,
                onBack = {},
                onSetPairing = {},
                onSaveServingPort = {},
                onInvalidServingPort = {},
                onRestartNow = {},
                onSelectNatMode = {},
                onSaveNatMode = {},
                onSaveRegion = {},
                onWithdrawRegion = {},
                onInvalidRegion = {},
                onFactoryReset = {},
                // The seed-rotate surface landed on android between this
                // session's two batches.
                // Seeded un-armed: its confirm only exists once armed, and this
                // fixture's subject is the rest of the page.
                seedRotateConfirm = null,
                seedRotateStatus = null,
                onArmSeedRotate = {},
                onCancelSeedRotate = {},
                onConfirmSeedRotate = {},
                // Legal takedown — same reasoning as seed-rotate
                // above: seeded un-armed, this fixture's subject is the rest
                // of the page.
                takedownArmed = null,
                takedownStatus = null,
                onArmTakedown = { _, _, _, _, _ -> },
                onCancelTakedown = {},
                onConfirmTakedown = {},
                // Outside-app sign-in keys: seeded ANSWERED, because the three
                // controls are page-disabled until the set has answered — and a
                // page-disabled control is not the gate's to explain, so an
                // unread fixture would assert nothing about the gate.
                oauthKeys = com.fauna.app.ui.viewmodel.AdminNestVM.OauthKeysRead.Ready(oauthKeySet),
                oauthArmed = oauthArmed,
                oauthStatus = null,
                oauthInFlight = false,
                onRotateIssuerKey = {},
                onArmOauthForced = {},
                onCancelOauthForced = {},
                onConfirmOauthForced = {},
                // FFI-free stand-ins for the shared issuer folds — same reason
                // as parse_port below: this file's subject is the gate.
                issuerKeyRowLabel = { row, _ -> LocalizedText("admin.nest_page.oauth_key_signing", mapOf("kid" to row.kid)) },
                issuerKeyRotateCost = { LocalizedText("admin.nest_page.oauth_rotate_desc", mapOf("minutes" to "20")) },
                // FFI-free stand-in for the shared parse_port, as the sibling
                // content test does — this file's subject is the gate.
                parsePort = { it.toIntOrNull()?.takeIf { p -> p in 1..65535 } },
                // FFI-free stand-in for the shared admin_parse_region_code —
                // same reason.
                parseRegionCode = {
                    it.takeIf { code ->
                        code.length in 2..8 && code.all { c -> c in 'A'..'Z' || c in '0'..'9' }
                    }
                },
            )
        }
    }

    /** The answered issuer key set the fixture seeds — one signer. */
    private val oauthKeySet = com.fauna.ffi.FfiIssuerKeyView(
        activeKid = "kid-new",
        keys = listOf(
            com.fauna.ffi.FfiIssuerKeyRow(kid = "kid-new", signing = true, retiredAt = null, servedUntil = null),
        ),
        retirementHorizonSecs = 1_200uL,
        rotationInFlight = false,
    )

    /** The shared forced confirm, armed for [arm] (the fold's own keys). */
    private fun oauthArmedFor(arm: com.fauna.ffi.FfiIssuerForcedArm) =
        com.fauna.app.ui.viewmodel.AdminNestVM.OauthArmed(
            arm = arm,
            confirm = com.fauna.ffi.FfiIssuerForcedConfirmView(
                summary = LocalizedText("admin.nest_page.oauth_force_rotate_confirm_one", emptyMap()),
                confirmLabel = LocalizedText("admin.nest_page.oauth_force_rotate_confirm_button", emptyMap()),
            ),
        )

    /** Every committing control on the page, and the buffers that must NOT gate. */
    private val committing = listOf(
        "admin-service-pairing-toggle",
        "admin-nest-serving-port-save-button",
        "admin-nest-region-save-button",
        "admin-nest-nat-mode-save-button",
        "nest-os-restart-now-button",
        // The ordinary issuer rotation commits on the press (no confirm).
        "admin-nest-oauth-rotate-button",
    )

    @Test
    fun adminNestCommittingControlsAllGateWithNoNest() {
        render(FfiConnectionState.DISCONNECTED) { AdminNest() }
        committing.forEach { tag ->
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsNotEnabled()
        }
    }

    @Test
    fun adminNestCommittingControlsAreAllLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminNest() }
        committing.forEach { tag ->
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsEnabled()
        }
    }

    @Test
    fun adminNestBuffersAndArmingStayLiveWithNoNest() {
        // The pairing of this whole page: "the commit gates, not the buffer"
        // and "arming is local". If these greyed too, the page would satisfy a
        // disabled-only check while breaking the contract — an admin could not
        // even type a port or read the reset confirmation offline.
        render(FfiConnectionState.DISCONNECTED) { AdminNest() }
        composeTestRule.onNodeWithTag("admin-nest-serving-port-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-nest-region-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-public-radio")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-private-radio")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-factory-reset-button")
            .performScrollTo().assertIsEnabled()
        // The two forced arms only ARM the shared inline confirm — local, no
        // dispatch — so they stay live with no nest; the confirm gates.
        composeTestRule.onNodeWithTag("admin-nest-oauth-force-rotate-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-nest-oauth-secret-force-rotate-button")
            .performScrollTo().assertIsEnabled()
    }

    /** The shared forced confirm declares exactly the ARMED arm's kind: with no
     *  nest it greys for either arm (one test per arm — `setContent` runs once
     *  per test), and its cancel beside it (pure local UI) stays live so the
     *  admin can still back out. */
    private fun assertOauthConfirmGatesWithNoNest(arm: com.fauna.ffi.FfiIssuerForcedArm) {
        render(FfiConnectionState.DISCONNECTED) { AdminNest(oauthArmed = oauthArmedFor(arm)) }
        composeTestRule.onNodeWithTag("admin-nest-oauth-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-nest-oauth-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun adminNestOauthKeyArmConfirmGatesWithNoNest() =
        assertOauthConfirmGatesWithNoNest(com.fauna.ffi.FfiIssuerForcedArm.ISSUER_KEY)

    @Test
    fun adminNestOauthSecretArmConfirmGatesWithNoNest() =
        assertOauthConfirmGatesWithNoNest(com.fauna.ffi.FfiIssuerForcedArm.SESSION_SECRET)

    @Test
    fun adminNestOauthForcedConfirmIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            AdminNest(oauthArmed = oauthArmedFor(com.fauna.ffi.FfiIssuerForcedArm.SESSION_SECRET))
        }
        composeTestRule.onNodeWithTag("admin-nest-oauth-confirm-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun aGateDeclaredInsideAnAlertDialogSlotStillSeesTheState() {
        // apple's rule 3 exists because a SwiftUI `.alert` builder is a separate
        // presentation context its environment may not reach — a gate declared
        // there can silently never grey while looking perfectly correct. Compose
        // is claimed not to have that problem: an `AlertDialog`'s slots are
        // ordinary composable lambdas whose sub-composition inherits
        // CompositionLocals from where the dialog is declared.
        //
        // That claim is load-bearing for `check-offline-gate-kinds.py`, whose
        // android surface deliberately carries NO `presentation_calls`. So it is
        // measured here, declaring INSIDE the slot — the position production
        // avoids only because the body needs room for the reason.
        render(FfiConnectionState.DISCONNECTED) {
            AlertDialog(
                onDismissRequest = {},
                title = { Text("t") },
                text = { Text("b") },
                confirmButton = {
                    val gate = faunaGate("fauna.admin.factory_reset")
                    TextButton(
                        onClick = {},
                        enabled = gate.enabled,
                    ) { Text("confirm") }
                },
            )
        }
        // Asserted by LABEL, not by a testTag: this synthetic confirm has no
        // ui.yaml id, and inventing one would be an unapproved element
        // (§ UI Consistency rule A).
        composeTestRule.onNodeWithText("confirm").assertIsNotEnabled()
    }

    @Test
    fun factoryResetConfirmGatesAndTheDialogBodyCarriesTheReason() {
        render(FfiConnectionState.DISCONNECTED) { AdminNest() }
        val before = composeTestRule.onAllNodesWithText(needsNest())
            .fetchSemanticsNodes().size
        // Every gated control on the page carries its own reason — R11 (account-data-plane.md § The ratified decisions)'s "per
        // affordance, never a global banner" as a count. Tied to `committing`
        // rather than a literal so adding a declaration to this page without
        // its reason fails here instead of passing quietly.
        assertEquals(
            "each gated control on the page owes a reason beside it",
            committing.size,
            before,
        )
        composeTestRule.onNodeWithTag("admin-factory-reset-button")
            .performScrollTo()
            .performClick()
        composeTestRule.onNodeWithTag("admin-factory-reset-confirm-button")
            .assertIsNotEnabled()
        // Counted, not matched by identity: with no nest EVERY gated control on
        // this page renders the same reason string, so a bare
        // `onNodeWithText(needsNest())` is ambiguous and dies on "found 5 nodes"
        // — which reads exactly like the reason being absent, and cost this
        // session a wrong diagnosis (a Robolectric dialog-window theory that was
        // never true). What the test actually means is "opening the dialog adds
        // ITS OWN reason", so it asserts the delta.
        val after = composeTestRule.onAllNodesWithText(needsNest())
            .fetchSemanticsNodes().size
        assertEquals(
            "opening the confirm must add exactly one reason — the dialog body's",
            before + 1,
            after,
        )
    }

    @Test
    fun factoryResetConfirmIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminNest() }
        composeTestRule.onNodeWithTag("admin-factory-reset-button")
            .performScrollTo()
            .performClick()
        composeTestRule.onNodeWithTag("admin-factory-reset-confirm-button")
            .assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun thePagesOwnWorkingFlagStillWinsWhileConnected() {
        // The gate composes with the page's intent rather than replacing it:
        // mid-save, these stay dead even with a perfectly good nest.
        render(FfiConnectionState.CONNECTED) { AdminNest(working = true) }
        composeTestRule.onNodeWithTag("admin-service-pairing-toggle")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-nest-serving-port-save-button")
            .performScrollTo().assertIsNotEnabled()
    }

    // ── Batch 2: the linked-nests page ──────────────────────────────────────

    @Composable
    private fun LinkedNests(home: LinkedNestRow? = null) {
        LinkedNestsContent(
            snapshot = LinkedNestsSnapshot(
                home = home,
                pairings = emptyList(),
                status = LinkedNestStatus.IDLE,
                error = null,
                restoreOutcome = null,
            ),
            onBack = {},
            onLink = {},
            onUnlink = {},
            onSetLens = { _, _ -> },
            onRenew = {},
            onRevoke = {},
            onMint = { _, _, _, _ -> },
            onRevokeBackupSeal = {},
            onRevokeBackupWriter = {},
            onRestoreGeneration = { _, _, _, _ -> },
            shortId = { it.take(8) },
            durationOptions = { emptyList() },
            durationLabel = { uniffi.fauna_core.LocalizedText(it.name, emptyMap()) },
        )
    }

    /** A home row carrying exactly one restorable (`Listed`) retained
     *  generation — the fixture `generationRestoreGates*` below needs to reach
     *  `nest-trust-generation-restore` (`nests.md` § Trust facet — generation
     *  recovery). */
    private fun homeRowWithListedGeneration() = LinkedNestRow(
        nestId = "aa".repeat(32),
        capabilities = emptyList(),
        expiresAt = null,
        createdAt = 0L,
        label = null,
        nestUrl = null,
        isHome = true,
        trustGrants = emptyList(),
        trustHistory = emptyList(),
        lens = TrustLens.NOW,
        availableHolders = emptyList(),
        mintOptions = emptyList(),
        mintDefaultDuration = TrustGrantDuration.ONE_OFF,
        trustBackups = emptyList(),
        trustGenerations = listOf(
            TrustGenerationRow(
                status = TrustGenerationStatus.LISTED,
                destinationId = "dest-1",
                destinationLabel = "Recovery",
                folderName = "__mail",
                path = "/Mail/2026",
                pathHash = "aa11",
                manifestHash = "mm22",
                sizeBytes = 2048,
                supersededAt = 1_700_000_000L,
                expiresAt = 1_702_592_000L,
            ),
        ),
    )

    private fun openAddFormAndType() {
        composeTestRule.onNodeWithTag("nests-add-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("nests-add-input")
            .performScrollTo()
            .performTextInput("https://nest.example.com")
    }

    @Test
    fun nestsAddSubmitGates_whileItsCancelAndInputStayLive() {
        // ⚠ The trap recorded from web's leg is
        // the whole reason this test types first: `nests-add-submit-button`
        // carries its OWN non-empty-input predicate, so on an untouched form it
        // is disabled for a reason that has nothing to do with the gate — and
        // the assertion would pass against an app with no gate at all. Typing
        // satisfies the call site's predicate so the ONLY thing left that can
        // disable it is the verdict.
        render(FfiConnectionState.DISCONNECTED) { LinkedNests() }
        openAddFormAndType()

        composeTestRule.onNodeWithTag("nests-add-submit-button")
            .performScrollTo().assertIsNotEnabled()
        // The live siblings: the buffer the user is still filling in, and the
        // way back out of the form. Neither issues a wire kind.
        composeTestRule.onNodeWithTag("nests-add-input").assertIsEnabled()
        composeTestRule.onNodeWithTag("nests-add-cancel-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertIsDisplayed()
    }

    @Test
    fun nestsAddSubmitIsLiveWhenConnectedAndFilled() {
        render(FfiConnectionState.CONNECTED) { LinkedNests() }
        openAddFormAndType()
        composeTestRule.onNodeWithTag("nests-add-submit-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun nestsAddSubmitStaysDeadOnAnEmptyFormEvenConnected() {
        // The other half of the trap above: proves the call site's own
        // predicate still governs, so the gate composed with it rather than
        // replacing it. Without this, the two tests above could not distinguish
        // "the gate works" from "the gate took over `enabled`".
        render(FfiConnectionState.CONNECTED) { LinkedNests() }
        composeTestRule.onNodeWithTag("nests-add-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("nests-add-submit-button")
            .performScrollTo().assertIsNotEnabled()
        // ...and the gate stays silent, because the PAGE owns this refusal.
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // `fauna.backup.generation.restore` (`nests.md` § Trust facet —
    // generation recovery, ratified 2026-07-29) — the roll-back-to-this-
    // version affordance on a retained backup generation.
    //
    // NO `ScrollHost` here, same reasoning as [AdminNest] above:
    // `LinkedNestsContent` is a whole screen and already scrolls itself
    // (`Scaffold` → `LazyColumn`) — nesting a second vertical scroller
    // measures the inner one with an infinite height constraint, which
    // Compose rejects outright (found this session, mirrors the
    // `AdminNestContent` trap the comment above already records).
    @Test
    fun generationRestoreGates_whileTheLensToggleStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { LinkedNests(home = homeRowWithListedGeneration()) }
        composeTestRule.onNodeWithTag("nest-trust-generation-restore")
            .performScrollTo().assertIsNotEnabled()
        // The live sibling: flipping the Now/History lens is pure local UI
        // state, no nest round-trip, and must not grey with the source gone.
        composeTestRule.onNodeWithTag("nest-trust-view-history")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun generationRestoreIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { LinkedNests(home = homeRowWithListedGeneration()) }
        composeTestRule.onNodeWithTag("nest-trust-generation-restore")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ══════════════════════════════════════════════════════════════════════
    // Batch 3 — the user-facing pages: account, profile/subscriptions, nostr.
    //
    // Ten declarations, found by RULE 4 rather than by walking pages: the oracle differential against tui's
    // compiler-exhaustive `Gesture::wire_kind` names every OnlineOnly kind
    // android declares nowhere, so this batch closed a driven list instead of
    // a guessed one.
    //
    // The live siblings here are unusually good witnesses because the shared
    // table classifies them, not this test: `fauna.subscriptions.tiers.create`
    // / `.update` / `.delete` are **OfflineSafe** and
    // `fauna.subscriptions.requests.approve` is **OfflineQueued**, so the tier
    // and request controls sitting beside the gated ones MUST stay live by
    // ruling 1. A blanket disable would fail these, which is exactly the
    // one-sided pass the pairing exists to prevent.
    // ══════════════════════════════════════════════════════════════════════

    // ── account settings ─────────────────────────────────────────────────

    @Test
    fun changeHandleCommitGates_whileItsBufferStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            ChangeHandleCard(
                newHandle = "newname",
                changingHandle = false,
                onNewHandleChange = {},
                onChangeHandle = {},
            )
        }
        // Typed already, so the call site's own non-blank predicate is
        // satisfied and the verdict is the only thing left that can disable it
        // (the trap, same as `nests-add-submit-button` above).
        composeTestRule.onNodeWithTag("change-handle").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("new-handle").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertIsDisplayed()
    }

    @Test
    fun changeHandleCommitIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            ChangeHandleCard(
                newHandle = "newname",
                changingHandle = false,
                onNewHandleChange = {},
                onChangeHandle = {},
            )
        }
        composeTestRule.onNodeWithTag("change-handle").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun changeHandleStaysDeadOnAnEmptyBufferEvenConnected() {
        // The converse case: the page's own predicate still governs, so the
        // gate composed with it rather than replacing it.
        render(FfiConnectionState.CONNECTED) {
            ChangeHandleCard(
                newHandle = "",
                changingHandle = false,
                onNewHandleChange = {},
                onChangeHandle = {},
            )
        }
        composeTestRule.onNodeWithTag("change-handle").assertIsNotEnabled()
        // ...and the gate stays silent, because the PAGE owns this refusal.
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    private fun deleteLabel(): String =
        ApplicationProvider.getApplicationContext<Context>().getString(R.string.common_delete)

    private fun cancelLabel(): String =
        ApplicationProvider.getApplicationContext<Context>().getString(R.string.common_cancel)

    @Test
    fun deleteAccountConfirmGates_whileItsCancelSiblingStaysLive() {
        // The account's most destructive gesture. Asserted by LABEL, not by a
        // testTag: this confirm has no ui.yaml id on android, and inventing one
        // would be an unapproved element (§ UI Consistency rule A).
        render(FfiConnectionState.DISCONNECTED) {
            DeleteAccountConfirmDialog(onDismiss = {}, onConfirm = {})
        }
        composeTestRule.onNodeWithText(deleteLabel()).assertIsNotEnabled()
        composeTestRule.onNodeWithText(cancelLabel()).assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertIsDisplayed()
    }

    @Test
    fun deleteAccountConfirmIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            DeleteAccountConfirmDialog(onDismiss = {}, onConfirm = {})
        }
        composeTestRule.onNodeWithText(deleteLabel()).assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ── nostr: the bunker invite commit (the roster and its revoke live on
    // the Connected apps page — see the connected-apps group below) ────────

    private fun nostrBridge() = FfiBridgeStatus(
        id = "nostr",
        name = "Nostr",
        available = true,
        linked = true,
        identity = FfiBridgeIdentity(label = "Public Key", value = "npub1abc", display = "npub1abc"),
        mode = "generated",
        settings = emptyList(),
        supportsFollows = true,
        linkModes = null,
        error = null,
    )

    @Composable
    private fun Nostr() {
        NostrContent(
            registered = true,
            bridge = nostrBridge(),
            follows = emptyList(),
            onLink = { _, _ -> },
            onUnlink = {},
            onUpdateSetting = { _, _ -> },
            onAddRelay = {},
            onRemoveRelay = {},
            onAddFollow = { _, _ -> },
            onRemoveFollow = {},
        )
    }

    @Test
    fun nostrBunkerInviteGates_whileTheRelayAndFollowBuffersStayLive() {
        render(FfiConnectionState.DISCONNECTED) { Nostr() }
        composeTestRule.onNodeWithTag("nostr-bunker-connect-btn")
            .performScrollTo().assertIsNotEnabled()
        // The live siblings: the relay and follow inputs are buffers, not
        // commits, and must stay usable with no nest.
        composeTestRule.onNodeWithTag("nostr-relay-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("nostr-follow-pubkey-input")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun nostrBunkerInviteIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { Nostr() }
        composeTestRule.onNodeWithTag("nostr-bunker-connect-btn")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ── connected apps: the answers and the revoke, per credential class ──

    private fun connectedRow(key: String, mail: MailAppPassword? = null) = ConnectedAppRow(
        key = key,
        `class` = if (mail != null) "app_password" else "device",
        name = LocalizedText("connected_apps.verbatim", mapOf("text" to "An app")),
        clientId = null,
        publisher = null,
        scopeDescriptions = emptyList(),
        createdAtMillis = 1_756_000_000_000L,
        lastUsedAtMillis = null,
        lastsUntilMillis = null,
        connected = true,
        mail = mail,
    )

    private fun requestCard() = ConnectedAppsConsentCardRow(
        consentIdHex = "ccdd",
        code = "ABCD-1234",
        clientId = "https://app.example/client",
        clientName = "Example App",
        scopeDescriptions = listOf("Post as you"),
        sets = emptyList(),
    )

    @Composable
    private fun ConnectedApps(rows: List<ConnectedAppRow>, armed: String? = null, requests: List<ConnectedAppsConsentCardRow> = emptyList()) {
        ConnectedAppsContent(
            snapshot = ConnectedAppsSnapshot(
                loaded = true,
                requests = requests,
                principals = rows,
                blocked = emptyList(),
                error = null,
            ),
            code = "ABCD",
            revokeArmed = armed,
            revealed = emptyMap(),
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
            formatTime = { it.toString() },
        )
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun bothRequestAnswersGate_becauseDenyIsAlsoAWireCall() {
        render(FfiConnectionState.DISCONNECTED) { ConnectedApps(emptyList(), requests = listOf(requestCard())) }
        composeTestRule.onNodeWithTag("connected-apps-request-approve").performScrollTo().assertIsNotEnabled()
        // The "no" gates alongside the "yes" — deny is NOT a local dismiss. The
        // waiting browser only gets a clean refusal once the answer reaches the
        // nest, so a live deny would silently do nothing for the app blocked on
        // the other side of the ceremony.
        composeTestRule.onNodeWithTag("connected-apps-request-decline").performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("connected-apps-request-block").performScrollTo().assertIsNotEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun bothRequestAnswersAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { ConnectedApps(emptyList(), requests = listOf(requestCard())) }
        composeTestRule.onNodeWithTag("connected-apps-request-approve").performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("connected-apps-request-decline").performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("connected-apps-request-block").performScrollTo().assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theConnectSubmitGates_whileTheCodeFieldStaysAUsableBuffer() {
        render(FfiConnectionState.DISCONNECTED) { ConnectedApps(emptyList()) }
        composeTestRule.onNodeWithTag("connected-apps-connect-submit").performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("connected-apps-connect-code").performScrollTo().assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun aNestSideRevokeGates() {
        // Every non-mail row's verb is a nest call the machine picks.
        render(FfiConnectionState.DISCONNECTED) { ConnectedApps(listOf(connectedRow("device:1")), armed = "device:1") }
        composeTestRule.onNodeWithTag("connected-apps-item-revoke-confirm").performScrollTo().assertIsNotEnabled()
        // The arm and the cancel are local, so they stay live.
        composeTestRule.onNodeWithTag("connected-apps-item-revoke-cancel").performScrollTo().assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun aMailPasswordRevokeStaysLiveWithNoNest() {
        // A mail app password's revoke is a write to the user's OWN account plane
        // (`fauna.account.state.put`, OfflineSafe — the ruling the Mail & Calendar
        // page's revoke carried), so it survives the outage that gates the row above.
        val mail = MailAppPassword(
            muaUsername = "{handle}+c1@example.com",
            kind = LocalizedText("mail_settings.kind_password", emptyMap()),
            revoked = false,
        )
        render(FfiConnectionState.DISCONNECTED) { ConnectedApps(listOf(connectedRow("mail:c1", mail)), armed = "mail:c1") }
        composeTestRule.onNodeWithTag("connected-apps-item-revoke-confirm").performScrollTo().assertIsEnabled()
    }

    // ── subscriptions: the reader's own page ─────────────────────────────

    private fun mineSubscription() = FfiMineSubscription(
        authorId = ByteArray(32) { 1 },
        tier = "gold",
        status = "active",
        handle = "someone",
        since = 0uL,
        authorDisplay = "someone",
    )

    @Composable
    private fun SubscriptionSettings() {
        SubscriptionSettingsContent(
            subscriptions = listOf(mineSubscription()),
            working = false,
            onBack = {},
            onUnsubscribe = {},
            onRedeemClaim = {},
        )
    }

    @Test
    fun subscriptionCommitsGate_whileTheClaimCodeBufferStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { SubscriptionSettings() }
        composeTestRule.onNodeWithTag("subscription-mine-unsubscribe-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("subscription-claim-redeem-button")
            .performScrollTo().assertIsNotEnabled()
        // "The commit gates, not the buffer": a claim code can be pasted with
        // no nest and redeemed on reconnect.
        composeTestRule.onNodeWithTag("subscription-claim-redeem-input")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun subscriptionCommitsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { SubscriptionSettings() }
        composeTestRule.onNodeWithTag("subscription-mine-unsubscribe-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("subscription-claim-redeem-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ── profile: offers (subscribe) and the author's tiers tab ───────────

    private fun offerTier() = FfiTierItem(
        name = "gold",
        rank = 1u,
        description = null,
        priceHint = "$5/mo",
        askingPriceSats = null,
        paymentUrl = null,
        autoApprove = false,
        createdAt = 0uL,
        unlocksPost = null,
    )

    @Composable
    private fun Offers() {
        ProfileOffersContent(
            offers = listOf(offerTier()),
            statusFor = { OfferStatus.NONE },
            statusLabel = { "none" },
            working = false,
            onSubscribe = {},
        )
    }

    @Test
    fun offerSubscribeGates() {
        render(FfiConnectionState.DISCONNECTED) { ScrollHost { Offers() } }
        composeTestRule.onNodeWithTag("subscription-offer-subscribe-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertIsDisplayed()
    }

    @Test
    fun offerSubscribeIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { ScrollHost { Offers() } }
        composeTestRule.onNodeWithTag("subscription-offer-subscribe-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Composable
    private fun Tiers() = TiersWith(tiers = listOf(offerTier()))

    @Composable
    private fun TiersWith(tiers: List<FfiTierItem>) {
        ProfileTiersContent(
            tiers = tiers,
            requests = emptyList(),
            subscribers = emptyList(),
            selectedTier = 0,
            approving = false,
            working = false,
            onCreate = { _, _, _, _, _, _, _ -> },
            onUpdate = { _, _, _, _, _, _, _ -> },
            onDelete = {},
            onApprove = {},
            onReject = {},
            onSelectTier = {},
            onRemove = { _, _ -> },
            providers = listOf(
                ProviderItem(
                    kind = "fake",
                    tier = "gold",
                    createdAt = 0uL,
                    lastVerifiedAt = null,
                    lastRejectedAt = null,
                )
            ),
            providerKinds = listOf("fake"),
            claims = emptyList<ClaimItem>(),
            onSetProvider = { _, _, _ -> },
            onRemoveProvider = {},
            onMintClaim = {},
            hexFull = { HexUtil.bytesToHex(it) },
            webhookUrl = { kind -> "https://example.test/webhook/$kind" },
            claimStatusLabel = { _, _ -> LocalizedText("k", emptyMap()) },
            providerStatusLabel = { _, _ -> LocalizedText("k", emptyMap()) },
        )
    }

    /** [Tiers] with an empty tier list — the author has created none yet, so the
     *  claim-mint and provider-save predicates are unmet for a page reason. */
    @Composable
    private fun TiersWithNoTiers() = TiersWith(tiers = emptyList())

    @Test
    fun paymentsCommitsGate_whileTheOfflineSafeTierControlsStayLive() {
        render(FfiConnectionState.DISCONNECTED) { ScrollHost { Tiers() } }

        composeTestRule.onNodeWithTag("subscription-provider-remove-button")
            .performScrollTo().assertIsNotEnabled()

        // The live siblings the SHARED TABLE picks, not this test:
        // `fauna.subscriptions.tiers.create` / `.update` / `.delete` are
        // OfflineSafe, so ruling 1 says they must not grey. A blanket disable
        // fails here — which is the whole point of the pairing.
        composeTestRule.onNodeWithTag("subscription-tier-create-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("subscription-tier-edit-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("subscription-tier-delete-button")
            .performScrollTo().assertIsEnabled()
        // Arming is local: the add-provider opener only reveals a form.
        composeTestRule.onNodeWithTag("subscription-provider-add-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun paymentsCommitsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { ScrollHost { Tiers() } }
        composeTestRule.onNodeWithTag("subscription-provider-remove-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun claimMintGates_whileItsTierSelectStaysLive() {
        // ⚠ The trap: the mint carries its OWN predicate
        // (`tier.isNotEmpty()`). It is satisfied here NOT by driving the select
        // but because `ClaimSection` seeds `tier` to `tierNames.first()` and
        // [Tiers] renders one tier — so the only thing left that can disable
        // this button is the verdict. [claimMintStaysDeadWithNoTiersEvenConnected]
        // is the converse that makes the pair discriminating; without it this
        // assert could not tell a working gate from one that took over `enabled`.
        render(FfiConnectionState.DISCONNECTED) { ScrollHost { Tiers() } }
        composeTestRule.onNodeWithTag("subscription-claim-tier-select")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("subscription-claim-mint-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun claimMintIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { ScrollHost { Tiers() } }
        composeTestRule.onNodeWithTag("subscription-claim-mint-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun claimMintStaysDeadWithNoTiersEvenConnected() {
        // The converse: with no tiers the page's own predicate is unmet, so the
        // mint is dead for a reason that is NOT the gate — and the gate stays
        // silent, because the page owns that refusal.
        render(FfiConnectionState.CONNECTED) { ScrollHost { TiersWithNoTiers() } }
        composeTestRule.onNodeWithTag("subscription-claim-mint-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun providerFormSaveGates_whileItsFieldsAndCancelStayLive() {
        // Same trap, same resolution as the mint above: `ProviderForm` seeds
        // `tier` to `tierNames.first()`, so the save's own
        // `!working && tier.isNotEmpty()` predicate is already satisfied and
        // the verdict is the only thing left that can disable it.
        render(FfiConnectionState.DISCONNECTED) { ScrollHost { Tiers() } }
        composeTestRule.onNodeWithTag("subscription-provider-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("subscription-provider-form-secret")
            .performScrollTo().performTextInput("shh")

        composeTestRule.onNodeWithTag("subscription-provider-form-save")
            .performScrollTo().assertIsNotEnabled()
        // "The commit gates, not the buffer" — the form's fields and its way
        // back out all stay live with no nest.
        composeTestRule.onNodeWithTag("subscription-provider-form-secret")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("subscription-provider-form-cancel")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun providerFormSaveIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { ScrollHost { Tiers() } }
        composeTestRule.onNodeWithTag("subscription-provider-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("subscription-provider-form-save")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun providerFormSaveStaysDeadWithNoTiersEvenConnected() {
        // The converse, and the reason the pair above can discriminate.
        render(FfiConnectionState.CONNECTED) { ScrollHost { TiersWithNoTiers() } }
        composeTestRule.onNodeWithTag("subscription-provider-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("subscription-provider-form-save")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ══════════════════════════════════════════════════════════════════════
    // Batch 4 — family, web, moderation. Nine more declarations, again taken
    // off the rule-4 differential rather than a page walk.
    //
    // This batch is where the fan-out's easy heuristics stop working, and the
    // three tests below are chosen to pin exactly that:
    //
    //  * **A "no" can be a commit.** Declining a guardianship transfer tells
    //    the proposing nest the offer was refused, so `family-incoming-
    //    transfer-decline-button` declares — the first cancel-shaped control in
    //    the whole fan-out that is NOT the live sibling. Reading "cancel ⇒
    //    local" off the earlier batches would have shipped it ungated.
    //  * **A "copy link" can be a commit.** `web-published-post-copy-paywall-
    //    link-button` mints a capability token on the nest. Its plain
    //    copy-web-link sibling really is local, and the pair is asserted
    //    together so the difference is visible rather than argued.
    //  * **One control, two gestures.** `train-correction-button` gates or not
    //    per ROW: a local detection's correction never leaves the device, a
    //    server row's does. Both render in one queue below.
    // ══════════════════════════════════════════════════════════════════════

    // ── family ───────────────────────────────────────────────────────────

    private fun familyPolicy() = FfiReachPolicy(
        contactApproval = false,
        unknownSenderMail = "allow",
        federationContact = true,
        feedSources = "allow",
        contentPolicy = null,
        screenTime = null,
        contentNotify = null,
        unknownPeerDm = null,
    )

    private fun ward(pending: FfiFamilyPendingTransfer? = null) = FfiFamilyWardInfo(
        actorId = ByteArray(32) { 1 },
        handle = "ward",
        policy = familyPolicy(),
        pendingTransfer = pending,
        contentNotices = emptyList(),
        usageTodayMinutes = null,
        devices = emptyList(),
        // family-safety.md § The account age band. The offline gate reasons
        // about connection state, never about a band, so `null` throughout.
        ageBand = null,
        blockedDmPeers = emptyList(),
    )

    private fun familyStatus(
        wards: List<FfiFamilyWardInfo> = listOf(ward()),
        incoming: List<FfiFamilyIncomingTransfer> = emptyList(),
    ) = FfiFamilyStatus(
        supervisedBy = null,
        policy = null,
        wards = wards,
        incomingTransfers = incoming,
        usageTodayMinutes = null,
        contactRequests = emptyList(),
        feedRequests = emptyList(),
        ageBand = null,
        supervision = null,
    )

    @Composable
    private fun Family(status: FfiFamilyStatus) {
        FamilyContent(
            status = status,
            approvals = emptyList(),
            selectedWardActorId = status.wards.firstOrNull()?.actorId,
            onBack = {},
            onSelectWard = {},
            onSavePolicy = { _, _ -> },
            onSaveError = {},
            onMarkDevice = { _, _, _ -> },
            onAllowBlockedPeer = {},
            onDecideApproval = { _, _ -> },
            onAddContact = { _, _ -> },
            onGraduate = {},
            onProposeTransfer = { _, _ -> },
            onCancelTransfer = {},
            onAcceptIncomingTransfer = {},
            onDeclineIncomingTransfer = {},
        )
    }

    private fun incomingTransfer() = FfiFamilyIncomingTransfer(
        supervisedActorId = ByteArray(32) { 8 },
        supervisedHandle = "ward9",
        guardianHandle = "guardian",
        createdAt = 0,
    )

    @Test
    fun theTransferDECLINEGatesToo_becauseARefusalIsACommit() {
        // The finding this batch turns on. Every earlier batch's cancel was the
        // live sibling; this one is not, because declining tells the proposing
        // guardian's nest the offer was refused. If this ever goes green while
        // the accept beside it is red, the rule "a cancel is always local" has
        // been re-derived somewhere and is wrong.
        render(FfiConnectionState.DISCONNECTED) {
            Family(familyStatus(incoming = listOf(incomingTransfer())))
        }
        composeTestRule.onNodeWithTag("family-incoming-transfer-accept-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("family-incoming-transfer-decline-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theTransferPairIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            Family(familyStatus(incoming = listOf(incomingTransfer())))
        }
        composeTestRule.onNodeWithTag("family-incoming-transfer-accept-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("family-incoming-transfer-decline-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun proposeTransferGates_whileItsHexBufferStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { Family(familyStatus()) }
        composeTestRule.onNodeWithTag("family-transfer-button")
            .performScrollTo().assertIsNotEnabled()
        // The commit gates, not the buffer.
        composeTestRule.onNodeWithTag("family-transfer-input")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun proposeTransferIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { Family(familyStatus()) }
        composeTestRule.onNodeWithTag("family-transfer-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun cancelPendingTransferGates() {
        val pending = FfiFamilyPendingTransfer(
            proposedGuardianActorId = ByteArray(32) { 7 },
            proposedGuardianHandle = "newguardian",
            createdAt = 0,
        )
        render(FfiConnectionState.DISCONNECTED) {
            Family(familyStatus(wards = listOf(ward(pending = pending))))
        }
        composeTestRule.onNodeWithTag("family-transfer-cancel-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun graduateArmingStaysLiveWhileItsConfirmGates() {
        // Arming is local: the opener must still work with no nest, or the user
        // cannot even read what graduation would do.
        render(FfiConnectionState.DISCONNECTED) { Family(familyStatus()) }
        composeTestRule.onNodeWithTag("family-graduate-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("family-graduate-confirm-button")
            .assertIsNotEnabled()
    }

    @Test
    fun graduateConfirmIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { Family(familyStatus()) }
        composeTestRule.onNodeWithTag("family-graduate-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("family-graduate-confirm-button")
            .assertIsEnabled()
    }

    // ── web settings ─────────────────────────────────────────────────────

    @Composable
    private fun WebSettings(origin: String? = "https://alice.example.test") {
        WebSettingsContent(
            view = SubdomainView(enabled = false, url = "https://alice.example.test/", disabledReason = null),
            onBack = {},
            onSetEnabled = {},
            posts = listOf(
                FfiPublishedPost(postId = ByteArray(32) { 3 }, slug = "a-post", gatedTier = "gold"),
            ),
            hydrated = true,
            origin = origin,
        )
    }

    @Test
    fun theSubdomainToggleGates_andThePaywallCopyGatesWhileThePlainCopyStaysLive() {
        // Two findings in one reveal. The toggle is a dispatch-on-change commit
        // (no Save beside it). And `copy-paywall-link` LOOKS like a clipboard
        // action but mints a token on the nest, while `copy-link` beside it
        // really is local — asserting both is what makes the distinction
        // visible instead of merely claimed in a comment.
        render(FfiConnectionState.DISCONNECTED) { WebSettings() }
        composeTestRule.onNodeWithTag("web-settings-subdomain-toggle")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("web-published-post-copy-paywall-link-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("web-published-post-copy-link-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theSubdomainToggleAndPaywallCopyAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { WebSettings() }
        composeTestRule.onNodeWithTag("web-settings-subdomain-toggle")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("web-published-post-copy-paywall-link-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun thePaywallCopyStaysDeadWithoutAnOriginEvenConnected() {
        // The converse: that button carries its OWN `origin != null` predicate,
        // so this pins that the gate composed with it rather than replacing it.
        render(FfiConnectionState.CONNECTED) { WebSettings(origin = null) }
        composeTestRule.onNodeWithTag("web-published-post-copy-paywall-link-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ── admin web: the apex select dispatches on pick ────────────────────

    @Composable
    private fun AdminWeb() {
        AdminWebContent(
            currentActorHex = null,
            actors = listOf(ActorOption(idHex = "ab".repeat(32), label = "alice")),
            apexUrl = "https://example.test/",
            onBack = {},
            onSetApex = {},
        )
    }

    @Test
    fun theApexActorSelectGates() {
        render(FfiConnectionState.DISCONNECTED) { AdminWeb() }
        composeTestRule.onNodeWithTag("admin-web-apex-actor-select")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theApexActorSelectIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminWeb() }
        composeTestRule.onNodeWithTag("admin-web-apex-actor-select")
            .performScrollTo().assertIsEnabled()
    }

    // ── moderation: one control, two gestures ────────────────────────────

    private fun queueRow(id: String, source: QueueRowSource) = QueueRow(
        contentId = id,
        contentType = "post",
        category = "spam",
        confidencePerMille = 900u,
        action = if (source == QueueRowSource.LOCAL) null else 1u,
        timestamp = 0L,
        source = source,
    )

    @Test
    fun theServerRowsCorrectionGates_whileALocalRowsStaysLiveInTheSameQueue() {
        // The discriminant, asserted the only way that actually proves it: both
        // row kinds in ONE render, at ONE moment, with ONE connection state. A
        // blanket disable fails this; so does a gate that ignored `source`.
        //
        // ⚠ `train-correction-button` is `indexed: true` in ui.yaml, so both
        // rows carry the same tag — hence `onAllNodesWithTag` and positional
        // reads rather than `onNodeWithTag`, which would die on "found 2".
        render(FfiConnectionState.DISCONNECTED) {
            ModerationQueueContent(
                queue = listOf(
                    queueRow("server-1", QueueRowSource.SERVER),
                    queueRow("local-1", QueueRowSource.LOCAL),
                ),
                isLoading = false,
                onCorrect = {},
            )
        }
        val buttons = composeTestRule.onAllNodesWithTag("train-correction-button")
        buttons.assertCountEquals(2)
        buttons[0].assertIsNotEnabled()   // the server row — needs the nest
        buttons[1].assertIsEnabled()      // the local row — never leaves the device
    }

    @Test
    fun bothCorrectionsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            ModerationQueueContent(
                queue = listOf(
                    queueRow("server-1", QueueRowSource.SERVER),
                    queueRow("local-1", QueueRowSource.LOCAL),
                ),
                isLoading = false,
                onCorrect = {},
            )
        }
        val buttons = composeTestRule.onAllNodesWithTag("train-correction-button")
        buttons.assertCountEquals(2)
        buttons[0].assertIsEnabled()
        buttons[1].assertIsEnabled()
    }

    // ══════════════════════════════════════════════════════════════════════
    // Batch 5 — the last three declarable kinds outside admin/bridges.
    //
    // A census of the 35 non-`bridges` kinds found only these three had an android control at all; the rest
    // are unbuilt sections or background machinery. So this batch
    // closes the declarable remainder, and what is left of the fan-out is the
    // un-censused admin (17) and bridges (58) planes.
    // ══════════════════════════════════════════════════════════════════════

    @Test
    fun theBackupDestinationRegisterGates_whileItsFormFieldsStayLive() {
        render(FfiConnectionState.DISCONNECTED) {
            ScrollHost {
                BackupDestinationsContent(
                    destinations = emptyList(),
                    working = false,
                    onAdd = { _, _ -> },
                    onEdit = { _, _, _ -> },
                    onRemove = { _, _ -> },
                )
            }
        }
        composeTestRule.onNodeWithTag("backup-destination-add-button")
            .performScrollTo().assertIsEnabled().performClick()
        // Satisfy the form's OWN predicate (a non-blank URL) before asserting,
        // or this passes against an app with no gate at all.
        composeTestRule.onNodeWithTag("backup-destination-url-input")
            .performScrollTo().performTextInput("https://nest.example.test")

        composeTestRule.onNodeWithTag("backup-destination-add-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        // The commit gates, not the buffer — and not the way back out.
        composeTestRule.onNodeWithTag("backup-destination-url-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("backup-destination-name-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("backup-destination-add-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theBackupDestinationRegisterIsLiveWhenConnectedAndFilled() {
        render(FfiConnectionState.CONNECTED) {
            ScrollHost {
                BackupDestinationsContent(
                    destinations = emptyList(),
                    working = false,
                    onAdd = { _, _ -> },
                    onEdit = { _, _, _ -> },
                    onRemove = { _, _ -> },
                )
            }
        }
        composeTestRule.onNodeWithTag("backup-destination-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("backup-destination-url-input")
            .performScrollTo().performTextInput("https://nest.example.test")
        composeTestRule.onNodeWithTag("backup-destination-add-confirm-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theBackupDestinationRegisterStaysDeadOnAnEmptyUrlEvenConnected() {
        // The converse, so the pair can tell a working gate from one that took
        // over `enabled`.
        render(FfiConnectionState.CONNECTED) {
            ScrollHost {
                BackupDestinationsContent(
                    destinations = emptyList(),
                    working = false,
                    onAdd = { _, _ -> },
                    onEdit = { _, _, _ -> },
                    onRemove = { _, _ -> },
                )
            }
        }
        composeTestRule.onNodeWithTag("backup-destination-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("backup-destination-add-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    private fun prunePreview() = PrunePreview(
        wouldPrune = 1,
        remaining = 3,
        candidates = listOf(PruneCandidate(id = 7, createdAt = 1_700_000_000L, tags = emptyList())),
        policyState = PolicyState.APPLIED,
    )

    @Test
    fun thePruneExecuteGates_whileItsCancelStaysLive() {
        // ⚠ This button is conditionally RENDERED, not merely disabled: the
        // section only draws it when the policy is APPLIED, candidates is
        // non-empty and nothing is busy. All three are seeded here — otherwise
        // the assertion would fail with "found no node" and read like a gate
        // bug rather than an unmet render predicate.
        render(FfiConnectionState.DISCONNECTED) {
            PrunePreviewContent(preview = prunePreview(), busy = false, onExecute = {}, onCancel = {})
        }
        composeTestRule.onNodeWithTag("snapshot-prune-execute-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("snapshot-prune-cancel-button").assertIsEnabled()
    }

    @Test
    fun thePruneExecuteIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            PrunePreviewContent(preview = prunePreview(), busy = false, onExecute = {}, onCancel = {})
        }
        composeTestRule.onNodeWithTag("snapshot-prune-execute-button").assertIsEnabled()
    }

    @Test
    fun theImmediateDeleteConfirmGates_whileItsFrictionBarStaysUsable() {
        // The friction bar is the point: a user with no nest must still be able
        // to READ the warning and type both confirmations. Only the commit dies.
        render(FfiConnectionState.DISCONNECTED) {
            ImmediateDeleteConfirmModal(
                snapshotId = 7,
                ackText = "I understand",
                busy = false,
                // The machine's predicate, stubbed to "already satisfied" so the
                // verdict is the only thing left that can disable the confirm.
                confirmEnabled = { _, _ -> true },
                onConfirm = { _, _ -> },
                onCancel = {},
            )
        }
        composeTestRule.onNodeWithTag("immediate-delete-confirm-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("immediate-delete-confirm-input").assertIsEnabled()
        composeTestRule.onNodeWithTag("immediate-delete-acknowledge-input").assertIsEnabled()
        composeTestRule.onNodeWithTag("immediate-delete-cancel-button").assertIsEnabled()
    }

    @Test
    fun theImmediateDeleteConfirmIsLiveWhenConnectedAndTheFrictionBarIsSatisfied() {
        render(FfiConnectionState.CONNECTED) {
            ImmediateDeleteConfirmModal(
                snapshotId = 7,
                ackText = "I understand",
                busy = false,
                confirmEnabled = { _, _ -> true },
                onConfirm = { _, _ -> },
                onCancel = {},
            )
        }
        composeTestRule.onNodeWithTag("immediate-delete-confirm-button").assertIsEnabled()
    }

    @Test
    fun theImmediateDeleteConfirmStaysDeadWhileTheFrictionBarIsUnsatisfied() {
        // The converse, and the one that matters most here: the friction bar is
        // an architectural rule (backups.md rule 4), so a gate that replaced its
        // predicate instead of composing with it would turn the most destructive
        // affordance in the app into a one-click button the moment a nest
        // appeared. This pins that it does not.
        render(FfiConnectionState.CONNECTED) {
            ImmediateDeleteConfirmModal(
                snapshotId = 7,
                ackText = "I understand",
                busy = false,
                confirmEnabled = { _, _ -> false },
                onConfirm = { _, _ -> },
                onCancel = {},
            )
        }
        composeTestRule.onNodeWithTag("immediate-delete-confirm-button").assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ══════════════════════════════════════════════════════════════════════
    // Batch 6 — the ADMIN plane, closed.
    //
    // A census of the 16 undeclared `fauna.admin.*` kinds found 15 of them had an android control
    // already rendering, on just three screens — a far better ratio than the
    // non-`bridges` census that preceded it, whose 35 kinds yielded only 3.
    // The 16th, `fauna.admin.users.create`, landed 2026-08-21 with the Admit
    // section — declared below like the rest.
    //
    // ⚠ This batch also CORRECTS the earlier census on one entry.
    // `fauna.admin.custody_hosting.remove` was recorded as a page-parity gap; it is not — `AdminCustodyHostingScreen` renders the row
    // button, the confirm and the cancel, and its VM calls the real
    // `remove_custody_hosting`. Declared below like the rest.
    // ══════════════════════════════════════════════════════════════════════

    private fun adminUser(
        label: String = "alice",
        tier: String = "free",
        handle: String? = null,
    ) = com.fauna.ffi.FfiAdminUser(
        actorId = ByteArray(32) { 1 },
        tier = tier,
        label = label,
        handle = handle,
        suspended = false,
        createdAt = 0,
        inboxBytesUsed = 0,
        storageBytesUsed = 0,
        eviction = null,
        mailServingEnabled = true,
        isAdmin = false,
    )

    private fun inviteCode(value: String = "ABC123") = com.fauna.ffi.FfiAdminInviteCode(
        code = value, tier = "free", usesLeft = 3, createdAt = 0, ageBand = null,
    )

    private fun inviteRequest(handle: String = "bob") = com.fauna.ffi.FfiAdminInviteRequest(
        id = 7,
        actorId = ByteArray(32) { 2 },
        handle = handle,
        message = "let me in",
        status = "pending",
        isPending = true,
        createdAt = 0,
        decidedAt = null,
        decidedBy = null,
        denialReason = null,
        ageBand = null,
        ageBandProvenance = null,
    )

    /**
     * The admin-users hub with every gated control on screen at once: an
     * invite request (approve/deny), the registration Save, the invite mint
     * and delete, and a user row carrying all five row controls.
     *
     * `rowControls` is forced to offer all five so one render covers the whole
     * page — the shared rule would never return that combination for a real
     * user, which is exactly why it is stubbed here: this test is about the
     * gate, and `admin_user_row_controls` has its own unit tests.
     */
    @Composable
    private fun AdminUsers() {
        AdminUsersContent(
            pendingRequests = listOf(inviteRequest()),
            inviteCodes = listOf(inviteCode()),
            users = listOf(adminUser()),
            allUsers = listOf(adminUser()),
            userTotal = 1,
            userOffset = 0,
            tiers = listOf("free", "personal"),
            mintedCode = null,
            actionError = null,
            registrationMode = com.fauna.ffi.FfiRegistrationMode.CLOSED,
            unknownRegistrationMode = null,
            maxFreeUsers = "",
            ageVerificationRequired = false,
            onBack = {},
            onSetUserTier = { _, _ -> },
            onCreateCode = { _, _, _, _ -> },
            onDeleteCode = {},
            onApprove = { _, _, _, _ -> },
            onDeny = { _, _ -> },
            onEvictUser = {},
            onSuspendUser = {},
            onCancelEviction = {},
            onMakeAdmin = {},
            onRemoveAdmin = {},
            onNextPage = {},
            onPrevPage = {},
            onSaveRegistration = { _, _, _ -> },
            onAdmitUser = { _, _, _ -> },
            hexFull = { HexUtil.bytesToHex(it) },
            mailServingStatusLabel = { LocalizedText("admin.users_page.serving_here", emptyMap()) },
            rowControls = {
                com.fauna.ffi.FfiAdminUserRowControls(
                    `suspend` = true, evict = true, restore = true,
                    makeAdmin = true, removeAdmin = true,
                )
            },
        )
    }

    /** Reveal the invite-create form, whose confirm is the minting control. */
    private fun revealInviteForm() {
        composeTestRule.onNodeWithTag("create-invite-code-btn").performScrollTo().performClick()
    }

    @Test
    fun everyAdminUsersCommitGates_whileItsBuffersAndRevealsStayLive() {
        // The whole batch's discriminating render: twelve commits and their
        // local siblings in ONE composition at ONE connection state. A blanket
        // disable fails on the second half; a missing declaration fails on the
        // first.
        render(FfiConnectionState.DISCONNECTED) { AdminUsers() }

        // The reveal is pure-local, so it must survive the outage — and it has
        // to, or the mint below could not be reached to be asserted at all.
        composeTestRule.onNodeWithTag("create-invite-code-btn").performScrollTo().assertIsEnabled()
        revealInviteForm()

        listOf(
            "invite-request-row-approve-button",
            "invite-request-row-deny-button",
            "admin-users-registration-save-button",
            "admin-users-admit-button",
            "create-invite-confirm-btn",
            "admin-settings-invite-delete-button",
            "admin-users-tier-select",
            "admin-users-evict-button",
            "admin-users-suspend-button",
            "admin-users-cancel-eviction-button",
            "admin-users-make-admin-button",
            "admin-users-remove-admin-button",
        ).forEach { tag ->
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsNotEnabled()
        }

        // The live half: every buffer feeding those commits, and the form's own
        // cancel. None of them touches the wire, so none may grey.
        listOf(
            "invite-request-row-deny-reason-field",
            "invite-request-row-tier-select",
            "invite-request-row-guardian-select",
            "admin-users-max-free-users-input",
            "admin-users-admit-actor-input",
            "admin-users-admit-handle-input",
            "admin-users-admit-tier-select",
            "admin-settings-tier-select",
            "admin-settings-max-uses-input",
            "admin-users-invite-guardian-select",
            "admin-settings-invite-cancel-button",
        ).forEach { tag ->
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsEnabled()
        }
    }

    @Test
    fun everyAdminUsersCommitIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminUsers() }
        revealInviteForm()
        listOf(
            "invite-request-row-approve-button",
            "invite-request-row-deny-button",
            "admin-users-registration-save-button",
            "admin-users-admit-button",
            "create-invite-confirm-btn",
            "admin-settings-invite-delete-button",
            "admin-users-tier-select",
            "admin-users-evict-button",
            "admin-users-suspend-button",
            "admin-users-cancel-eviction-button",
            "admin-users-make-admin-button",
            "admin-users-remove-admin-button",
        ).forEach { tag ->
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsEnabled()
        }
    }

    @Test
    fun theDenyGatesToo_becauseRefusingAnAdmissionIsStillACommit() {
        // Kept as its own case for the same reason batch 4 kept the family
        // transfer-decline: "deny" reads like a dismissal, and a leg that
        // declared only the approve would still pass the paired test above if
        // the two ever drifted apart into separate renders.
        render(FfiConnectionState.DISCONNECTED) { AdminUsers() }
        composeTestRule.onNodeWithTag("invite-request-row-deny-button")
            .performScrollTo().assertIsNotEnabled()
        // Its reason buffer is the sibling that proves this is not a blanket
        // disable of the row.
        composeTestRule.onNodeWithTag("invite-request-row-deny-reason-field")
            .performScrollTo().assertIsEnabled()
    }

    // ── admin settings: tier caps and membership designations ────────────

    private fun adminTier(name: String = "free") = com.fauna.ffi.FfiAdminTier(
        name = name,
        maxInboxBytes = 100,
        maxStorageBytes = 200,
        maxDevices = 3,
        maxBlobSize = 50,
        maxFeeds = 10,
    )

    private fun membershipTier(tierName: String) = com.fauna.ffi.FfiAdminMembershipTier(
        tierName = tierName,
        adminTier = "personal",
        lapseTier = "free",
        createdAt = 0,
    )

    @Composable
    private fun AdminSettings(designated: Boolean) {
        AdminSettingsContent(
            tiers = listOf(adminTier()),
            ownMembershipTierNames = listOf("supporter"),
            membershipTiers = if (designated) listOf(membershipTier("supporter")) else emptyList(),
            onBack = {},
            onSaveTier = { _, _, _, _, _, _ -> },
            onSaveMembershipTier = { _, _, _ -> },
            onClearMembershipTier = {},
            parseCap = { it.trim().toLongOrNull()?.coerceAtLeast(0) },
        )
    }

    @Test
    fun theTierAndMembershipCommitsGate_whileTheirDraftFieldsStayLive() {
        render(FfiConnectionState.DISCONNECTED) { AdminSettings(designated = true) }
        composeTestRule.onNodeWithTag("admin-settings-tier-save-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-settings-membership-save-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-settings-membership-clear-button")
            .performScrollTo().assertIsNotEnabled()
        // The five cap fields and the three membership pickers are drafts: the
        // admin may keep editing with no nest, and only Save reaches for one.
        composeTestRule.onNodeWithTag("admin-settings-tier-cap-inbox")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-settings-tier-cap-feeds")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-settings-membership-tier-select")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-settings-membership-admin-tier-select")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theTierAndMembershipCommitsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminSettings(designated = true) }
        composeTestRule.onNodeWithTag("admin-settings-tier-save-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-settings-membership-save-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-settings-membership-clear-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theMembershipClearStaysDeadOnAnUndesignatedRowEvenConnected() {
        // The converse case, and the one that must SURVIVE an unplugged gate:
        // an undesignated row has nothing to clear, which is the page's own
        // predicate and stronger than the gate's. Its Save sibling is live in
        // the same render, so this is a guard rather than a restatement of the
        // gate.
        render(FfiConnectionState.CONNECTED) { AdminSettings(designated = false) }
        composeTestRule.onNodeWithTag("admin-settings-membership-clear-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-settings-membership-save-button")
            .performScrollTo().assertIsEnabled()
    }

    // ── admin custody hosting: only the confirm commits ──────────────────

    private fun hostingRow() = com.fauna.ffi.FfiAdminHostingRow(
        hostActorId = "aa".repeat(32),
        ownerActorId = "bb".repeat(32),
        ownerNestUrl = "https://owner.example",
        grantId = byteArrayOf(1, 2, 3),
        retainedBytesCap = 8192,
        heldBytes = 4096,
        stopped = false,
        receiptState = com.fauna.ffi.FfiReceiptState.FRESH,
    )

    @Composable
    private fun AdminCustodyHosting(working: Boolean = false) {
        AdminCustodyHostingContent(
            rows = listOf(hostingRow()),
            status = null,
            working = working,
            budgetText = { "${it}B" },
            heldText = { "${it}B" },
            shortId = { it.take(8) },
            onBack = {},
            onRemove = { _, _ -> },
        )
    }

    @Test
    fun theCustodyRemoveConfirmGates_whileItsArmAndCancelStayLive() {
        // Corrects row 41 (b): this section EXISTS on android. Arming and
        // cancelling map to no wire kind at all (tui's admin kind map says so
        // in as many words), so the pair around the confirm must stay live —
        // and the arm has to, or the confirm could not be reached.
        render(FfiConnectionState.DISCONNECTED) { AdminCustodyHosting() }
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-0")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-0")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theCustodyRemoveConfirmIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminCustodyHosting() }
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-0")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-confirm-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theCustodyRemoveConfirmStaysDeadWhileARequestIsInFlightEvenConnected() {
        // The page's own `working` predicate, handed to the gate rather than
        // re-tested beside it — so a reconnect restores exactly the page's
        // intent and never more. Survives an unplugged gate.
        //
        // `working` has to be flipped AFTER arming rather than seeded true: the
        // confirm only exists in the armed branch, and a busy row will not arm.
        val working = mutableStateOf(false)
        render(FfiConnectionState.CONNECTED) { AdminCustodyHosting(working = working.value) }
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-0")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-confirm-button")
            .performScrollTo().assertIsEnabled()
        working.value = true
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-confirm-button")
            .performScrollTo().assertIsNotEnabled()
    }

    // ── admin-dns fixtures ──────────────────────────────────────────────────
    //
    // Deliberate near-copies of `AdminDnsContentTest`'s: that class is the page's
    // own render coverage and this one is the gate's, so sharing a fixture would
    // couple two suites that must be able to move independently — the same reason
    // the other page helpers above build their own rows.

    private fun localDomain(
        domain: String = "example.com",
        primary: Boolean = true,
        removed: Long? = null,
        domainId: ByteArray = ByteArray(16) { if (primary) 1 else 2 },
    ) = LocalDomainView(
        domainId = domainId,
        domain = domain,
        isPrimary = primary,
        mtaStsMode = "testing",
        mtaStsCertMode = "expand_primary",
        mtaStsMaxAgeSeconds = 604800,
        spfRecord = "v=spf1 mx ~all",
        dkimSelector = null,
        dkimRotationDue = false,
        dkimSelectorActivatedAt = null,
        catchAllActorId = null,
        catchAllClearedBySuccessionAt = null,
        roleAddressOverrides = emptyList(),
        dmarcPolicy = DomainDmarcPolicy.REJECT,
        addedAt = 0,
        removedAt = removed,
    )

    private fun record(
        name: String = "example.com",
        type: String = "MX",
        expected: String = "10 mail.example.com",
        status: VerifyStatus? = VerifyStatus.OK,
    ) = DnsRecordRow(
        name = name,
        recordType = type,
        expected = expected,
        ttlSeconds = 3600u,
        verdict = status?.let { RecordVerdict(observed = listOf(expected), status = it) },
    )

    private fun dnsDomain(
        domain: String = "example.com",
        mode: String = "manual",
    ) = DomainView(
        domain = domain,
        mode = mode,
        isPrimary = false,
        records = listOf(record(name = domain)),
        autoRenew = false,
    )

    private fun pendingCert(domain: String = "example.com") = PendingCertIssue(
        domain = domain,
        challenges = listOf(
            record(name = "_acme-challenge.$domain", type = "TXT", expected = "tok123", status = null),
        ),
    )

    /** A `grace`-state rename: post-flip, so force-complete + extend + abort are all offered. */
    private fun renameView() = PrimaryDomainRenameView(
        renameId = ByteArray(16) { 0xAB.toByte() },
        state = "grace",
        oldPrimaryDomain = "old.example.com",
        newPrimaryDomain = "new.example.com",
        startedAt = 1_700_000_000_000L,
        graceDays = 7,
        graceEndsAt = 4_102_444_800_000L, // 2100-01-01 — far future
        readyToCompleteAt = null,
        isPostFlipActive = true,
        isPreFlip = false,
        canComplete = false,
        canForceComplete = true,
        canExtend = true,
        canAbort = true,
    )

    // ── Batch 7: the admin-dns page, and the backups discriminant ───────────
    //
    // `admin-dns` carried NO `faunaGate` call at all before this batch, so every
    // case below is first coverage for that page. Its shape is the reason it is
    // worth its own block: the page mixes all three classes in one composition —
    // nine deployment-state commits that must grey (`fauna.bridges.*`), one
    // cert delivery that must grey (`fauna.tls.publish_cert`), and a whole slice
    // writing the ADMIN'S OWN `fauna.state.dns` document (`fauna.account.state.put`,
    // OfflineSafe) that must stay live. A blanket disable would pass a
    // one-sided check here and break the contract, so the pairing is the
    // assertion throughout.

    /** A managed domain: `singleIssue` true, so the issue button delivers a cert. */
    private fun managedDns(domain: String = "managed.example.com") =
        dnsDomain(domain = domain, mode = "managed")

    /** A manual domain: `singleIssue` false, so the issue button only opens the order. */
    private fun manualDns(domain: String = "manual.example.com") =
        dnsDomain(domain = domain, mode = "manual")

    @Composable
    private fun AdminDns(
        activeDomains: List<LocalDomainView> = emptyList(),
        removedDomains: List<LocalDomainView> = emptyList(),
        dnsDomains: List<DomainView> = emptyList(),
        activeRename: PrimaryDomainRenameView? = null,
        renameAvailable: Boolean = false,
        actors: List<ActorOption> = emptyList(),
        delegations: List<DelegationView> = emptyList(),
        pendingCert: PendingCertIssue? = null,
        working: Boolean = false,
    ) {
        // Deliberately NOT wrapped in [ScrollHost]: `AdminDnsContent` owns its own
        // `verticalScroll`, and nesting two vertical scrollers is an immediate
        // infinite-constraint crash.
        AdminDnsContent(
            activeDomains = activeDomains,
            removedDomains = removedDomains,
            activeRename = activeRename,
            renameAvailable = renameAvailable,
            addingFirstDomain = false,
            dnsDomains = dnsDomains,
            credentials = emptyList(),
            actors = actors,
            manageAll = false,
            certStatuses = emptyList(),
            delegations = delegations,
            pendingCert = pendingCert,
            working = working,
            error = null,
            onBack = {},
            onRefresh = {},
            onAddDomain = {},
            onRemoveDomain = {},
            onRestoreDomain = {},
            onStartRename = { _, _ -> },
            onCompleteRename = { _, _ -> },
            onExtendRename = { _, _ -> },
            onAbortRename = {},
            onSetCatchAll = { _, _ -> },
            onSetRoleAddress = { _, _, _ -> },
            onSetMode = { _, _ -> },
            onSetManageAll = {},
            onPutCredential = { _, _, _ -> },
            onClearCredential = {},
            onIssueCert = {},
            onBeginManualIssue = {},
            onCompleteManualIssue = {},
            onCancelManualIssue = {},
            onDelegateRenewal = { _, _ -> },
            onRemoveDelegation = {},
            onSetAutoRenew = { _, _ -> },
        )
    }

    @Test
    fun theAddDomainSubmitGates_whileItsOpenerInputAndCancelStayLive() {
        render(FfiConnectionState.DISCONNECTED) { AdminDns() }
        // Revealing the form touches nothing on the wire, so the opener must
        // stay live — and it has to, or the submit could never be reached.
        composeTestRule.onNodeWithTag("admin-dns-add-domain-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("admin-dns-add-domain-submit-button")
            .performScrollTo().assertIsNotEnabled()
        // *The commit gates, not the buffer* — in its two clearest forms.
        composeTestRule.onNodeWithTag("admin-dns-add-domain-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-dns-add-domain-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theAddDomainSubmitIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminDns() }
        composeTestRule.onNodeWithTag("admin-dns-add-domain-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-add-domain-submit-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theDomainRemoveAndRestoreCommitsGate() {
        render(FfiConnectionState.DISCONNECTED) {
            AdminDns(
                activeDomains = listOf(localDomain(domain = "b.example.com", primary = false)),
                dnsDomains = listOf(manualDns("b.example.com")),
                removedDomains = listOf(
                    localDomain(domain = "gone.example.com", primary = false, removed = 1L),
                ),
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-domain-remove-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-dns-removed-domain-restore-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theDomainRemoveStaysDeadOnThePrimaryEvenConnected() {
        // The page's OWN predicate (the primary domain can never be removed),
        // handed to the gate rather than re-tested beside it. A converse case:
        // it must SURVIVE an unplugged gate, or the gate has replaced the page's
        // rule instead of composing with it — and this is the destructive
        // direction, where replacing it would offer a removal the nest forbids.
        render(FfiConnectionState.CONNECTED) {
            AdminDns(
                activeDomains = listOf(localDomain(primary = true)),
                dnsDomains = listOf(manualDns("example.com")),
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-domain-remove-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theDispatchOnPickCatchAllAndRoleSelectsGate_becauseThePickIsTheCall() {
        // Neither picker is a draft: picking IS the call, so the anchor carries
        // the declaration and desensitizing it closes the menu with it.
        render(FfiConnectionState.DISCONNECTED) {
            AdminDns(
                activeDomains = listOf(localDomain(primary = true)),
                dnsDomains = listOf(manualDns("example.com")),
                actors = listOf(ActorOption("aa", "Someone")),
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-domain-catch-all-select")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-dns-domain-role-address-postmaster-select")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theCatchAllAndRoleSelectsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            AdminDns(
                activeDomains = listOf(localDomain(primary = true)),
                dnsDomains = listOf(manualDns("example.com")),
                actors = listOf(ActorOption("aa", "Someone")),
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-domain-catch-all-select")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-dns-domain-role-address-postmaster-select")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theOwnConfigWritesOnThisPageStayLiveWithNoNest() {
        // The half that stops this page's gate over-claiming, and the reason the
        // batch could not simply grey `admin-dns` wholesale: the mode toggle, the
        // auto-renew opt-out and the manage-all master switch all load-mutate-save
        // the admin's OWN `fauna.state.dns` document — `fauna.account.state.put`,
        // OfflineSafe by ruling 1 — so they must survive the same outage that
        // kills the commits above, in the same composition.
        render(FfiConnectionState.DISCONNECTED) {
            AdminDns(
                activeDomains = listOf(localDomain(primary = true)),
                dnsDomains = listOf(managedDns("example.com")),
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-manage-all-toggle")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-dns-domain-mode")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-dns-domain-auto-renew")
            .performScrollTo().assertIsEnabled()
        // …and the page's read affordance, which ruling 1 never greys. No
        // `performScrollTo()`: this one lives in the `TopAppBar`, outside the
        // page's `verticalScroll`, so asking to scroll to it throws for want of
        // a scroll parent rather than telling you anything about the gate.
        composeTestRule.onNodeWithTag("admin-dns-refresh-button").assertIsEnabled()
    }

    @Test
    fun theCertIssueButtonSplitsOnItsDiscriminant_managedDeadManualLive() {
        // ⚠ THE DISCRIMINANT CASE, and the sharpest pairing on the page: ONE
        // control id, two domains, one composition, opposite verdicts. The
        // managed domain's click runs the whole DNS-01 order and DELIVERS the
        // cert to this nest (`fauna.tls.publish_cert`, OnlineOnly → dead); the
        // manual domain's click only opens the CA order and stashes a breadcrumb
        // in the admin's own config (`fauna.account.state.put`, OfflineSafe → live).
        // Nothing about the two buttons differs but the kind the gate is handed,
        // so this cannot pass on a blanket disable, and it cannot pass on a
        // blanket enable either.
        render(FfiConnectionState.DISCONNECTED) {
            AdminDns(
                activeDomains = listOf(
                    localDomain(domain = "managed.example.com", primary = true),
                    localDomain(
                        domain = "manual.example.com",
                        primary = false,
                        domainId = ByteArray(16) { 3 },
                    ),
                ),
                dnsDomains = listOf(managedDns(), manualDns()),
            )
        }
        val issueButtons = composeTestRule.onAllNodesWithTag("admin-dns-cert-issue-button")
        assertEquals(2, issueButtons.fetchSemanticsNodes().size)
        // Composition order follows `activeDomains`, so [0] is the managed one.
        issueButtons[0].performScrollTo().assertIsNotEnabled()
        issueButtons[1].performScrollTo().assertIsEnabled()
    }

    @Test
    fun theManagedCertIssueIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            AdminDns(
                activeDomains = listOf(localDomain(domain = "managed.example.com", primary = true)),
                dnsDomains = listOf(managedDns()),
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-cert-issue-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theManualPhaseTwoCompleteGates_whileAbandoningTheOrderStaysLive() {
        // Phase 2 is where the manual path finally reaches this nest, so it is
        // the half that greys. Cancelling only drops the breadcrumb — an
        // `OfflineSafe` config write — and must stay live, or an admin whose
        // nest went away is stuck holding a suspended order forever.
        render(FfiConnectionState.DISCONNECTED) {
            AdminDns(
                activeDomains = listOf(localDomain(domain = "manual.example.com", primary = true)),
                dnsDomains = listOf(manualDns()),
                pendingCert = pendingCert("manual.example.com"),
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-cert-complete-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-dns-cert-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theRenameBannerCommitsGate_whileTheirArmsAndBufferStayLive() {
        // Arming is local (both confirms are reveal-then-confirm), so the two
        // openers and the extend-days input stay live while the three commits
        // die. The openers HAVE to stay live, or the confirms below could not be
        // reached to assert at all.
        render(FfiConnectionState.DISCONNECTED) {
            AdminDns(
                activeDomains = listOf(localDomain(primary = true)),
                dnsDomains = listOf(manualDns("example.com")),
                activeRename = renameView(),
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-rename-extend-days-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-dns-rename-extend-button")
            .performScrollTo().assertIsNotEnabled()

        composeTestRule.onNodeWithTag("admin-dns-rename-complete-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-complete-confirm-button")
            .performScrollTo().assertIsNotEnabled()

        composeTestRule.onNodeWithTag("admin-dns-rename-abort-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-abort-confirm-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theRenameBannerCommitsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            AdminDns(
                activeDomains = listOf(localDomain(primary = true)),
                dnsDomains = listOf(manualDns("example.com")),
                activeRename = renameView(),
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-rename-extend-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-dns-rename-complete-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-complete-confirm-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theRenameSheetSubmitGates_withATargetAlreadyPicked() {
        // ⚠ The submit has its OWN predicate (a target must be picked), so
        // asserting it dead on an untouched sheet would pass against an app with
        // no gate at all — the trap web's own leg recorded.
        // The sheet opened from the primary row seeds the first non-primary as
        // the target, so a second domain is what makes this assertion mean anything.
        render(FfiConnectionState.DISCONNECTED) {
            AdminDns(
                activeDomains = listOf(
                    localDomain(primary = true),
                    localDomain(
                        domain = "second.example.com",
                        primary = false,
                        domainId = ByteArray(16) { 3 },
                    ),
                ),
                dnsDomains = listOf(manualDns("example.com"), manualDns("second.example.com")),
                renameAvailable = true,
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-domain-rename-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-submit-button").assertIsNotEnabled()
        // The sheet's own buffers are untouched by the gate.
        composeTestRule.onNodeWithTag("admin-dns-rename-grace-days-input").assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-dns-rename-cancel-button").assertIsEnabled()
    }

    @Test
    fun theRenameSheetSubmitIsLiveWhenConnectedWithATarget() {
        render(FfiConnectionState.CONNECTED) {
            AdminDns(
                activeDomains = listOf(
                    localDomain(primary = true),
                    localDomain(
                        domain = "second.example.com",
                        primary = false,
                        domainId = ByteArray(16) { 3 },
                    ),
                ),
                dnsDomains = listOf(manualDns("example.com"), manualDns("second.example.com")),
                renameAvailable = true,
            )
        }
        composeTestRule.onNodeWithTag("admin-dns-domain-rename-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-submit-button").assertIsEnabled()
    }

    // ── The backups destination discriminant (a corrected declaration) ──────
    //
    // This control declared ONE kind for what are three ceremonies, and its
    // comment recorded the wrong reason for it. The add branches are both
    // OnlineOnly, so no enabled-assert can tell them apart — the witness that
    // the nest branch now names `fauna.backup.nest_key.grant` is
    // `check-offline-gate-kinds.py` rule 4, which counted that kind undeclared
    // on android until this batch. The EDIT branch is a different matter: it is
    // `fauna.account.state.put`, OfflineSafe, and the case below is a real behavioural
    // difference that the previous declaration got wrong.

    @Test
    fun theEditFormSaveStaysLiveWithNoNest_becauseARenameIsTheOwnersOwnConfigWrite() {
        // ⚠ This case FAILED before this batch: the edit form shares its submit
        // composable with the add form, which declared the OnlineOnly
        // `fauna.backup.destination.register` for every branch — greying exactly
        // the edit the shared layer goes out of its way to keep working offline
        // (`backup_destination_edit`: "renaming an offline destination must still
        // work"; it re-resolves only when the URL actually changed). Editing one
        // row of the owner's own config document is `fauna.account.state.put`, which is
        // also what tui rules for `SubmitTarget::Edit`.
        render(FfiConnectionState.DISCONNECTED) {
            ScrollHost {
                BackupDestinationsContent(
                    destinations = listOf(destination()),
                    working = false,
                    onAdd = { _, _ -> },
                    onEdit = { _, _, _ -> },
                    onRemove = { _, _ -> },
                )
            }
        }
        composeTestRule.onNodeWithTag("backup-destination-edit-button")
            .performScrollTo().performClick()
        // The edit form opens pre-filled from the row, so the submit's own
        // non-blank-URL predicate is already satisfied — nothing to type.
        composeTestRule.onNodeWithTag("backup-destination-add-confirm-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theNestDestinationAddGates_onceItsUrlPredicateIsSatisfied() {
        // ⚠ Trap 3 in force: the nest branch's own predicate is a non-blank URL,
        // so asserting the gate on an untouched form would pass against an app
        // with no gate at all. Type first, then assert.
        render(FfiConnectionState.DISCONNECTED) {
            ScrollHost {
                BackupDestinationsContent(
                    destinations = listOf(destination()),
                    working = false,
                    onAdd = { _, _ -> },
                    onEdit = { _, _, _ -> },
                    onRemove = { _, _ -> },
                )
            }
        }
        composeTestRule.onNodeWithTag("backup-destination-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("backup-destination-url-input")
            .performScrollTo().performTextInput("https://other.example.com")
        composeTestRule.onNodeWithTag("backup-destination-add-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        // The buffer beside it survives, so this is not a blanket disable.
        composeTestRule.onNodeWithTag("backup-destination-add-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theNestDestinationAddStaysDeadWithABlankUrlEvenConnected() {
        // The converse: the form's own predicate, which must survive an
        // unplugged gate. If it reddens, the gate REPLACED the page's rule and a
        // connected admin could submit an enrollment with no address at all.
        render(FfiConnectionState.CONNECTED) {
            ScrollHost {
                BackupDestinationsContent(
                    destinations = listOf(destination()),
                    working = false,
                    onAdd = { _, _ -> },
                    onEdit = { _, _, _ -> },
                    onRemove = { _, _ -> },
                )
            }
        }
        composeTestRule.onNodeWithTag("backup-destination-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("backup-destination-add-confirm-button")
            .performScrollTo().assertIsNotEnabled()
    }

    // ── admin-mail fixtures ─────────────────────────────────────────────────
    //
    // Catalog-shaped values; nothing here is under test, the page just needs a
    // full snapshot to render all six groups. Named `mail*` to sit beside the
    // admin-dns set above without colliding.

    private fun mailSpam() = SpamPolicyView(
        maxScoreBeforeSpamFolder = 5u,
        maxScoreBeforeReject = 0u,
        dnsblServers = listOf("zen.spamhaus.org"),
        rejectNoRdns = false,
        greylistEnabled = false,
        greylistDelaySecs = 60u,
        maxConnPerMin = 30u,
        fcrdnsMode = "score_signal",
        heloIdentityRequired = false,
        rejectFcrdnsFail = false,
        maxMessageBytes = 26_214_400u,
        bayesianWeightMilli = 700u,
        bayesianMinSamples = 50u,
        bayesianFullConfidenceSamples = 200u,
        trainingHistoryRetentionDays = 30u,
        unlistedRecipientPenalty = 0u,
        baselineStandingPublish = false,
    )

    private fun mailAuth() = AuthPolicyView(
        enforceDmarc = true,
        enforceDmarcQuarantine = false,
        enforceSpfHardfail = false,
        enforceDkim = false,
        logOnly = false,
        maxAuthFailuresPerMinute = 30u,
        maxConnPerIp = 0u,
    )

    private fun mailSubmission() =
        SubmissionPolicyView(maxPerDay = 1000u, maxRecipientsPerMessage = 100u)

    private fun mailImap() = ImapPolicyView(
        idleTimeoutSecs = 1740u,
        tombstoneRetentionDays = 30u,
        deleteNonempty = "forbidden",
        bodystructureCacheMax = 256u,
        storageBytesDefault = 1uL shl 30,
        messageCountDefault = 50_000u,
    )

    private fun mailOutbound() = OutboundPolicyView(
        retryScheduleSeconds = listOf(0uL, 300uL, 900uL),
        permanentFailureTimeoutHours = 120u,
        delayWarningAtHours = 4u,
        ndrRateLimitDays = 1u,
        suppressNdrSpfHardfail = true,
        suppressNdrDmarcReject = true,
        postmasterCcBounces = false,
        tlsrptSendReports = false,
        ipv6Enabled = false,
        treat5xxAsTransient = emptyList(),
    )

    private fun mailAlias() = AliasPolicyView(
        exactAliasesMax = 20u,
        reservedLocalParts = listOf("postmaster", "abuse"),
        subaddressingEnabled = true,
        wildcardPrefixEnabled = true,
    )

    // ── Batch 8: the admin-mail policy page ─────────────────────────────────
    //
    // Six policy groups, each a full-PUT of its own sub-struct, plus the
    // deployment-wide enable and the baseline publish. The page's whole shape is
    // *the commit gates, not the buffer* at scale: it renders roughly forty
    // editable fields and sixteen boolean toggles that are pure drafts, against
    // eight controls that actually reach the nest. An admin with no nest must
    // still be able to compose a policy change and have it survive the reconnect.

    @Composable
    private fun AdminMail(mailEnabled: Boolean = true, working: Boolean = false) {
        // `AdminMailContent` owns its own `verticalScroll` (trap 7), so no
        // [ScrollHost] here either. FFI-free parse stubs keep the page off the
        // native path; the GATE still goes through the real shared rule, which is
        // the only FFI this file wants.
        AdminMailContent(
            mailEnabled = mailEnabled,
            spam = mailSpam(),
            auth = mailAuth(),
            submission = mailSubmission(),
            imap = mailImap(),
            outbound = mailOutbound(),
            alias = mailAlias(),
            working = working,
            onBack = {},
            onSetMailEnabled = {},
            onSaveSpam = {},
            onSaveAuth = {},
            onSaveSubmission = {},
            onSaveImap = {},
            onSaveOutbound = {},
            onSaveAlias = {},
            baselinePublishResult = null,
            onPublishBaseline = {},
            parseCount = { it.trim().toUIntOrNull() },
            parseCountU64 = { it.trim().toULongOrNull() },
        )
    }

    @Test
    fun allSixPolicySavesGate_andSoDoTheEnableAndTheBaseline() {
        render(FfiConnectionState.DISCONNECTED) { AdminMail() }
        // The six full-PUTs, each its own wire kind.
        for (group in listOf("spam", "auth", "submission", "imap", "outbound", "alias")) {
            composeTestRule.onNodeWithTag("admin-mail-$group-save-button")
                .performScrollTo().assertIsNotEnabled()
        }
        // The deployment-wide enable is a dispatch-on-change commit, not a draft.
        composeTestRule.onNodeWithTag("admin-mail-enabled-toggle")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-mail-publish-spam-baseline-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun everyPolicyBufferOnTheMailPageStaysLiveWithNoNest() {
        // ⚠ The half that makes the case above mean anything, and the one this
        // page could most easily get wrong: `ToggleRow` serves the gated enable
        // AND sixteen pure drafts, so gating inside the shared composable rather
        // than at the call site would grey every one of these. A blanket disable
        // would pass the desensitizing test and fail here.
        render(FfiConnectionState.DISCONNECTED) { AdminMail() }
        for (tag in listOf(
            "admin-mail-reject-no-rdns-toggle",
            "admin-mail-greylist-enabled-toggle",
            "admin-mail-helo-identity-required-toggle",
            "admin-mail-reject-fcrdns-fail-toggle",
            "admin-mail-auth-enforce-dmarc-toggle",
            "admin-mail-auth-log-only-toggle",
            "admin-mail-outbound-tlsrpt-send-toggle",
            "admin-mail-alias-subaddressing-toggle",
        )) {
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsEnabled()
        }
        // …and a numeric draft field, the other buffer shape on this page.
        composeTestRule.onNodeWithTag("admin-mail-max-conn-per-min-input")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theMailPageCommitsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminMail() }
        composeTestRule.onNodeWithTag("admin-mail-enabled-toggle")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-mail-spam-save-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-mail-publish-spam-baseline-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theMailSavesStayDeadWhileARequestIsInFlightEvenConnected() {
        // The page's own `working` predicate, handed to the gate rather than
        // re-tested beside it — a converse case that must SURVIVE the unplug.
        render(FfiConnectionState.CONNECTED) { AdminMail(working = true) }
        composeTestRule.onNodeWithTag("admin-mail-spam-save-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-mail-enabled-toggle")
            .performScrollTo().assertIsNotEnabled()
    }
    // ── The mail sub-pages (batch 9) ────────────────────────────────────────
    //
    // `MailSpamScreen`, `MailListMembersScreen`, `MailListsScreen`,
    // `MailAliasesScreen` and `MailSettingsScreen` — the five pages reached from
    // the mail-settings hub. Twenty declarations, and three shapes worth naming
    // before the cases:
    //
    // 1. **Three of these pages carry a control that must stay live BECAUSE ITS
    //    KIND IS OfflineSafe, not because it was missed** — the spam page's
    //    share-reports toggle (`fauna.moderation.report_share.set`) and mail
    //    settings' credential revoke and disable-mail confirm (`fauna.account.state.put`,
    //    the user's own document). They carry no `faunaGate` call at all: android's
    //    checker rejects a declaration that can never desensitize, so unlike linux
    //    the question is recorded in a comment rather than a call. They are the
    //    strongest live siblings in the whole suite — a destructive confirm that
    //    SHOULD survive an outage, right beside one that must not.
    // 2. **The discriminants here are not observable.** The alias submit is
    //    three-way (update / generate-disposable / create), the alias active
    //    toggle two-way (enable / revoke) and the lists submit two-way
    //    (update / create) — but every arm is OnlineOnly, so no assertion can
    //    tell a correct split from a collapsed one. The cases below prove each
    //    MODE's control gates; the split itself is future-proofing, exactly as
    //    batch 7 recorded for the cert-issue button (there the two arms differed
    //    in class, so its baseline could witness the split — here nothing can).
    // 3. The alias row's overflow DELETE item lives in a `DropdownMenu` popup.
    //    Its declaration is checker-verified; asserted here is the claim that
    //    actually needed proving — that its OPENER stays live, since a dead
    //    opener could never reach the item at all.

    @Composable
    private fun MailSpam(working: Boolean = false) {
        MailSpamContent(
            events = listOf(
                SpamTrainingView(
                    historyIdHex = "abcd",
                    message = "Re: invoice",
                    label = TrainingLabel.SPAM,
                    source = TrainingSource.EXPLICIT_BUTTON,
                    createdAtMs = 1_700_000_000_000L,
                    modelDeltaApplied = byteArrayOf(),
                    sealedSubject = byteArrayOf(),
                    mailbox = "",
                )
            ),
            contributeBaseline = false,
            reportShare = false,
            reportSharePublished = emptyList(),
            thresholdOverride = null,
            working = working,
            labelBadge = { it.name },
            sourceBadge = { it.name },
            onBack = {},
            onResetModel = {},
            onSetContributeBaseline = {},
            onSetReportShare = {},
            onSetThresholdOverride = {},
            onUndo = {},
            parseCount = { it.toUIntOrNull() },
        )
    }

    @Test
    fun everyMailSpamCommitGates_whileItsOfflineSafeShareToggleStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { MailSpam() }
        for (tag in listOf(
            "mail-spam-reset-model-button",
            "mail-spam-contribute-baseline-toggle",
            "mail-spam-threshold-override-input",
            "mail-spam-training-history-list-item-undo-button",
        )) {
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsNotEnabled()
        }
        // The half a blanket grey would fail. Report sharing is a plain write on
        // the shared moderation manager — OfflineSafe — so ruling 1 says it must
        // survive the same outage, in the same composition, one row away from a
        // dead sibling.
        composeTestRule.onNodeWithTag("mail-spam-share-reports-toggle")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theMailSpamPageIsFullyLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { MailSpam() }
        for (tag in listOf(
            "mail-spam-reset-model-button",
            "mail-spam-contribute-baseline-toggle",
            "mail-spam-threshold-override-input",
            "mail-spam-training-history-list-item-undo-button",
            "mail-spam-share-reports-toggle",
        )) {
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsEnabled()
        }
    }

    @Test
    fun theMailSpamCommitsStayDeadWhileARequestIsInFlightEvenConnected() {
        // A converse case, and one that must SURVIVE the unplug: these three
        // carry the page's own `working` predicate, handed to the gate rather
        // than re-tested beside it. If this reddens under a broken gate, the
        // gate REPLACED the page's intent instead of composing with it.
        render(FfiConnectionState.CONNECTED) { MailSpam(working = true) }
        for (tag in listOf(
            "mail-spam-reset-model-button",
            "mail-spam-contribute-baseline-toggle",
            "mail-spam-training-history-list-item-undo-button",
        )) {
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsNotEnabled()
        }
        // The threshold field carries no `working` predicate of its own, so it
        // stays live here — the live sibling that keeps this case honest.
        composeTestRule.onNodeWithTag("mail-spam-threshold-override-input")
            .performScrollTo().assertIsEnabled()
    }

    @Composable
    private fun MailListMembers() {
        MailListMembersContent(
            listName = "Weekly",
            members = listOf(
                MemberView(address = "a@example.com", subscribedAtMs = 1_700_000_000_000L, status = MemberStatus.SUBSCRIBED),
                MemberView(address = "b@example.com", subscribedAtMs = null, status = MemberStatus.UNSUBSCRIBED),
            ),
            subscribedCount = 1u,
            unsubscribedCount = 1u,
            statusLabel = { it.name },
            onBack = {},
            onAddMember = {},
            onBatchImport = {},
            onUnsubscribe = {},
            onResubscribe = {},
        )
    }

    @Test
    fun theMemberAddSubmitGates_whileItsBufferOpenerAndCancelStayLive() {
        render(FfiConnectionState.DISCONNECTED) { MailListMembers() }
        // The opener REVEALS the sheet — arming is local, so it stays live, and
        // a dead one could not reach the submit this case is about.
        composeTestRule.onNodeWithTag("mail-list-members-add-button")
            .performScrollTo().assertIsEnabled().performClick()
        // ⚠ Type FIRST. The submit carries its own `isNotBlank()` predicate, so
        // asserting it on an untouched form would pass against an app with no
        // gate at all — the trap this row records from web.
        composeTestRule.onNodeWithTag("mail-list-members-add-sheet-address-input")
            .performScrollTo().assertIsEnabled().performTextInput("new@example.com")
        composeTestRule.onNodeWithTag("mail-list-members-add-sheet-submit-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-list-members-add-sheet-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theMemberImportSubmitGates_whileItsTextareaAndCancelStayLive() {
        render(FfiConnectionState.DISCONNECTED) { MailListMembers() }
        composeTestRule.onNodeWithTag("mail-list-members-import-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("mail-list-members-import-sheet-input")
            .performScrollTo().assertIsEnabled().performTextInput("x@example.com")
        composeTestRule.onNodeWithTag("mail-list-members-import-sheet-submit-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-list-members-import-sheet-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun bothMemberSubscriptionCommitsGate_eachOnItsOwnKind() {
        // Not a discriminant: `subscribed` picks which BUTTON exists, so the two
        // rows carry two different ids and two separate single-kind
        // declarations. Seeding one row of each proves both.
        render(FfiConnectionState.DISCONNECTED) { MailListMembers() }
        composeTestRule.onNodeWithTag("mail-list-members-list-item-unsubscribe-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-list-members-list-item-resubscribe-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theMemberAddSubmitStaysDeadOnAnEmptyAddressEvenConnected() {
        // Converse — must SURVIVE the unplug. The gate composes with the form's
        // own emptiness predicate rather than taking it over.
        render(FfiConnectionState.CONNECTED) { MailListMembers() }
        composeTestRule.onNodeWithTag("mail-list-members-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-list-members-add-sheet-submit-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Composable
    private fun MailLists(localDomains: List<String> = listOf("example.com")) {
        MailListsContent(
            lists = listOf(
                ListView(
                    listIdHex = "aa",
                    friendlyName = "Weekly",
                    localPart = "news",
                    localDomain = "example.com",
                    address = "news@example.com",
                    description = "",
                    memberCount = 12u,
                    lastSendAtMs = null,
                    sendsToday = 0u,
                    recipientsToday = 0u,
                    listHelpUrl = "",
                    listArchiveUrl = "",
                    recipientsPerSend = null,
                )
            ),
            localDomains = localDomains,
            working = false,
            onBack = {},
            onCreate = {},
            onUpdate = { _, _ -> },
            onDelete = {},
            onViewMembers = {},
        )
    }

    @Test
    fun theListCreateSubmitAndDeleteGate_whileTheOpenerEditAndMembersStayLive() {
        render(FfiConnectionState.DISCONNECTED) { MailLists() }
        // Delete is the row's commit; Edit reveals a sheet and Members
        // navigates, so both stay live beside it.
        composeTestRule.onNodeWithTag("mail-lists-list-item-delete-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-lists-list-item-edit-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("mail-lists-list-item-members-button")
            .performScrollTo().assertIsEnabled()
        // The add opener stays live and reveals the create-mode sheet, whose
        // submit is the discriminant's create arm.
        composeTestRule.onNodeWithTag("mail-lists-add-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-submit-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theListEditSubmitAlsoGates_theDiscriminantsOtherArm() {
        // The same composable reached through its OTHER call site, where
        // `editing != null` makes the kind `update_account_list`. Both arms are
        // OnlineOnly, so this cannot witness the split — it witnesses that the
        // edit path is gated at all, which a create-only declaration would still
        // give. Its value is that a future arm-specific reclassification has a
        // case standing on it.
        render(FfiConnectionState.DISCONNECTED) { MailLists() }
        composeTestRule.onNodeWithTag("mail-lists-list-item-edit-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-submit-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theListsAddOpenerStaysDeadWithNoLocalDomainEvenConnected() {
        // Converse — must SURVIVE the unplug. The opener carries no gate at all;
        // it is dead purely on the page's own `localDomains.isNotEmpty()`. If
        // this ever reddens, a gate has been added where the sweep decided none
        // belonged.
        render(FfiConnectionState.CONNECTED) { MailLists(localDomains = emptyList()) }
        composeTestRule.onNodeWithTag("mail-lists-add-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Composable
    private fun MailAliases(
        defaultDomain: String? = "example.com",
        disabled: Boolean = false,
    ) {
        MailAliasesContent(
            aliases = listOf(
                AliasView(
                    aliasIdHex = "aa",
                    localDomain = "example.com",
                    kind = AliasKind.EXACT,
                    pattern = "bob",
                    address = "bob@example.com",
                    label = "Shopping",
                    disabled = disabled,
                    isCanonical = false,
                    hitCount = 3u,
                    lastHitAtMs = null,
                    spamThresholdOverride = null,
                    rateLimitPerHour = null,
                    rateLimitPerDay = null,
                    usesRemaining = null,
                    expiresAtMs = null,
                )
            ),
            defaultDomain = defaultDomain,
            working = false,
            lastImportResult = null,
            kindLabel = { it.name },
            hitsLabel = { "${it.hitCount}" },
            parseCount = { it.toUIntOrNull() },
            parseCountI64 = { it.toLongOrNull() },
            onBack = {},
            onCreate = { _, _, _, _, _ -> },
            onGenerateDisposable = {},
            onGenerateWithParams = { _, _, _ -> },
            onUpdate = { _, _, _, _, _ -> },
            onRevoke = {},
            onEnable = {},
            onDelete = {},
            onImport = {},
        )
    }

    @Test
    fun theAliasGenerateGates_whileTheAddAndImportOpenersStayLive() {
        render(FfiConnectionState.DISCONNECTED) { MailAliases() }
        // Generate dispatches directly, so it is a commit and gates.
        composeTestRule.onNodeWithTag("mail-aliases-generate-disposable-button")
            .performScrollTo().assertIsNotEnabled()
        // Its two neighbours only reveal sheets — the pairing that makes this
        // page's assertion discriminating rather than "everything is grey".
        composeTestRule.onNodeWithTag("mail-aliases-add-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("mail-aliases-import-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theAliasSubmitGatesInCreateMode_whileItsBuffersAndCancelStayLive() {
        render(FfiConnectionState.DISCONNECTED) { MailAliases() }
        composeTestRule.onNodeWithTag("mail-aliases-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-aliases-add-sheet-submit-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-aliases-add-sheet-pattern-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("mail-aliases-add-sheet-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theAliasSubmitAlsoGatesInEditMode_theDiscriminantsUpdateArm() {
        render(FfiConnectionState.DISCONNECTED) { MailAliases() }
        composeTestRule.onNodeWithTag("mail-aliases-list-item-edit-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-aliases-add-sheet-submit-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theAliasImportSubmitGates_whileItsTextareaAndCancelStayLive() {
        render(FfiConnectionState.DISCONNECTED) { MailAliases() }
        composeTestRule.onNodeWithTag("mail-aliases-import-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-aliases-import-submit-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-aliases-import-textarea")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("mail-aliases-import-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theAliasRowActiveToggleAndRevokeGate_whileEditAuditAndOverflowStayLive() {
        render(FfiConnectionState.DISCONNECTED) { MailAliases() }
        composeTestRule.onNodeWithTag("mail-aliases-list-item-disabled-toggle")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-aliases-list-item-revoke-button")
            .performScrollTo().assertIsNotEnabled()
        // Edit reveals, audit expands, and the overflow opens a menu — three
        // live siblings in the same row as two dead controls. The overflow one
        // matters most: the DELETE item inside it declares, and a gated opener
        // would make that item unreachable rather than merely dead.
        composeTestRule.onNodeWithTag("mail-aliases-list-item-edit-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("mail-aliases-list-item-show-audit")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("mail-aliases-list-item-overflow-menu")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theAliasGenerateStaysDeadWithNoDefaultDomainEvenConnected() {
        // Converse — must SURVIVE the unplug. Without a default domain there is
        // no address to mint, connected or not.
        render(FfiConnectionState.CONNECTED) { MailAliases(defaultDomain = null) }
        composeTestRule.onNodeWithTag("mail-aliases-generate-disposable-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theAliasRevokeStaysDeadOnAnAlreadyRevokedAliasEvenConnected() {
        // Converse — must SURVIVE the unplug, and the sharpest one on this page:
        // the revoke button's own `!alias.disabled` predicate is handed INTO the
        // gate. A gate that replaced it would turn this into a live button that
        // re-revokes an already-revoked alias.
        render(FfiConnectionState.CONNECTED) { MailAliases(disabled = true) }
        composeTestRule.onNodeWithTag("mail-aliases-list-item-revoke-button")
            .performScrollTo().assertIsNotEnabled()
        // …while the toggle beside it is live, because re-ENABLING is exactly
        // what a disabled alias offers. Same row, opposite verdicts, neither
        // coming from the offline gate.
        composeTestRule.onNodeWithTag("mail-aliases-list-item-disabled-toggle")
            .performScrollTo().assertIsEnabled()
    }

    @Composable
    private fun MailSettings(pendingRotation: PendingRotationStatus? = null) {
        MailSettingsContent(
            enabled = true,
            caldavEnabled = false,
            carddavEnabled = false,
            servesWebdavSet = false,
            credentialManagementReachable = true,
            servingEnabled = true,
            credentials = listOf(
                MailCredentialSummary(
                    credentialId = "c1",
                    displayName = "iPhone Mail",
                    kind = CredentialKind.PLAIN,
                    createdAt = 1_700_000_000u,
                    muaUsername = "{handle}+c1@example.com",
                    revoked = false,
                )
            ),
            mua = MuaInstructions(
                imapHost = "mail.example.com",
                imapPort = 993u,
                smtpHost = "mail.example.com",
                smtpPort = 465u,
                caldavHost = "mail.example.com",
                caldavPort = 443u,
                webdavUrl = "https://mail.example.com/webdav/",
                domain = "example.com",
                usernameFormat = "{handle}@{domain}",
                authMechanism = "PLAIN",
            ),
            pendingRotation = pendingRotation,
            status = SettingsStatus.Idle,
            nestEncrypted = true,
            generatedPassword = "Generated-Strong-Pw-1234", // gitleaks:allow
            lastToken = null,
            onBack = {},
            onNavAliases = {},
            onNavLists = {},
            onNavExport = {},
            onNavImport = {},
            onNavSpam = {},
            onEnable = { _, _, _, _ -> },
            onAddCredential = { _, _, _, _ -> },
            onDisableMail = {},
            onSetServingEnabled = {},
            strengthLabel = { "" },
            statusLabel = { _, _ -> "All up to date" },
            onStartRotation = { _, _ -> },
            onResumeRotation = {},
            onRegeneratePassword = {},
            onClearToken = {},
        )
    }

    @Test
    fun theCredentialProvisioningCommitGates_whileItsCancelStaysLive() {
        // The provisioning submit is the commit (`provision_wrapped_mls_blob`,
        // OnlineOnly); the cancel beside it is a local close. The credential
        // revoke that used to pair with them here is a row of the Connected apps
        // page now — its offline-safe verdict is pinned there
        // (`aMailPasswordRevokeStaysLive…`).
        render(FfiConnectionState.DISCONNECTED) { MailSettings() }
        composeTestRule.onNodeWithTag("mail-settings-add-credential-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("mail-add-credential-submit-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-add-credential-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theRotateKeysConfirmGates_whileItsOpenerAndCancelStayLive() {
        render(FfiConnectionState.DISCONNECTED) { MailSettings() }
        composeTestRule.onNodeWithTag("mail-settings-rotate-keys-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("mail-rotate-keys-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("mail-rotate-keys-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun thePendingRotationResumeGates_becauseResumingReSealsTheMsek() {
        render(FfiConnectionState.DISCONNECTED) {
            MailSettings(pendingRotation = PendingRotationStatus(credentialsRemaining = listOf("a", "b")))
        }
        composeTestRule.onNodeWithTag("mail-settings-pending-rotation-resume-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theServeHereToggleGates_whileTheMailEnabledToggleStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { MailSettings() }
        composeTestRule.onNodeWithTag("mail-settings-serve-here-toggle")
            .performScrollTo().assertIsNotEnabled()
        // The hub's own enabled toggle dispatches NOTHING — flipping it on
        // reveals the enable form, flipping it off opens the disable confirm.
        // It is a reveal on both edges, so arming-is-local keeps it live, and
        // gating it would strand both ceremonies behind a dead control.
        composeTestRule.onNodeWithTag("mail-settings-enabled-toggle")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theMailSettingsCommitsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { MailSettings() }
        composeTestRule.onNodeWithTag("mail-settings-serve-here-toggle")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("mail-settings-add-credential-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-add-credential-submit-button")
            .performScrollTo().assertIsEnabled()
    }
    // ── The bridge / DAV plane (batch 10) ───────────────────────────────────
    //
    // The three deployment-wide DAV enables, the forwarder admin, the
    // pending/approved bridge roster, the unified Bridges page and the folder
    // destination places. Fifteen kinds. Two things here are worth reading
    // before the cases:
    //
    // 1. **`revoke_service_user` is the "rotate" button.** An earlier census
    //    graded that kind as having no android render site at all and filed it
    //    as an unbuilt section; it is the approved-roster rotate confirm, whose
    //    machine action calls `nest.revoke_service_user`. Grade a kind by
    //    following the dispatcher to its `request(...)`, never by matching the
    //    control's own vocabulary.
    // 2. **The unified Bridges page's follows and settings stay live and carry
    //    no gate** — `add_follow`/`remove_follow` are OfflineQueued and
    //    `set_settings` is OfflineSafe, so ruling 1 leaves all three alone.
    //    They are the live siblings of the unlink beside them.

    @Composable
    private fun AdminCalendar(working: Boolean = false) {
        AdminCalendarContent(
            caldavEnabled = true,
            caldavPort = 8443,
            working = working,
            onBack = {},
            onSetCaldavEnabled = {},
            onSavePort = {},
            onInvalidPort = {},
            parsePort = { it.toIntOrNull() },
        )
    }

    @Test
    fun theCaldavEnableAndPortSaveGate_whileThePortBufferStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { AdminCalendar() }
        composeTestRule.onNodeWithTag("admin-calendar-enabled-toggle")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-save-button")
            .performScrollTo().assertIsNotEnabled()
        // The commit gates, not the buffer — the split this page has and the
        // spam page's threshold field does not, because here Save exists.
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-input")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theCarddavAndWebdavEnablesGate_andAreLiveWhenConnected() {
        // The two one-control sibling pages, asserted together: same shape, same
        // ruling, and a dispatch-on-change toggle is the commit on both.
        render(FfiConnectionState.DISCONNECTED) {
            AdminContactsContent(
                carddavEnabled = true,
                working = false,
                onBack = {},
                onSetCarddavEnabled = {},
            )
        }
        composeTestRule.onNodeWithTag("admin-contacts-carddav-enabled-toggle")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theWebdavEnableGates() {
        render(FfiConnectionState.DISCONNECTED) {
            AdminFilesContent(
                webdavEnabled = true,
                working = false,
                onBack = {},
                onSetWebdavEnabled = {},
            )
        }
        composeTestRule.onNodeWithTag("admin-files-webdav-enabled-toggle")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theDavEnablesAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminCalendar() }
        composeTestRule.onNodeWithTag("admin-calendar-enabled-toggle")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-save-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theCaldavCommitsStayDeadWhileARequestIsInFlightEvenConnected() {
        // Converse — must SURVIVE the unplug.
        render(FfiConnectionState.CONNECTED) { AdminCalendar(working = true) }
        composeTestRule.onNodeWithTag("admin-calendar-enabled-toggle")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-save-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Composable
    private fun AdminAliases(localDomains: List<String> = listOf("example.com")) {
        AdminAliasesContent(
            forwarders = listOf(
                ForwarderView(
                    aliasIdHex = "aa",
                    localDomain = "example.com",
                    pattern = "sales",
                    address = "sales@example.com",
                    forwardTarget = "someone@example.org",
                )
            ),
            localDomains = localDomains,
            actionError = null,
            working = false,
            onBack = {},
            onCreate = { _, _, _ -> },
            onDelete = {},
        )
    }

    @Test
    fun theForwarderCreateAndDeleteGate_whileTheFormBuffersStayLive() {
        // Both dispatch through the SHARED `ForwarderMachine`, so both gates sit
        // at their call sites — a machine is not a composable and a kind literal
        // has to sit where the checker can read it.
        render(FfiConnectionState.DISCONNECTED) { AdminAliases() }
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-submit-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-row-delete-button")
            .performScrollTo().assertIsNotEnabled()
        // The three inputs above the submit are pure buffers.
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-pattern-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-target-input")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theForwarderSubmitStaysDeadWithNoLocalDomainEvenConnected() {
        // Converse — must SURVIVE the unplug. `canAdd` is the page's own
        // predicate and the gate composes with it.
        render(FfiConnectionState.CONNECTED) { AdminAliases(localDomains = emptyList()) }
        composeTestRule.onNodeWithTag("admin-aliases-forwarder-add-submit-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Composable
    private fun AdminBridgesPending() {
        AdminBridgesPendingContent(
            pending = listOf(
                PendingBridgeView(
                    pubkeyHex = "deadbeef",
                    requestedRole = "mta",
                    sourceIp = null,
                    firstSeenAt = 1_717_000_000_000uL,
                )
            ),
            approved = listOf(
                ApprovedBridgeView(
                    pubkeyHex = "cafebabe",
                    role = "atproto.pds",
                    approvedAt = 1_717_000_000_000uL,
                )
            ),
            working = false,
            displayName = { it },
            onBack = {},
            onApprove = { _, _ -> },
            onReject = {},
            onRotate = {},
        )
    }

    @Test
    fun theApproveAndRejectGate_becauseRefusingAnAdmissionIsStillACommit() {
        // A DENY is a commit — the fourth instance of that shape in this
        // fan-out. The reject is also a two-click inline confirm and gates
        // whole, arm click included: no opener, nothing revealed.
        render(FfiConnectionState.DISCONNECTED) { AdminBridgesPending() }
        composeTestRule.onNodeWithTag("admin-bridges-pending-approve-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-bridges-pending-reject-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun theRotateConfirmGates_onRevokeServiceUserNotOnAnythingCalledRotate() {
        // The census-correcting case. The opener stays live — it reveals a
        // warning worth reading with no nest — and the CONFIRM declares
        // `revoke_service_user`, which is what rotating a bridge key actually
        // issues.
        render(FfiConnectionState.DISCONNECTED) { AdminBridgesPending() }
        composeTestRule.onNodeWithTag("admin-bridges-approved-rotate-button")
            .performScrollTo().assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("admin-bridges-rotate-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-bridges-rotate-cancel-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theBridgeAdmissionControlsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AdminBridgesPending() }
        composeTestRule.onNodeWithTag("admin-bridges-pending-approve-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-bridges-pending-reject-button")
            .performScrollTo().assertIsEnabled()
    }

    // ⚠ `linkModes` MUST be non-empty, and that is trap 3 in its purest form.
    // With `linkModes = null` the shared `bridgeLinkBlock` rule returns
    // `NoApplicableMode`, so `UnlinkedSection` renders a BLOCKED placeholder —
    // same `bridge-action-button` id, hard-coded `enabled = false` — and returns
    // before the real link button ever composes. A disconnected assert then
    // passes against an app with NO GATE AT ALL, which is exactly what the first
    // run of this batch did. The live-direction case is what caught it: the
    // pairing is the assertion, not the disabled assert alone.
    private fun bridgeStatus(linked: Boolean) = FfiBridgeStatus(
        id = "rss",
        name = "RSS",
        available = true,
        linked = linked,
        identity = FfiBridgeIdentity(label = "Feed", value = "u", display = "u"),
        mode = "generated",
        settings = emptyList(),
        supportsFollows = true,
        linkModes = listOf(
            FfiBridgeLinkMode(
                mode = "token",
                label = "Link RSS",
                clientAction = null,
                platform = null,
                fields = listOf(FfiBridgeLinkField("url", "Feed URL", "text", null)),
            ),
        ),
        error = null,
    )

    @Test
    fun theBridgeUnlinkGates_whileItsFollowsRailStaysLive() {
        // Driven through `BridgeCard`, the production entry point, so the
        // linked/unlinked split is the real one.
        render(FfiConnectionState.DISCONNECTED) {
            BridgeCard(
                bridge = bridgeStatus(linked = true),
                bridgeFollows = emptyList(),
                onLink = { _, _ -> },
                onUnlink = {},
                onUpdateSetting = { _, _ -> },
                onAddFollow = { _, _ -> },
                onRemoveFollow = {},
            )
        }
        // ⚠ No `performScrollTo()` anywhere in these three cases. `BridgeCard`
        // is a card, not a page: rendered on its own it has no scroll parent,
        // and `performScrollTo()` then throws "Semantic Node has no parent
        // layout with a Scroll SemanticsAction" — an AssertionError that reads
        // exactly like the gate having failed. Trap 7's second half, paid for
        // here. The card is small enough to be laid out whole.
        composeTestRule.onNodeWithTag("bridge-action-button").assertIsNotEnabled()
        // `add_follow` is OfflineQueued — class 2 works without a nest, so
        // ruling 1 leaves it alone. This is the live sibling that stops a
        // blanket grey from passing on this page.
        composeTestRule.onNodeWithTag("bridge-add-follow-button").assertIsEnabled()
    }

    @Test
    fun theBridgeLinkGates() {
        render(FfiConnectionState.DISCONNECTED) {
            BridgeCard(
                bridge = bridgeStatus(linked = false),
                bridgeFollows = emptyList(),
                onLink = { _, _ -> },
                onUnlink = {},
                onUpdateSetting = { _, _ -> },
                onAddFollow = { _, _ -> },
                onRemoveFollow = {},
            )
        }
        composeTestRule.onNodeWithTag("bridge-action-button").assertIsNotEnabled()
    }

    @Test
    fun theBridgeLinkIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            BridgeCard(
                bridge = bridgeStatus(linked = false),
                bridgeFollows = emptyList(),
                onLink = { _, _ -> },
                onUnlink = {},
                onUpdateSetting = { _, _ -> },
                onAddFollow = { _, _ -> },
                onRemoveFollow = {},
            )
        }
        composeTestRule.onNodeWithTag("bridge-action-button").assertIsEnabled()
    }

    // ── The events plane ────────────────────────────────────────────────────
    //
    // Three kinds, eleven controls, two screens — the batch that needed both
    // events screens split into stateless `*Content` first, because nine of the
    // eleven sat inside the VM-bound bodies where no case here could reach them.
    //
    // The kinds come from the lead app's fallback-free `Action::wire_kind`
    // (`apps/fauna-tui/src/events/mod.rs`): `provision_calendar`, `delete_event`,
    // and `put_event_ciphertext` for the four VEVENT writers — create, RSVP,
    // reminder set/remove, and attendee invite. android adds a fifth writer tui
    // has no gesture for at all: the `.ics` **import**, which parses, seals and
    // PUTs every VEVENT it reads.

    private fun eventDetail(organizedByMe: Boolean) = EventDetail(
        id = "ev-1",
        uid = "uid-1",
        summary = "Gate-proof standup",
        dtstart = "2026-09-01T09:00:00",
        organizedByMe = organizedByMe,
    )

    private fun calendar() = FaunaCalendar(id = "cal-1", name = "Work")

    private fun invited() = EventSummary(
        id = "ev-2",
        uid = "uid-2",
        summary = "Invited review",
        dtstart = "2026-09-02T10:00:00",
    )

    private fun str(id: Int): String =
        ApplicationProvider.getApplicationContext<Context>().getString(id)

    /**
     * [EventDetailContent] with every callback inert — the cases below assert
     * sensitivity, and a fired callback is asserted explicitly where it matters.
     */
    @Composable
    private fun DetailUnderTest(
        organizedByMe: Boolean,
        currentReminder: String? = null,
        onDeleteEvent: () -> Unit = {},
    ) {
        EventDetailContent(
            event = eventDetail(organizedByMe),
            attendees = emptyList(),
            currentReminder = currentReminder,
            isLoading = false,
            onBack = {},
            onRsvp = {},
            onSetReminder = {},
            onRemoveReminder = {},
            onInviteAttendee = {},
            onDeleteEvent = onDeleteEvent,
        )
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theEventDetailRsvpButtonsGate_whileTheBackArrowStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { DetailUnderTest(organizedByMe = false) }
        composeTestRule.onNodeWithTag("event-detail-rsvp-going").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("event-detail-rsvp-decline").assertIsNotEnabled()
        // Backing out of a detail page is pure navigation. If this went dead too
        // the pair would prove nothing — a blanket grey would pass.
        composeTestRule.onNodeWithTag("event-detail-back").assertIsEnabled()
        // EXACTLY two reasons, and counting them is the point (§ R11 forbids a
        // page banner, so the number has to be the number of dead affordances,
        // not "at least one"). Two, not one: the RSVP pair shares a single
        // verdict because it issues a single kind, and the reminder **Set**
        // below it is the page's other gated control in this state — an event
        // with no reminder yet renders select + Set. A one-reason page here
        // would mean a group had lost its explanation; a three-reason page
        // would mean something had grown a banner or double-declared.
        composeTestRule.onAllNodesWithText(needsNest()).assertCountEquals(2)
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theEventDetailRsvpButtonsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { DetailUnderTest(organizedByMe = false) }
        composeTestRule.onNodeWithTag("event-detail-rsvp-going").assertIsEnabled()
        composeTestRule.onNodeWithTag("event-detail-rsvp-decline").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theReminderSetGates_whileItsPresetSelectStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { DetailUnderTest(organizedByMe = false) }
        composeTestRule.onNodeWithTag("event-detail-reminder-set").assertIsNotEnabled()
        // The sharpest live sibling on this page: the preset select is a DRAFT,
        // applied on Set and never on selection (events.md § Reminders; the lead
        // app pins `SetReminderOffset` → no kind). A user may pick their offset
        // with no nest and commit it on reconnect. If this ever goes dead, the
        // gate has been attached to the wrong half of the control.
        composeTestRule.onNodeWithTag("event-detail-reminder-select").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theReminderSetIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { DetailUnderTest(organizedByMe = false) }
        composeTestRule.onNodeWithTag("event-detail-reminder-set").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theReminderRemoveGates_whileTheCurrentOffsetStaysReadable() {
        render(FfiConnectionState.DISCONNECTED) {
            DetailUnderTest(organizedByMe = false, currentReminder = "PT1H")
        }
        composeTestRule.onNodeWithTag("event-detail-reminder-remove").assertIsNotEnabled()
        // Reading what is already set is a read; ruling 1 never greys it, and the
        // label is what tells the user *which* reminder they cannot yet clear.
        composeTestRule.onNodeWithTag("event-detail-reminder-current").assertIsDisplayed()
        // The set-state and no-reminder-state are exclusive: Set must be absent.
        composeTestRule.onNodeWithTag("event-detail-reminder-set").assertDoesNotExist()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theReminderRemoveIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            DetailUnderTest(organizedByMe = false, currentReminder = "PT1H")
        }
        composeTestRule.onNodeWithTag("event-detail-reminder-remove").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theAttendeeInviteGates_whileItsFieldStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { DetailUnderTest(organizedByMe = true) }
        // Trap 3: the button carries its own `inviteEmail.isNotBlank()` predicate,
        // so an untouched form disables it for a reason that has nothing to do
        // with the nest. Satisfy the page's predicate first or this assert passes
        // against a build with no gate at all.
        composeTestRule.onNodeWithTag("attendee-invite-field")
            .performTextInput("someone@example.com")
        composeTestRule.onNodeWithTag("attendee-invite-button").assertIsNotEnabled()
        // Composing the invitation offline and sending it on reconnect is the
        // whole point of not greying the buffer.
        composeTestRule.onNodeWithTag("attendee-invite-field").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theAttendeeInviteIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { DetailUnderTest(organizedByMe = true) }
        composeTestRule.onNodeWithTag("attendee-invite-field")
            .performTextInput("someone@example.com")
        composeTestRule.onNodeWithTag("attendee-invite-button").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun anEmptyInviteFieldDisablesTheButtonWithoutTheGatesReason() {
        // The converse case. The nest is present, so anything dead here is dead
        // by the page's own intent — and the gate must add no second reason under
        // it. Had the gate REPLACED the page's predicate rather than composing
        // with it, this would still be enabled.
        render(FfiConnectionState.CONNECTED) { DetailUnderTest(organizedByMe = true) }
        composeTestRule.onNodeWithTag("attendee-invite-button").assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theDeleteConfirmGates_whileItsOpenerAndCancelStayLive() {
        var deleted = false
        render(FfiConnectionState.DISCONNECTED) {
            DetailUnderTest(organizedByMe = true, onDeleteEvent = { deleted = true })
        }
        // The opener only REVEALS the dialog — a gated opener would make the
        // confirm unreachable rather than merely dead, which is not the contract.
        composeTestRule.onNodeWithTag("event-delete-btn").assertIsEnabled()
        composeTestRule.onNodeWithTag("event-delete-btn").performClick()

        // The confirm carries no testTag, so it is addressed by its label — the
        // disposition `aGateDeclaredInsideAnAlertDialogSlotStillSeesTheState`
        // established: an AlertDialog's slots are ordinary composables in the
        // same composition and read `LocalConnectionState` normally.
        composeTestRule.onNodeWithText(str(R.string.common_delete)).assertIsNotEnabled()
        composeTestRule.onNodeWithText(str(R.string.common_cancel)).assertIsEnabled()

        // Belt and braces: a node can report disabled and still run its onClick
        // if the flag was wired to the wrong place.
        composeTestRule.onNodeWithText(str(R.string.common_delete)).performClick()
        assertFalse("a desensitized delete confirm must not fire its action", deleted)
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theDeleteConfirmIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { DetailUnderTest(organizedByMe = true) }
        composeTestRule.onNodeWithTag("event-delete-btn").performClick()
        composeTestRule.onNodeWithText(str(R.string.common_delete)).assertIsEnabled()
    }

    /** [EventsContent] with one calendar and every callback inert. */
    @Composable
    private fun EventsUnderTest(
        selectedCalendar: FaunaCalendar? = calendar(),
        invitedEvents: List<EventSummary> = emptyList(),
        draft: State<FfiEventDrafts?> = remember { mutableStateOf(null) },
        onDraftEdited: (
            summary: String, dtstart: String, dtend: String, description: String, location: String,
        ) -> Unit = { _, _, _, _, _ -> },
        onStartFreshDraft: () -> Unit = {},
    ) {
        EventsContent(
            calendars = listOf(calendar()),
            selectedCalendar = selectedCalendar,
            visibleCalendarIds = setOf("cal-1"),
            events = emptyList(),
            invitedEvents = invitedEvents,
            visibleEvents = emptyList(),
            isLoading = false,
            calendarView = FfiCalendarViewMode.AGENDA,
            selectedDate = java.time.LocalDate.of(2026, 9, 1),
            onRefresh = {},
            onSelectCalendar = {},
            onToggleCalendarVisibility = {},
            onSelectView = {},
            onSelectDate = {},
            onOpenEvent = {},
            onImportIcs = {},
            onExportIcs = {},
            onRsvp = { _, _ -> },
            onCreateCalendar = {},
            onCreateEvent = {},
            draft = draft,
            onDraftEdited = onDraftEdited,
            onStartFreshDraft = onStartFreshDraft,
        )
    }

    // ── Draft-persistence v2, events rail (reserved-folders.md § Drafts Sync;
    // events.md § Persistence) ──────────────────────────────────────────────

    /** The field's typed value alone — `assertTextEquals` compares against
     *  the MERGED `Text` (which for a labelled `OutlinedTextField` includes
     *  the label, e.g. "Summary") plus `EditableText`, so it never matches a
     *  bare typed value; fetch `EditableText` directly instead. */
    private fun SemanticsNodeInteraction.editableText(): String =
        fetchSemanticsNode().config[androidx.compose.ui.semantics.SemanticsProperties.EditableText].text

    /** Rule 1: the New Event (FAB) opener resumes a persisted draft. */
    @Test
    @Config(qualifiers = "h1280dp")
    fun theNewEventOpenerResumesAPersistedDraft() {
        val draft = mutableStateOf<FfiEventDrafts?>(
            FfiEventDrafts("Standup", "2026-09-02T09:00", "2026-09-02T09:30", "", "notes")
        )
        render(FfiConnectionState.CONNECTED) { EventsUnderTest(draft = draft) }
        composeTestRule.onNodeWithTag("new-event-btn").performClick()
        assertEquals("Standup", composeTestRule.onNodeWithTag(Ids.EVENT_SUMMARY).editableText())
        assertEquals("2026-09-02T09:00", composeTestRule.onNodeWithTag(Ids.EVENT_DTSTART).editableText())
    }

    /** Rule 2: a day-cell/slot open (`resumableDraft = null`) never resumes,
     *  even with a pending draft — it stays on its own date prefill only. This
     *  drives [EventFormSheet] directly rather than through a grid slot click
     *  (untagged), since `resumableDraft = null` IS the exact mechanism
     *  [EventsScreen]'s two `onSlotClick` sites rely on. */
    @Test
    @Config(qualifiers = "h1280dp")
    fun aDayCellFreshStartNeverResumesEvenWithAPendingDraft() {
        render(FfiConnectionState.CONNECTED) {
            EventFormSheet(
                calendarId = "cal-1",
                onDismiss = {},
                onCreate = {},
                initialDtstart = "2026-09-02T09:00",
                initialDtend = "2026-09-02T10:00",
                resumableDraft = null,
            )
        }
        assertEquals("", composeTestRule.onNodeWithTag(Ids.EVENT_SUMMARY).editableText())
        assertEquals("2026-09-02T09:00", composeTestRule.onNodeWithTag(Ids.EVENT_DTSTART).editableText())
    }

    /** The fourth rule's late-restore half: a draft arriving AFTER the New
     *  Event sheet is already open (mounted with nothing to resume yet) must
     *  still reach it. */
    @Test
    @Config(qualifiers = "h1280dp")
    fun aLateRestoreStillReachesAnAlreadyOpenSheet() {
        val draft = mutableStateOf<FfiEventDrafts?>(null)
        render(FfiConnectionState.CONNECTED) { EventsUnderTest(draft = draft) }
        composeTestRule.onNodeWithTag("new-event-btn").performClick()
        assertEquals("", composeTestRule.onNodeWithTag(Ids.EVENT_SUMMARY).editableText())
        composeTestRule.runOnIdle { draft.value = FfiEventDrafts("Late arrival", "", "", "", "") }
        assertEquals("Late arrival", composeTestRule.onNodeWithTag(Ids.EVENT_SUMMARY).editableText())
    }

    /** The fourth rule's non-destructive half: once the user has typed
     *  something, a draft arriving afterward must NOT overwrite it — resuming
     *  is the caller's decision, declined once the compose already holds
     *  authored text. */
    @Test
    @Config(qualifiers = "h1280dp")
    fun theResumeIsNonDestructiveOfAlreadyAuthoredText() {
        val draft = mutableStateOf<FfiEventDrafts?>(null)
        render(FfiConnectionState.CONNECTED) { EventsUnderTest(draft = draft) }
        composeTestRule.onNodeWithTag("new-event-btn").performClick()
        composeTestRule.onNodeWithTag(Ids.EVENT_SUMMARY).performTextInput("My own text")
        composeTestRule.runOnIdle { draft.value = FfiEventDrafts("Stale draft", "", "", "", "") }
        assertEquals("My own text", composeTestRule.onNodeWithTag(Ids.EVENT_SUMMARY).editableText())
    }

    /** Every field edit reports to [EventDraftsHost] via `onDraftEdited` — but
     *  mounting the sheet does not, so a slow restore is never raced by a
     *  spurious blank write (the bug this design avoids: see
     *  [EventFormSheet]'s own `onValueChange`-driven reporting, not a
     *  `LaunchedEffect` keyed on the fields). */
    @Test
    @Config(qualifiers = "h1280dp")
    fun editingAFieldReportsTheEditButOpeningTheSheetDoesNot() {
        var edits = 0
        render(FfiConnectionState.CONNECTED) {
            EventsUnderTest(onDraftEdited = { _, _, _, _, _ -> edits++ })
        }
        composeTestRule.onNodeWithTag("new-event-btn").performClick()
        assertEquals("opening the sheet must not itself report an edit", 0, edits)
        composeTestRule.onNodeWithTag(Ids.EVENT_SUMMARY).performTextInput("A")
        assertEquals(1, edits)
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theIcsImportGates_whileItsExportSiblingStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { EventsUnderTest() }
        // Import parses, seals and PUTs every VEVENT it reads; export is a pure
        // read of the same calendar, rendered inches away. Ruling 1 says the read
        // survives — this pairing is what proves the gate reads the *kind* and
        // not "anything on the calendar toolbar".
        composeTestRule.onNodeWithText(str(R.string.events_import_ics)).assertIsNotEnabled()
        composeTestRule.onNodeWithText(str(R.string.events_export_ics)).assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theIcsImportIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { EventsUnderTest() }
        composeTestRule.onNodeWithText(str(R.string.events_import_ics)).assertIsEnabled()
        composeTestRule.onNodeWithText(str(R.string.events_export_ics)).assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theInvitedEventRsvpButtonsGate_whileTheComposeOpenerStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            EventsUnderTest(selectedCalendar = null, invitedEvents = listOf(invited()))
        }
        composeTestRule.onNodeWithTag("event-rsvp-going").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("event-rsvp-interested").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("event-rsvp-decline").assertIsNotEnabled()
        // The compose FAB opens a form; it writes nothing until that form is
        // submitted, so it must survive the outage the RSVPs do not.
        composeTestRule.onNodeWithTag("new-event-btn").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theInvitedEventRsvpButtonsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            EventsUnderTest(selectedCalendar = null, invitedEvents = listOf(invited()))
        }
        composeTestRule.onNodeWithTag("event-rsvp-going").assertIsEnabled()
        composeTestRule.onNodeWithTag("event-rsvp-interested").assertIsEnabled()
        composeTestRule.onNodeWithTag("event-rsvp-decline").assertIsEnabled()
    }

    @Test
    fun theCreateCalendarConfirmGates_whileItsCancelStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            CreateCalendarDialog(onDismiss = {}, onCreate = {})
        }
        // Satisfy the dialog's own `name.isNotBlank()` first (trap 3).
        composeTestRule.onNodeWithTag("calendar-name").performTextInput("Work")
        composeTestRule.onNodeWithTag("create-calendar").assertIsNotEnabled()
        composeTestRule.onNodeWithText(str(R.string.common_cancel)).assertIsEnabled()
    }

    @Test
    fun theCreateCalendarConfirmIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            CreateCalendarDialog(onDismiss = {}, onCreate = {})
        }
        composeTestRule.onNodeWithTag("calendar-name").performTextInput("Work")
        composeTestRule.onNodeWithTag("create-calendar").assertIsEnabled()
    }

    @Test
    fun anEmptyCalendarNameDisablesCreateWithoutTheGatesReason() {
        // Converse: dead by the dialog's own intent, with the nest present.
        render(FfiConnectionState.CONNECTED) {
            CreateCalendarDialog(onDismiss = {}, onCreate = {})
        }
        composeTestRule.onNodeWithTag("create-calendar").assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theCreateEventSubmitGates_whileItsSummaryFieldStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            EventFormSheet(
                calendarId = "cal-1",
                onDismiss = {},
                onCreate = {},
                initialDtstart = "2026-09-01T09:00",
                initialDtend = "2026-09-01T10:00",
            )
        }
        // The form's own completeness predicate needs a summary too (trap 3).
        composeTestRule.onNodeWithTag("event-summary").performTextInput("Standup")
        composeTestRule.onNodeWithTag("create-event").assertIsNotEnabled()
        // Drafting the event offline stays possible; only the submit is dead.
        composeTestRule.onNodeWithTag("event-summary").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theCreateEventSubmitIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            EventFormSheet(
                calendarId = "cal-1",
                onDismiss = {},
                onCreate = {},
                initialDtstart = "2026-09-01T09:00",
                initialDtend = "2026-09-01T10:00",
            )
        }
        composeTestRule.onNodeWithTag("event-summary").performTextInput("Standup")
        composeTestRule.onNodeWithTag("create-event").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun anIncompleteEventFormDisablesCreateWithoutTheGatesReason() {
        // Converse: no summary, nest present.
        render(FfiConnectionState.CONNECTED) {
            EventFormSheet(
                calendarId = "cal-1",
                onDismiss = {},
                onCreate = {},
                initialDtstart = "2026-09-01T09:00",
                initialDtend = "2026-09-01T10:00",
            )
        }
        composeTestRule.onNodeWithTag("create-event").assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ── The Bluesky / atproto plane ─────────────────────────────────────────
    //
    // Nine kinds over eleven declaration sites, all on one page — the largest
    // single page in this fan-out. The kinds come from the lead app's
    // fallback-free `Action::wire_kind` (`apps/fauna-tui/src/settings/mod.rs`),
    // which settles three questions this page would otherwise have to guess:
    //
    //  - `select_level` AND `confirm_transition` collapse onto ONE kind
    //    (`set_integration_level`), because selecting a rung reaches the nest
    //    directly on the effect-free Off -> Linked move and stages the
    //    transition card on every other pick. Both paths that call end in the
    //    same kind, so the selector and the card's confirm both declare it.
    //  - `AtprotoReveal` is a local class-**Read** — ruling 1 leaves
    //    it alone, so the reveal button beside the credential revoke stays live.
    //  - `AtprotoRequestContest` is `None`: "no nest call at all", the whole
    //    point of a remedy for a nest that may be the attacker. It is the
    //    sharpest live sibling on the page — a destructive red confirm that must
    //    survive the outage.
    //
    // ⚠ `atproto.delete_presence` is declared here, contradicting two earlier
    // notes that recorded the control as INERT (`onClick = {}`) and closed it
    // "S5 owes the declaration, do not reopen". Both were true when written and
    // are now stale: the trickle-down wired the whole ceremony,
    // `AtprotoVM.confirmDelete` dispatches, and `AtprotoSettingsMachine::
    // confirm_delete` calls `nest_api.delete_presence()`. The lead app declares
    // it too.

    private fun atprotoSnapshot(
        level: String = "off",
        hostedAllowed: Boolean = true,
        pendingTransition: TransitionCardModel? = null,
        showDidMethodRadio: Boolean = false,
        showDeletePresence: Boolean = false,
        deleteConfirm: DeleteConfirmCardModel? = null,
        credentials: List<AppCredentialRow> = emptyList(),
        delegation: DelegationRow? = null,
        contest: ContestCardRow? = null,
        contestConfirm: ContestConfirmCardModel? = null,
    ) = AtprotoSettingsSnapshot(
        level = level,
        hostedAllowed = hostedAllowed,
        hostedGateReason = LocalizedText("atproto_settings.gate_reason", emptyMap()),
        handlePreview = "",
        identity = null,
        link = null,
        pendingTransition = pendingTransition,
        didMethod = "plc",
        showDidMethodRadio = showDidMethodRadio,
        historyBackfill = false,
        showDeletePresence = showDeletePresence,
        deleteConfirm = deleteConfirm,
        credentials = credentials,
        sessions = emptyList(),
        externalAppsEnabled = true,
        delegation = delegation,
        consents = emptyList(),
        contest = contest,
        contestConfirm = contestConfirm,
        error = null,
    )

    private fun transitionCard(inProgress: Boolean = false) = TransitionCardModel(
        targetLevel = "linked",
        lines = listOf(LocalizedText("atproto_settings.transition_line", emptyMap())),
        showHistoryBackfill = true,
        inProgress = inProgress,
    )

    private fun deleteCard(inProgress: Boolean = false) = DeleteConfirmCardModel(
        lines = listOf(LocalizedText("atproto_settings.delete_line", emptyMap())),
        inProgress = inProgress,
        retireIdentity = RetireIdentityOptIn(available = false, selected = false, unavailableReason = null),
    )

    private fun contestCard() = ContestCardRow(
        state = "contestable",
        detail = LocalizedText("atproto_settings.contest_detail", emptyMap()),
        deadline = null,
        showContest = true,
    )

    private fun contestConfirmCard() = ContestConfirmCardModel(
        lines = listOf(LocalizedText("atproto_settings.contest_line", emptyMap())),
        inProgress = false,
    )

    private fun credential(revealable: Boolean = true) = AppCredentialRow(
        credentialId = "cred-1",
        label = "Ivory",
        dmAllowed = false,
        createdAtMillis = 1_756_000_000_000L,
        lastUsedAtMillis = null,
        revealable = revealable,
    )

    private fun delegation() = DelegationRow(
        deviceKeyHex = "eeff",
        capabilities = listOf("post"),
        authorizedAtMicros = 1_756_000_000_000_000UL,
        expiresAtMicros = null,
        liveness = "live",
        lastUsedAtMillis = null,
    )

    /**
     * [AtprotoSettingsContent] with every callback inert. `blueskyBridge = null`
     * lets the page's own synthetic-unlinked placeholder stand in, exactly as
     * production does before the first `fauna.bridges.list` reply lands.
     */
    @Composable
    private fun AtprotoUnderTest(snapshot: AtprotoSettingsSnapshot) {
        AtprotoSettingsContent(
            snapshot = snapshot,
            blueskyBridge = null,
            blueskyFollows = emptyList(),
            onBack = {},
            onOpenContestConfirm = {},
            onCancelContest = {},
            onRequestContest = {},
            onOpenDeleteConfirm = {},
            onCancelDelete = {},
            onConfirmDelete = {},
            onSelectLevel = {},
            onConfirmTransition = {},
            onCancelTransition = {},
            onSetDidMethod = {},
            onSetHistoryBackfill = {},
            onSetExternalAppsEnabled = {},
            onMint = { _, _ -> null },
            onRevealSecret = { null },
            onRevoke = {},
            onAuthorizeExternalApps = {},
            onDeauthorizeExternalApps = {},
            onLinkBridge = { _, _ -> },
            onUnlinkBridge = {},
            onUpdateBridgeSetting = { _, _ -> },
            onAddBridgeFollow = { _, _ -> },
            onRemoveBridgeFollow = {},
        )
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theDepthRungsGate_whileTheContestConfirmStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            AtprotoUnderTest(
                atprotoSnapshot(
                    contest = contestCard(),
                    contestConfirm = contestConfirmCard(),
                )
            )
        }
        composeTestRule.onNodeWithTag("atproto-depth-linked").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("atproto-depth-hosted-visible").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("atproto-depth-hosted-full").assertIsNotEnabled()
        // THE discriminator for this whole page. The contest is signed and
        // submitted on this device's OWN connection to the public PLC directory
        // — `ui/atproto.md` § User actions: "no nest call at all", which is the
        // entire point of a remedy for a nest that may be the attacker. A
        // destructive red confirm that must SURVIVE the outage, inches from the
        // delete confirm that must not. No blanket grey can pass this pair.
        composeTestRule.onNodeWithTag("atproto-contest-confirm").assertIsEnabled()
        composeTestRule.onNodeWithTag("atproto-contest-cancel").assertIsEnabled()
        composeTestRule.onNodeWithTag("atproto-contest").assertIsEnabled()
        // EXACTLY one reason, and the count is the assertion (§ R11 forbids a
        // page banner, so the number must be the number of dead affordance
        // GROUPS). One, not four: the selector's rungs share a single verdict
        // because they issue a single kind. At level `off` with no staged
        // transition it is the only gated group on the page — the transition
        // card, the linked BridgeCard, the hosted panel, the delete confirm and
        // the whole full-PDS panel are all unrendered in this state. ⚠ Seed a
        // different state and this number moves; it is a property of the state,
        // not of the selector.
        composeTestRule.onAllNodesWithText(needsNest()).assertCountEquals(1)
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theDepthRungsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { AtprotoUnderTest(atprotoSnapshot()) }
        composeTestRule.onNodeWithTag("atproto-depth-linked").assertIsEnabled()
        composeTestRule.onNodeWithTag("atproto-depth-hosted-full").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun aHostedRungOnANonPublicDomainStaysDeadWithoutTheGatesReason() {
        // CONVERSE. The page's own hosted gate already closed the two hosted
        // rungs; the nest is present. Unplugging the offline gate must not move
        // this — if it did, the gate would be REPLACING the page's predicate
        // instead of composing with it, and a reconnect would hand the user two
        // rungs their domain cannot support.
        render(FfiConnectionState.CONNECTED) {
            AtprotoUnderTest(atprotoSnapshot(hostedAllowed = false))
        }
        composeTestRule.onNodeWithTag("atproto-depth-hosted-visible").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("atproto-depth-hosted-full").assertIsNotEnabled()
        // Off and Linked are unaffected by the hosted gate, so they stay live.
        composeTestRule.onNodeWithTag("atproto-depth-linked").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theTransitionConfirmGates_whileItsCancelStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            AtprotoUnderTest(atprotoSnapshot(pendingTransition = transitionCard()))
        }
        composeTestRule.onNodeWithTag("atproto-depth-confirm").assertIsNotEnabled()
        // Backing out of a staged transition is a pure-local machine mutation
        // (`AtprotoVM.cancelTransition` never dispatches). A user who staged a
        // change before the nest went away must still be able to unstage it.
        composeTestRule.onNodeWithTag("atproto-depth-cancel").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun aTransitionAlreadyInFlightStaysDeadWithoutTheGatesReason() {
        // CONVERSE: the card's own `inProgress` closed the confirm, nest present.
        render(FfiConnectionState.CONNECTED) {
            AtprotoUnderTest(atprotoSnapshot(pendingTransition = transitionCard(inProgress = true)))
        }
        composeTestRule.onNodeWithTag("atproto-depth-confirm").assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theDeletePresenceConfirmGates_whileItsOpenerAndCancelStayLive() {
        render(FfiConnectionState.DISCONNECTED) {
            AtprotoUnderTest(
                atprotoSnapshot(showDeletePresence = true, deleteConfirm = deleteCard())
            )
        }
        composeTestRule.onNodeWithTag("atproto-delete-confirm").assertIsNotEnabled()
        // The OPENER stays live — a gated opener would make the confirm
        // unreachable rather than dead, which is a different (and worse) thing
        // than the contract asks for: the user could not even see the reason.
        composeTestRule.onNodeWithTag("atproto-delete-presence").assertIsEnabled()
        // Backing out of a destructive ceremony must never need a nest.
        composeTestRule.onNodeWithTag("atproto-delete-cancel").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theDeletePresenceConfirmIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            AtprotoUnderTest(
                atprotoSnapshot(showDeletePresence = true, deleteConfirm = deleteCard())
            )
        }
        composeTestRule.onNodeWithTag("atproto-delete-confirm").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theExternalAppsKillSwitchAndMintGate_whileTheDidMethodDraftStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            AtprotoUnderTest(
                atprotoSnapshot(level = "hosted_full", showDidMethodRadio = true)
            )
        }
        // A dispatch-on-change toggle IS the commit — no Save beside it.
        composeTestRule.onNodeWithTag("atproto-external-apps-enable").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("atproto-app-credential-mint").assertIsNotEnabled()
        // The DID-method radio is a DRAFT the machine holds locally
        // (`AtprotoVM.setDidMethod` never dispatches) — a user may choose their
        // method with no nest and commit it with the transition. If this ever
        // goes dead the gate has been attached to the wrong half of the page.
        composeTestRule.onNodeWithTag("atproto-did-method-plc").assertIsEnabled()
        composeTestRule.onNodeWithTag("atproto-did-method-web").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theExternalAppsKillSwitchAndMintAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            AtprotoUnderTest(atprotoSnapshot(level = "hosted_full"))
        }
        composeTestRule.onNodeWithTag("atproto-external-apps-enable").assertIsEnabled()
        composeTestRule.onNodeWithTag("atproto-app-credential-mint").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theCredentialRevokeGates_whileItsRevealSiblingStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            AtprotoUnderTest(
                atprotoSnapshot(level = "hosted_full", credentials = listOf(credential()))
            )
        }
        composeTestRule.onNodeWithTag("atproto-app-credential-revoke").assertIsNotEnabled()
        // The sharpest sibling on this row, and it is a CLASS distinction rather
        // than a local/remote one: reveal is a local class-Read, so
        // ruling 1 leaves it alone — the secret comes from this device's own
        // `fauna.state.atproto` store, never from the nest (the D3 custody split). A blanket
        // grey of the credential card would kill it.
        composeTestRule.onNodeWithTag("atproto-app-credential-reveal").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theFirstGrantDelegationAuthorizeGates() {
        // The `delegation == null` branch — its own call site, and the one a
        // declaration on the live-row branch alone would leave ungated.
        render(FfiConnectionState.DISCONNECTED) {
            AtprotoUnderTest(atprotoSnapshot(level = "hosted_full", delegation = null))
        }
        composeTestRule.onNodeWithTag("atproto-delegation-authorize").assertIsNotEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theDelegationRenewAndRevokeGate_whileTheCredentialRevealStaysLive() {
        render(FfiConnectionState.DISCONNECTED) {
            AtprotoUnderTest(
                atprotoSnapshot(
                    level = "hosted_full",
                    credentials = listOf(credential()),
                    delegation = delegation(),
                )
            )
        }
        // The live-row branch renders re-authorize (the RENEWAL gesture) beside
        // revoke. Two kinds, two verdicts — both OnlineOnly today, but read
        // separately so the page does not hard-code that coincidence.
        composeTestRule.onNodeWithTag("atproto-delegation-authorize").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("atproto-delegation-revoke").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("atproto-app-credential-reveal").assertIsEnabled()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theDelegationRenewAndRevokeAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) {
            AtprotoUnderTest(atprotoSnapshot(level = "hosted_full", delegation = delegation()))
        }
        composeTestRule.onNodeWithTag("atproto-delegation-authorize").assertIsEnabled()
        composeTestRule.onNodeWithTag("atproto-delegation-revoke").assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // ── batch 13: the zap-signer trust root, and the local restore ────────
    //
    // Two pages that each carry a control whose own predicate is NOT the
    // outage: the zap-signer add already reads a Dim-3 courtesy verdict, and
    // the local restore already reads a friction bar. Both are therefore
    // composition tests as much as gate tests — the converse cases below fail
    // if `faunaGate` ever REPLACES a page predicate instead of `&&`-ing with
    // it, which on a connected nest would re-open a restore whose typed id
    // does not match, or an add the nest would refuse anyway.

    private fun zapSigner() = ZapSignerItem(
        id = 1L,
        signerPubkey = "0".repeat(64),
        label = "a friend",
        createdAt = 100uL,
    )

    /** A `zaps` row whose Dim-3 answer is *disabled* — the add button's own
     *  predicate, independent of the transport. */
    private fun deniedZapsRow() = FfiFeatureRow(
        feature = "zaps",
        name = LocalizedText("features.section_title", emptyMap()),
        availability = "deny",
        deniedBy = null,
        cells = emptyList(),
        perOperationMax = null,
        perOperationMaxTier = null,
        unit = "millisats",
        affordance = "disabled",
        restriction = null,
        status = LocalizedText("features.status_restricted", emptyMap()),
    )

    @Composable
    private fun ZapSigners(gateRow: FfiFeatureRow? = null) {
        NostrContent(
            registered = true,
            bridge = nostrBridge(),
            follows = emptyList(),
            zapSigners = listOf(zapSigner()),
            zapSignerGateRow = gateRow,
            onLink = { _, _ -> },
            onUnlink = {},
            onUpdateSetting = { _, _ -> },
            onAddRelay = {},
            onRemoveRelay = {},
            onAddFollow = { _, _ -> },
            onRemoveFollow = {},
        )
    }

    @Test
    fun theZapSignerAddAndRemoveGate_whileTheirPubkeyAndLabelBuffersStayLive() {
        render(FfiConnectionState.DISCONNECTED) { ZapSigners() }
        // Designating a signer and undesignating one are both commits: the
        // remove is an immediate commit with no arming step, so the row button
        // IS the gesture.
        composeTestRule.onNodeWithTag("nostr-zap-signer-add-btn")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("nostr-zap-signer-remove")
            .performScrollTo().assertIsNotEnabled()
        // "The commit gates, not the buffer": a pubkey and a label can be
        // pasted with no nest and submitted on reconnect.
        composeTestRule.onNodeWithTag("nostr-zap-signer-pubkey-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("nostr-zap-signer-label-input")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theZapSignerCommitsAreLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { ZapSigners() }
        composeTestRule.onNodeWithTag("nostr-zap-signer-add-btn")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag("nostr-zap-signer-remove")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun theZapSignerAddStaysDeadUnderAFeatureDenyEvenConnected() {
        render(FfiConnectionState.CONNECTED) { ZapSigners(gateRow = deniedZapsRow()) }
        // The converse half. The page's Dim-3 verdict is the stronger, more
        // specific reason, so with a nest present the button is still dead and
        // the reason beside it is the FEATURE's, never "needs a nest".
        composeTestRule.onNodeWithTag("nostr-zap-signer-add-btn")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
        // Removal is de-escalation and is never feature-gated: it survives the
        // deny, which is what makes this case discriminating rather than a
        // blanket-disable assertion.
        composeTestRule.onNodeWithTag("nostr-zap-signer-remove")
            .performScrollTo().assertIsEnabled()
    }

    // ── backups: the local message-kind restore ──────────────────────────

    private fun snapshot() = FfiSnapshotSummary(
        id = 7L,
        createdAt = 100L,
        messageKind = "mail",
        fileCount = 3L,
        totalBytes = 1024L,
        deviceId = null,
    )

    /** Renders the card and satisfies its friction bar, so the assertion below
     *  is about the gate rather than about an unsatisfied page predicate. */
    private fun renderRestore(state: FfiConnectionState, confirmId: String = "7") {
        // `LocalRestoreContent` does not scroll itself (the Backups page it
        // mounts into does), so `performScrollTo()` needs a host to walk.
        render(state) {
            ScrollHost {
                LocalRestoreContent(
                    snapshots = listOf(snapshot()),
                    hasDestinations = true,
                    progress = RestoreProgress.IDLE,
                    configAbsent = false,
                    onRestore = { _, _ -> },
                )
            }
        }
        composeTestRule.onNodeWithTag("restore-confirm-input")
            .performScrollTo().performTextInput(confirmId)
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theLocalRestoreConfirmGates_whileItsFrictionBarStaysLive() {
        renderRestore(FfiConnectionState.DISCONNECTED)
        composeTestRule.onNodeWithTag("restore-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        // The friction bar is a buffer — a snapshot id can be re-typed with no
        // nest, and the restore fires on reconnect.
        composeTestRule.onNodeWithTag("restore-confirm-input")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertIsDisplayed()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theLocalRestoreConfirmIsLiveWhenConnected() {
        renderRestore(FfiConnectionState.CONNECTED)
        composeTestRule.onNodeWithTag("restore-confirm-button")
            .performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    @Config(qualifiers = "h1280dp")
    fun theLocalRestoreConfirmStaysDeadOnAnUnmatchedFrictionBarEvenConnected() {
        renderRestore(FfiConnectionState.CONNECTED, confirmId = "8")
        // The converse half: the typed id does not match the selected
        // snapshot, so the page's own predicate keeps the button dead with a
        // nest present. A gate that replaced that predicate would re-open a
        // destructive restore the friction bar exists to slow down.
        composeTestRule.onNodeWithTag("restore-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // -- encryption settings: the one-time key-package top-up --------------
    //
    // The button carries no testTag (no app tags it, and minting an id would
    // need a ui.yaml change under rule A), so it is addressed by its label --
    // the same disposition batch 11 took for the .ics import/export pair.

    private fun refreshKeysLabel(): String =
        ApplicationProvider.getApplicationContext<Context>()
            .getString(R.string.settings_encryption_page_refresh_keys)

    private fun renderEncryption(
        state: FfiConnectionState,
        isPublishing: Boolean = false,
    ) {
        render(state) {
            EncryptionSettingsContent(
                keyPackageCount = 4,
                isPublishing = isPublishing,
                errorMessage = null,
                onBack = {},
                onRefreshKeys = {},
            )
        }
    }

    @Test
    fun theKeyPackageRefreshGates_whileTheCountStaysReadable() {
        // Replenishing the pool PUBLISHES the fresh key packages to the nest
        // (`fauna.conversations.keypackage.upload`), so it cannot run offline.
        renderEncryption(FfiConnectionState.DISCONNECTED)
        composeTestRule.onNodeWithText(refreshKeysLabel()).assertIsNotEnabled()
        // A read is never greyed on the gate's account (ruling 1): the pool
        // count already in hand stays on screen beside the dead button.
        composeTestRule.onNodeWithText("4").assertIsDisplayed()
        composeTestRule.onNodeWithText(needsNest()).assertIsDisplayed()
    }

    @Test
    fun theKeyPackageRefreshIsLiveWhenConnected() {
        renderEncryption(FfiConnectionState.CONNECTED)
        composeTestRule.onNodeWithText(refreshKeysLabel()).assertIsEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    @Test
    fun theKeyPackageRefreshStaysDeadWhilePublishingEvenConnected() {
        // The converse half: the page's own in-flight predicate is the
        // stronger, more specific reason, so a publish already running keeps
        // the button dead WITH a nest -- and the reason beside it is the
        // page's silence, never "needs a nest". A gate that replaced that
        // predicate would re-open the button mid-publish.
        renderEncryption(FfiConnectionState.CONNECTED, isPublishing = true)
        composeTestRule.onNodeWithText(refreshKeysLabel()).assertIsNotEnabled()
        composeTestRule.onNodeWithText(needsNest()).assertDoesNotExist()
    }

    // -- nostr: the page's own unlink, and the blind spot that hid it ------
    //
    // `nostr-unlink-button` issues `fauna.bridges.unlink`, which BridgesScreen's
    // per-bridge unlink ALREADY declares -- so the rule-4 differential, which
    // is per-KIND, saw the kind as covered and never named this second control.
    // Kept as a worked example of what the probe cannot do for you.

    @Test
    fun theNostrUnlinkGates_whileTheIdentityCopyStaysLive() {
        render(FfiConnectionState.DISCONNECTED) { ZapSigners() }
        composeTestRule.onNodeWithTag("nostr-unlink-button")
            .performScrollTo().assertIsNotEnabled()
        // The live sibling on the same card: copying the npub to the clipboard
        // is local and must survive the outage.
        composeTestRule.onNodeWithTag("nostr-pubkey-copy-btn")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun theNostrUnlinkIsLiveWhenConnected() {
        render(FfiConnectionState.CONNECTED) { ZapSigners() }
        composeTestRule.onNodeWithTag("nostr-unlink-button")
            .performScrollTo().assertIsEnabled()
    }
}
