package com.fauna.app.ui.screen.settings

import android.content.Context
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.viewmodel.AdminNestVM
import com.fauna.ffi.FfiAdminRegionView
import com.fauna.ffi.FfiIssuerForcedArm
import com.fauna.ffi.FfiIssuerForcedConfirmView
import com.fauna.ffi.FfiIssuerKeyRow
import com.fauna.ffi.FfiIssuerKeyView
import com.fauna.ffi.FfiSeedRotationConfirmView
import com.fauna.ffi.FfiSeedRotationInheritor
import com.fauna.ffi.FfiTakedownFormView
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.NodeMode

/**
 * Compose-level coverage for the stateless [AdminNestContent] (the admin
 * `admin-nest` page, admin.md § N Nest): the admin pairing toggle + status
 * badge, the NAT-mode control (radios + save + status, admin.md § Nest →
 * NAT-mode control), the Factory Reset danger zone (button → confirm dialog),
 * and the page-level error element. Renders with seeded state — no Hilt, no
 * VM, no native `AdminNatModeMachine` construction (the NAT snapshot fields
 * are passed in directly; `localized()` resolution is pure Kotlin, no FFI).
 * Replaces the retired `AdminServicesContentTest` (the admin-services page it
 * tested no longer exists; its one live toggle — pairing — moved here). The
 * cross-app `test_admin_nest.py` / `test_admin_nat_mode.py --client
 * android` are the standing gates once the host emulator lands.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminNestContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        pairing: Boolean = true,
        servingPort: Int = 443,
        frontedByRouter: Boolean = false,
        osSecurityUpdates: Int = 0,
        osRebootPending: Boolean = false,
        restartingNow: Boolean = false,
        working: Boolean = false,
        error: String? = null,
        natSelectedMode: NodeMode = NodeMode.PUBLIC,
        natMessage: LocalizedText? = null,
        natSubmitEnabled: Boolean = true,
        natSubmitting: Boolean = false,
        regionView: FfiAdminRegionView? = null,
        regionWorking: Boolean = false,
        seedRotateConfirm: AdminNestVM.SeedRotateConfirmState? = null,
        seedRotateStatus: String? = null,
        takedownArmed: AdminNestVM.TakedownArmed? = null,
        takedownStatus: String? = null,
        // FFI-free reimplementation of the shared `takedown_form_view` fold
        // (`libs/fauna-client-moderation/src/takedown.rs`, unit-tested in
        // shared Rust — the four decisions its module doc enumerates) —
        // mirrors the parsePort/parseRegionCode stubs above, keeping this
        // test off the native FFI.
        takedownFormView: (String, Boolean, String, Boolean) -> FfiTakedownFormView =
            { contentId, _, reference, restore ->
                val trimmedId = contentId.trim()
                val trimmedRef = reference.trim()
                val blockedReason = when {
                    trimmedId.isEmpty() -> LocalizedText("admin.nest_page.takedown_blocked_no_content", emptyMap())
                    !restore && trimmedRef.isEmpty() ->
                        LocalizedText("admin.nest_page.takedown_blocked_no_reference", emptyMap())
                    else -> null
                }
                val suffix = if (restore) "restore" else "takedown"
                FfiTakedownFormView(
                    canSubmit = blockedReason == null,
                    blockedReason = blockedReason,
                    armLabel = LocalizedText("admin.nest_page.takedown_arm_$suffix", emptyMap()),
                    confirmSummary = LocalizedText("admin.nest_page.takedown_confirm_$suffix", emptyMap()),
                    confirmLabel = LocalizedText("admin.nest_page.takedown_confirm_button_$suffix", emptyMap()),
                )
            },
        onSetPairing: (Boolean) -> Unit = {},
        onSaveServingPort: (Int) -> Unit = {},
        onInvalidServingPort: () -> Unit = {},
        onRestartNow: () -> Unit = {},
        onSelectNatMode: (NodeMode) -> Unit = {},
        onSaveNatMode: () -> Unit = {},
        onSaveRegion: (String) -> Unit = {},
        onWithdrawRegion: () -> Unit = {},
        onInvalidRegion: () -> Unit = {},
        onArmSeedRotate: () -> Unit = {},
        onCancelSeedRotate: () -> Unit = {},
        onConfirmSeedRotate: () -> Unit = {},
        onArmTakedown: (String, Boolean, String, Boolean, FfiTakedownFormView) -> Unit = { _, _, _, _, _ -> },
        onCancelTakedown: () -> Unit = {},
        onConfirmTakedown: () -> Unit = {},
        oauthKeys: AdminNestVM.OauthKeysRead = AdminNestVM.OauthKeysRead.Unread,
        oauthArmed: AdminNestVM.OauthArmed? = null,
        oauthStatus: String? = null,
        oauthInFlight: Boolean = false,
        onRotateIssuerKey: () -> Unit = {},
        onArmOauthForced: (FfiIssuerForcedArm) -> Unit = {},
        onCancelOauthForced: () -> Unit = {},
        onConfirmOauthForced: (FfiIssuerForcedArm) -> Unit = {},
        // FFI-free reimplementations of the shared `issuer_key_row_label` /
        // `issuer_key_rotate_cost` folds (`libs/fauna-client-admin/src/lib.rs`,
        // unit-tested in shared Rust — including the round-UP whole minutes
        // this copies) — same reason as the takedown stub above. This test's
        // subject is the render: which element carries which fold's sentence.
        issuerKeyRowLabel: (FfiIssuerKeyRow, Long) -> LocalizedText = { row, nowSecs ->
            val until = row.servedUntil
            when {
                row.signing ->
                    LocalizedText("admin.nest_page.oauth_key_signing", mapOf("kid" to row.kid))
                until != null && until > nowSecs -> LocalizedText(
                    "admin.nest_page.oauth_key_retiring",
                    mapOf("kid" to row.kid, "minutes" to ((until - nowSecs + 59) / 60).toString()),
                )
                else -> LocalizedText("admin.nest_page.oauth_key_retired", mapOf("kid" to row.kid))
            }
        },
        issuerKeyRotateCost: (FfiIssuerKeyView) -> LocalizedText = { view ->
            LocalizedText(
                "admin.nest_page.oauth_rotate_desc",
                mapOf("minutes" to ((view.retirementHorizonSecs.toLong() + 59) / 60).toString()),
            )
        },
        onFactoryReset: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            AdminNestContent(
                pairing = pairing,
                servingPort = servingPort,
                frontedByRouter = frontedByRouter,
                osSecurityUpdates = osSecurityUpdates,
                osRebootPending = osRebootPending,
                restartingNow = restartingNow,
                working = working,
                error = error,
                natSelectedMode = natSelectedMode,
                natMessage = natMessage,
                natSubmitEnabled = natSubmitEnabled,
                natSubmitting = natSubmitting,
                regionView = regionView,
                regionWorking = regionWorking,
                seedRotateConfirm = seedRotateConfirm,
                seedRotateStatus = seedRotateStatus,
                takedownArmed = takedownArmed,
                takedownStatus = takedownStatus,
                oauthKeys = oauthKeys,
                oauthArmed = oauthArmed,
                oauthStatus = oauthStatus,
                oauthInFlight = oauthInFlight,
                onBack = {},
                onSetPairing = onSetPairing,
                onSaveServingPort = onSaveServingPort,
                onInvalidServingPort = onInvalidServingPort,
                onRestartNow = onRestartNow,
                onSelectNatMode = onSelectNatMode,
                onSaveNatMode = onSaveNatMode,
                onSaveRegion = onSaveRegion,
                onWithdrawRegion = onWithdrawRegion,
                onInvalidRegion = onInvalidRegion,
                onArmSeedRotate = onArmSeedRotate,
                onCancelSeedRotate = onCancelSeedRotate,
                onConfirmSeedRotate = onConfirmSeedRotate,
                onArmTakedown = onArmTakedown,
                onCancelTakedown = onCancelTakedown,
                onConfirmTakedown = onConfirmTakedown,
                onRotateIssuerKey = onRotateIssuerKey,
                onArmOauthForced = onArmOauthForced,
                onCancelOauthForced = onCancelOauthForced,
                onConfirmOauthForced = onConfirmOauthForced,
                onFactoryReset = onFactoryReset,
                // FFI-free stub of the shared `parse_port` (range logic is unit-tested
                // in shared Rust); keeps the serving-port save path off the native FFI.
                parsePort = { it.toIntOrNull()?.takeIf { p -> p in 1..65535 } },
                // FFI-free stub of the shared `admin_parse_region_code` (2-8 chars, each
                // an uppercase ASCII letter or digit, no case-fold — unit-tested in
                // shared Rust); keeps the region save path off the native FFI.
                parseRegionCode = {
                    it.takeIf { code ->
                        code.length in 2..8 && code.all { c -> c in 'A'..'Z' || c in '0'..'9' }
                    }
                },
                takedownFormView = takedownFormView,
                issuerKeyRowLabel = issuerKeyRowLabel,
                issuerKeyRotateCost = issuerKeyRotateCost,
            )
        }
    }

    /** A declared-region view with the given code, no authority/staleness/
     *  withdraw — the minimal "just declared" shape most tests need. */
    private fun regionView(
        declared: String? = null,
        canWithdraw: Boolean = declared != null,
        authority: LocalizedText? = null,
        staleness: LocalizedText? = null,
    ) = FfiAdminRegionView(
        declared = declared,
        status = LocalizedText(
            key = if (declared == null) "admin.nest_page.region_none" else "admin.nest_page.region_declared",
            args = if (declared == null) emptyMap() else mapOf("region" to declared),
        ),
        authority = authority,
        staleness = staleness,
        canWithdraw = canWithdraw,
    )

    @Test
    fun rendersHeadingControlsAndNavBack() {
        render()
        composeTestRule.onNodeWithTag("admin-nest-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        composeTestRule.onNodeWithTag("admin-service-pairing-toggle").assertExists()
        composeTestRule.onNodeWithTag("admin-service-pairing-status").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-serving-port-input").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-serving-port-save-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-region-section").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-region-status").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-region-input").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-region-save-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-public-radio").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-private-radio").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-save-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-status").assertExists()
        composeTestRule.onNodeWithTag("nest-os-maintenance-status").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-section").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-oauth-section").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-oauth-rotate-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-oauth-force-rotate-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-oauth-secret-force-rotate-button").assertExists()
        composeTestRule.onNodeWithTag("admin-factory-reset-section").assertExists()
        composeTestRule.onNodeWithTag("admin-factory-reset-button").assertExists()
        // `error-message` is deliberately ABSENT on a clean page — see
        // [errorMessageIsAbsentUntilThereIsAnError].
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
    }

    /**
     * `e2e-conventions.md` convention 2's rider, obligation (a): a shim carrying
     * the shared `error-message` id must LEAVE the tree when it has nothing to
     * say. This page used to render an empty `Box` under the id whenever [error]
     * was null, which made `is_visible("error-message")` structurally true on a
     * clean page — so the `assert not is_visible("error-message")` that every
     * such e2e test opens with could never fail (the bridge resolves visibility
     * as bare existence: `ElementOps.isVisible` = `findAll(id).isNotEmpty()`).
     */
    @Test
    fun errorMessageIsAbsentUntilThereIsAnError() {
        render()
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
    }

    @Test
    fun pairingToggleReflectsOnState() {
        render(pairing = true)
        composeTestRule.onNodeWithTag("admin-service-pairing-toggle").assertIsOn()
    }

    @Test
    fun pairingToggleReflectsOffState() {
        render(pairing = false)
        composeTestRule.onNodeWithTag("admin-service-pairing-toggle").assertIsOff()
    }

    @Test
    fun pairingToggleFiresWithNewValue() {
        var got: Boolean? = null
        render(pairing = false, onSetPairing = { got = it })
        composeTestRule.onNodeWithTag("admin-service-pairing-toggle").performScrollTo().performClick()
        assertEquals(true, got)
    }

    @Test
    fun pairingToggleDisabledWhileWorking() {
        render(working = true)
        composeTestRule.onNodeWithTag("admin-service-pairing-toggle").assertIsNotEnabled()
    }

    @Test
    fun factoryResetConfirmFlowFires() {
        var reset = false
        render(onFactoryReset = { reset = true })
        // Confirm button lives in the dialog; absent until the button opens it.
        composeTestRule.onNodeWithTag("admin-factory-reset-confirm-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-factory-reset-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-factory-reset-confirm-button").assertExists()
        composeTestRule.onNodeWithTag("admin-factory-reset-confirm-button").performClick()
        assertEquals(true, reset)
    }

    @Test
    fun errorRenders() {
        render(error = "services.update failed")
        composeTestRule.onNodeWithTag("error-message").assertTextEquals("services.update failed")
    }

    @Test
    fun servingPortSaveFiresWithSeededValue() {
        var saved: Int? = null
        render(servingPort = 8443, onSaveServingPort = { saved = it })
        composeTestRule.onNodeWithTag("admin-nest-serving-port-save-button")
            .performScrollTo().performClick()
        assertEquals(8443, saved)
    }

    @Test
    fun servingPortSaveParsesTypedValue() {
        var saved: Int? = null
        render(servingPort = 443, onSaveServingPort = { saved = it })
        composeTestRule.onNodeWithTag("admin-nest-serving-port-input")
            .performScrollTo().performTextReplacement("9443")
        composeTestRule.onNodeWithTag("admin-nest-serving-port-save-button").performClick()
        assertEquals(9443, saved)
    }

    @Test
    fun servingPortInvalidReportsAndDoesNotSave() {
        var saved: Int? = null
        var invalid = false
        render(onSaveServingPort = { saved = it }, onInvalidServingPort = { invalid = true })
        composeTestRule.onNodeWithTag("admin-nest-serving-port-input")
            .performScrollTo().performTextReplacement("70000") // > 65535 → invalid u16
        composeTestRule.onNodeWithTag("admin-nest-serving-port-save-button").performClick()
        assertEquals(true, invalid)
        assertEquals(null, saved)
    }

    @Test
    fun servingPortEditableOnDirectListener() {
        // The default (`fronted_by_router = false`): a direct-listener nest where
        // the port is a genuine admin choice → field + save button stay enabled.
        render(frontedByRouter = false)
        composeTestRule.onNodeWithTag("admin-nest-serving-port-input").assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-nest-serving-port-save-button")
            .performScrollTo().assertIsEnabled()
    }

    // ── Declared region (fauna.admin.region.{get,set}) ──

    @Test
    fun regionUndeclaredShowsNoneAndNoAuthorityOrWithdraw() {
        render(regionView = null)
        composeTestRule.onNodeWithTag("admin-nest-region-status").performScrollTo()
            .assertTextEquals("No region declared")
        composeTestRule.onNodeWithTag("admin-nest-region-authority").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-region-staleness").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-region-withdraw-button").assertDoesNotExist()
    }

    @Test
    fun regionDeclaredShowsAuthorityAndWithdraw() {
        render(
            regionView = regionView(
                declared = "NO",
                authority = LocalizedText(
                    key = "admin.nest_page.region_not_enrolled",
                    args = emptyMap(),
                ),
            )
        )
        composeTestRule.onNodeWithTag("admin-nest-region-authority").performScrollTo()
            .assertTextEquals("No authority is enrolled for this region, so no region rules apply here.")
        composeTestRule.onNodeWithTag("admin-nest-region-withdraw-button").assertExists()
    }

    @Test
    fun regionStalenessRendersOnlyWhenPresent() {
        render(
            regionView = regionView(
                declared = "NO",
                staleness = LocalizedText(key = "admin.nest_page.region_stale", args = emptyMap()),
            )
        )
        composeTestRule.onNodeWithTag("admin-nest-region-staleness").performScrollTo()
            .assertTextEquals(
                "Haven't been able to check for updated region rules recently. The " +
                    "rules already received stay in force."
            )
    }

    @Test
    fun regionSaveFiresWithParsedCode() {
        var saved: String? = null
        render(onSaveRegion = { saved = it })
        composeTestRule.onNodeWithTag("admin-nest-region-input")
            .performScrollTo().performTextReplacement("NO")
        composeTestRule.onNodeWithTag("admin-nest-region-save-button")
            .performScrollTo().performClick()
        assertEquals("NO", saved)
    }

    @Test
    fun regionInvalidCodeReportsAndDoesNotSave() {
        var saved: String? = null
        var invalid = false
        render(onSaveRegion = { saved = it }, onInvalidRegion = { invalid = true })
        composeTestRule.onNodeWithTag("admin-nest-region-input")
            .performScrollTo().performTextReplacement("no") // lower-case → rejected, no case-fold
        composeTestRule.onNodeWithTag("admin-nest-region-save-button")
            .performScrollTo().performClick()
        assertEquals(true, invalid)
        assertEquals(null, saved)
    }

    @Test
    fun regionWithdrawButtonFires() {
        var withdrawn = false
        render(regionView = regionView(declared = "NO"), onWithdrawRegion = { withdrawn = true })
        composeTestRule.onNodeWithTag("admin-nest-region-withdraw-button")
            .performScrollTo().performClick()
        assertEquals(true, withdrawn)
    }

    @Test
    fun regionInputDisabledWhileWorking() {
        render(regionWorking = true)
        composeTestRule.onNodeWithTag("admin-nest-region-input").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-nest-region-save-button")
            .performScrollTo().assertIsNotEnabled()
    }

    // ── NAT-mode control (fauna.setup.nat_mode via the shared AdminNatModeMachine) ──

    @Test
    fun natModeRadioReflectsSelectedMode() {
        render(natSelectedMode = NodeMode.PUBLIC)
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-public-radio").assertIsSelected()
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-private-radio").assertIsNotSelected()
    }

    @Test
    fun natModeRadioReflectsPrivateSelection() {
        render(natSelectedMode = NodeMode.PRIVATE)
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-private-radio").assertIsSelected()
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-public-radio").assertIsNotSelected()
    }

    @Test
    fun natModeRadioFiresSelectedMode() {
        var got: NodeMode? = null
        render(natSelectedMode = NodeMode.PUBLIC, onSelectNatMode = { got = it })
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-private-radio")
            .performScrollTo().performClick()
        assertEquals(NodeMode.PRIVATE, got)
    }

    @Test
    fun natModeSaveButtonFiresWhenEnabled() {
        var saved = false
        render(natSubmitEnabled = true, onSaveNatMode = { saved = true })
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-save-button")
            .performScrollTo().performClick()
        assertEquals(true, saved)
    }

    @Test
    fun natModeSaveButtonDisabledWhenSubmitDisabled() {
        render(natSubmitEnabled = false)
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-save-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun natModeRadiosDisabledWhileSubmitting() {
        render(natSubmitting = true)
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-public-radio").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-private-radio").assertIsNotEnabled()
    }

    @Test
    fun natModeStatusRendersResolvedMessage() {
        render(natMessage = LocalizedText(key = "admin.nest_page.nat_mode_saved", args = emptyMap()))
        composeTestRule.onNodeWithTag("admin-nest-nat-mode-status").performScrollTo()
            .assertTextEquals(
                "Network mode saved. Mail serving updated now; certificates and " +
                    "connectivity re-check at the next restart."
            )
    }

    // ── Host-OS maintenance (nest-os-*, installers/vps.md § Host OS Maintenance § 4) ──

    @Test
    fun osMaintenanceStatusAlwaysPresentAndBadgeButtonHiddenWhenIdle() {
        // The default (no host channel): the status line renders; neither the
        // count badge nor the restart-now button shows (nothing pending).
        render(osSecurityUpdates = 0, osRebootPending = false)
        composeTestRule.onNodeWithTag("nest-os-maintenance-status").performScrollTo().assertExists()
        composeTestRule.onNodeWithTag("nest-os-updates-count").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nest-os-restart-now-button").assertDoesNotExist()
    }

    @Test
    fun osUpdatesCountShowsRawCountWhenPending() {
        // The badge is a focused integer split off the categorical status line.
        render(osSecurityUpdates = 3)
        composeTestRule.onNodeWithTag("nest-os-updates-count")
            .performScrollTo().assertTextEquals("3")
    }

    @Test
    fun osRestartButtonShowsAndFiresWhenRebootPending() {
        var fired = false
        render(osRebootPending = true, onRestartNow = { fired = true })
        composeTestRule.onNodeWithTag("nest-os-restart-now-button")
            .performScrollTo().assertExists().performClick()
        assertEquals(true, fired)
    }

    @Test
    fun osRestartButtonDisabledWhileRestarting() {
        render(osRebootPending = true, restartingNow = true)
        composeTestRule.onNodeWithTag("nest-os-restart-now-button")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun servingPortReadOnlyWhenRouterFronted() {
        // Behind the cloud :443 SNI router the chosen port is inert (the nest
        // rejects a write) → field + save button disabled, hint shown.
        render(frontedByRouter = true)
        composeTestRule.onNodeWithTag("admin-nest-serving-port-input").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-nest-serving-port-save-button")
            .performScrollTo().assertIsNotEnabled()
    }

    // ── Deployment-identity rotation (admin-nest-seed-rotate-*) ──

    private fun inheritor(label: String) =
        FfiSeedRotationInheritor(actorId = ByteArray(32), label = label)

    @Test
    fun seedRotateUnarmedShowsOnlyTheArmButton() {
        render(seedRotateConfirm = null)
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-cancel-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-confirm-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-roster-item-0").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-roster-reason").assertDoesNotExist()
    }

    @Test
    fun seedRotateArmButtonFires() {
        var armed = false
        render(onArmSeedRotate = { armed = true })
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-button").performScrollTo().performClick()
        assertEquals(true, armed)
    }

    /** The armed-but-loading state: confirm exists (disabled — the e2e journey
     *  asserts this in the same frame the arm click returns), roster-reason
     *  shows the loading text, no roster items yet. */
    @Test
    fun seedRotateLoadingShowsDisabledConfirmAndNoRosterYet() {
        render(seedRotateConfirm = AdminNestVM.SeedRotateConfirmState.Loading)
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-confirm-button")
            .performScrollTo().assertExists().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-cancel-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-roster-item-0").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-roster-reason")
            .assertTextEquals("Checking who currently administers this nest…")
    }

    @Test
    fun seedRotateFailedShowsReasonAndDisabledConfirm() {
        render(seedRotateConfirm = AdminNestVM.SeedRotateConfirmState.Failed("boom"))
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-confirm-button")
            .performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-roster-reason").assertTextEquals("boom")
    }

    /** The resolved, confirmable roster: items index-parallel, no reason
     *  (mutually exclusive per ui.yaml's ordering rule), confirm enabled. */
    @Test
    fun seedRotateReadyShowsRosterAndEnablesConfirm() {
        val view = FfiSeedRotationConfirmView(
            inheritors = listOf(inheritor("alice@nest.test")),
            canConfirm = true,
            blockedReason = null,
        )
        render(seedRotateConfirm = AdminNestVM.SeedRotateConfirmState.Ready(view))
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-roster-item-0")
            .performScrollTo().assertTextEquals("alice@nest.test")
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-roster-item-1").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-roster-reason").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-confirm-button").assertIsEnabled()
    }

    /** A withheld confirm (e.g. an empty roster) renders the nest's own
     *  `blockedReason`, and confirm stays disabled even though the state is
     *  [AdminNestVM.SeedRotateConfirmState.Ready] — `canConfirm` gates it,
     *  not the arm/loading/failed distinction. */
    @Test
    fun seedRotateReadyButBlockedShowsReasonAndDisablesConfirm() {
        val view = FfiSeedRotationConfirmView(
            inheritors = emptyList(),
            canConfirm = false,
            blockedReason = LocalizedText(
                key = "admin.nest_page.rotate_seed_roster_empty",
                args = emptyMap(),
            ),
        )
        render(seedRotateConfirm = AdminNestVM.SeedRotateConfirmState.Ready(view))
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-roster-reason")
            .performScrollTo().assertTextEquals(
                "This nest reported no administrators, which can't be right. Nothing " +
                    "was rotated — reload this page and try again."
            )
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-confirm-button").assertIsNotEnabled()
    }

    @Test
    fun seedRotateCancelButtonFires() {
        var cancelled = false
        render(
            seedRotateConfirm = AdminNestVM.SeedRotateConfirmState.Loading,
            onCancelSeedRotate = { cancelled = true },
        )
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-cancel-button")
            .performScrollTo().performClick()
        assertEquals(true, cancelled)
    }

    @Test
    fun seedRotateConfirmButtonFiresWhenEnabled() {
        var confirmed = false
        val view = FfiSeedRotationConfirmView(
            inheritors = listOf(inheritor("alice@nest.test")),
            canConfirm = true,
            blockedReason = null,
        )
        render(
            seedRotateConfirm = AdminNestVM.SeedRotateConfirmState.Ready(view),
            onConfirmSeedRotate = { confirmed = true },
        )
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-confirm-button")
            .performScrollTo().performClick()
        assertEquals(true, confirmed)
    }

    @Test
    fun seedRotateStatusAbsentUntilAnAttempt() {
        render(seedRotateStatus = null)
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-status").assertDoesNotExist()
    }

    @Test
    fun seedRotateStatusRendersAfterAnAttempt() {
        render(seedRotateStatus = "Rotating the deployment identity…")
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-status")
            .performScrollTo().assertTextEquals("Rotating the deployment identity…")
    }

    // ── Legal takedown (admin-nest-takedown-*; moderation.md
    // § Legal takedown → Invocation surface) — android's e2e run stays
    // emulator-host-gated like every android journey; this
    // Robolectric render contract is the interim verification until the
    // emulator lands, exactly as its cross-app twin `test_admin_legal_
    // takedown.py --app android` will be once it does.

    @Test
    fun takedownUnarmedShowsOnlyTheArmButton() {
        render(takedownArmed = null)
        composeTestRule.onNodeWithTag("admin-nest-takedown-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-takedown-cancel-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-takedown-confirm-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-takedown-confirm-summary").assertDoesNotExist()
    }

    /** The structural guard, rendered: an empty form's arm button is
     *  disabled (the shared fold's `can_submit`), mirroring the e2e
     *  journey's `assert not app.driver.is_enabled(...)` on a fresh page. */
    @Test
    fun takedownEmptyFormDisablesTheArmButton() {
        render()
        composeTestRule.onNodeWithTag("admin-nest-takedown-button")
            .performScrollTo().assertIsNotEnabled()
    }

    /** Decision 1 in `takedown.rs`'s module doc: a citation-less takedown is
     *  never armable, even with a content id typed. */
    @Test
    fun takedownContentIdAloneStaysDisabled() {
        render()
        composeTestRule.onNodeWithTag("admin-nest-takedown-content-id-input")
            .performScrollTo().performTextInput("a".repeat(64))
        composeTestRule.onNodeWithTag("admin-nest-takedown-button").assertIsNotEnabled()
    }

    @Test
    fun takedownContentIdAndReferenceEnableTheArmButton() {
        render()
        composeTestRule.onNodeWithTag("admin-nest-takedown-content-id-input")
            .performScrollTo().performTextInput("a".repeat(64))
        composeTestRule.onNodeWithTag("admin-nest-takedown-reference-input")
            .performScrollTo().performTextInput("Court order 42/2026")
        composeTestRule.onNodeWithTag("admin-nest-takedown-button")
            .performScrollTo().assertIsEnabled()
    }

    /** Decision 2's asymmetry: restore mode arms with an empty reference —
     *  the overturn note is optional, unlike the takedown's citation. */
    @Test
    fun takedownRestoreWithNoReferenceStaysArmable() {
        render()
        composeTestRule.onNodeWithTag("admin-nest-takedown-content-id-input")
            .performScrollTo().performTextInput("a".repeat(64))
        composeTestRule.onNodeWithTag("admin-nest-takedown-restore-checkbox")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-nest-takedown-button")
            .performScrollTo().assertIsEnabled()
    }

    @Test
    fun takedownArmButtonFiresWithTheTypedForm() {
        var fired: List<Any?>? = null
        render(onArmTakedown = { id, conversation, reference, restore, view ->
            fired = listOf(id, conversation, reference, restore, view.canSubmit)
        })
        composeTestRule.onNodeWithTag("admin-nest-takedown-content-id-input")
            .performScrollTo().performTextInput("a".repeat(64))
        composeTestRule.onNodeWithTag("admin-nest-takedown-reference-input")
            .performScrollTo().performTextInput("Court order 42/2026")
        composeTestRule.onNodeWithTag("admin-nest-takedown-button").performScrollTo().performClick()
        assertEquals(listOf("a".repeat(64), false, "Court order 42/2026", false, true), fired)
    }

    /** The confirm is the decision surface: it renders the armed fold's own
     *  summary, and offers confirm + cancel — never the arm button again. */
    @Test
    fun takedownArmedShowsTheConfirmSurface() {
        val view = FfiTakedownFormView(
            canSubmit = true,
            blockedReason = null,
            armLabel = LocalizedText("admin.nest_page.takedown_arm_takedown", emptyMap()),
            confirmSummary = LocalizedText(
                "admin.nest_page.takedown_confirm_takedown",
                mapOf("content_type" to "post", "content_id" to "ab".repeat(32), "reference" to "Court order 42/2026"),
            ),
            confirmLabel = LocalizedText("admin.nest_page.takedown_confirm_button_takedown", emptyMap()),
        )
        render(
            takedownArmed = AdminNestVM.TakedownArmed(
                contentId = "ab".repeat(32),
                conversation = false,
                legalReference = "Court order 42/2026",
                restore = false,
                view = view,
            ),
        )
        composeTestRule.onNodeWithTag("admin-nest-takedown-confirm-summary")
            .performScrollTo().assertTextContains("Court order 42/2026", substring = true)
        composeTestRule.onNodeWithTag("admin-nest-takedown-confirm-button").assertIsEnabled()
        composeTestRule.onNodeWithTag("admin-nest-takedown-cancel-button").assertExists()
    }

    @Test
    fun takedownCancelButtonFires() {
        var cancelled = false
        val view = FfiTakedownFormView(
            canSubmit = true,
            blockedReason = null,
            armLabel = LocalizedText("admin.nest_page.takedown_arm_takedown", emptyMap()),
            confirmSummary = LocalizedText("admin.nest_page.takedown_confirm_takedown", emptyMap()),
            confirmLabel = LocalizedText("admin.nest_page.takedown_confirm_button_takedown", emptyMap()),
        )
        render(
            takedownArmed = AdminNestVM.TakedownArmed("ab".repeat(32), false, "ref", false, view),
            onCancelTakedown = { cancelled = true },
        )
        composeTestRule.onNodeWithTag("admin-nest-takedown-cancel-button")
            .performScrollTo().performClick()
        assertEquals(true, cancelled)
    }

    @Test
    fun takedownConfirmButtonFires() {
        var confirmed = false
        val view = FfiTakedownFormView(
            canSubmit = true,
            blockedReason = null,
            armLabel = LocalizedText("admin.nest_page.takedown_arm_takedown", emptyMap()),
            confirmSummary = LocalizedText("admin.nest_page.takedown_confirm_takedown", emptyMap()),
            confirmLabel = LocalizedText("admin.nest_page.takedown_confirm_button_takedown", emptyMap()),
        )
        render(
            takedownArmed = AdminNestVM.TakedownArmed("ab".repeat(32), false, "ref", false, view),
            onConfirmTakedown = { confirmed = true },
        )
        composeTestRule.onNodeWithTag("admin-nest-takedown-confirm-button")
            .performScrollTo().performClick()
        assertEquals(true, confirmed)
    }

    @Test
    fun takedownStatusAbsentUntilAnAttempt() {
        render(takedownStatus = null)
        composeTestRule.onNodeWithTag("admin-nest-takedown-status").assertDoesNotExist()
    }

    @Test
    fun takedownStatusRendersAfterAnAttempt() {
        render(takedownStatus = "Submitting…")
        composeTestRule.onNodeWithTag("admin-nest-takedown-status")
            .performScrollTo().assertTextEquals("Submitting…")
    }

    // ── Outside-app sign-in keys (admin-nest-oauth-*; authorization-server.md
    // § The issuer → Two rotation arms). The render contract tui's
    // `admin/nest.rs` tests pin for the lead app, restated over android's
    // stateless content: rows only from an answered read, three controls
    // disabled-never-hidden, one shared confirm painting what was CAPTURED
    // and firing the arm it was rendered for, a verdict that is never the
    // page's error-message. android's e2e run of the cross-app journey
    // (`test_admin_oauth_issuer_keys.py --app android`) stays emulator-host-gated
    // like every android journey; this is the interim verification.

    private val oauthButtons = listOf(
        "admin-nest-oauth-rotate-button",
        "admin-nest-oauth-force-rotate-button",
        "admin-nest-oauth-secret-force-rotate-button",
    )

    private val oauthConfirmMembers = listOf(
        "admin-nest-oauth-confirm-summary",
        "admin-nest-oauth-confirm-button",
        "admin-nest-oauth-cancel-button",
    )

    private fun ctx(): Context = ApplicationProvider.getApplicationContext()

    private fun nowSecs(): Long = System.currentTimeMillis() / 1000

    /** The signer, plus — when [retiringFor] is given — one retired key still
     *  served for that many more seconds (counted from the real clock, which
     *  the paint counts against). */
    private fun keySet(retiringFor: Long? = null): FfiIssuerKeyView {
        val keys = mutableListOf(
            FfiIssuerKeyRow(kid = "kid-new", signing = true, retiredAt = null, servedUntil = null),
        )
        if (retiringFor != null) {
            val until = nowSecs() + retiringFor
            keys += FfiIssuerKeyRow(
                kid = "kid-old",
                signing = false,
                retiredAt = until - 1_200,
                servedUntil = until,
            )
        }
        return FfiIssuerKeyView(
            activeKid = "kid-new",
            keys = keys,
            retirementHorizonSecs = 1_200uL,
            rotationInFlight = keys.size > 1,
        )
    }

    private fun ready(retiringFor: Long? = null) = AdminNestVM.OauthKeysRead.Ready(keySet(retiringFor))

    private fun armed(arm: FfiIssuerForcedArm, keyCount: Int = 2) = AdminNestVM.OauthArmed(
        arm = arm,
        confirm = when (arm) {
            FfiIssuerForcedArm.ISSUER_KEY -> FfiIssuerForcedConfirmView(
                summary = LocalizedText(
                    "admin.nest_page.oauth_force_rotate_confirm_many",
                    mapOf("count" to keyCount.toString()),
                ),
                confirmLabel = LocalizedText("admin.nest_page.oauth_force_rotate_confirm_button", emptyMap()),
            )
            FfiIssuerForcedArm.SESSION_SECRET -> FfiIssuerForcedConfirmView(
                summary = LocalizedText("admin.nest_page.oauth_secret_force_rotate_confirm", emptyMap()),
                confirmLabel = LocalizedText(
                    "admin.nest_page.oauth_secret_force_rotate_confirm_button",
                    emptyMap(),
                ),
            )
        },
    )

    /** Until the key set answers, the section paints the loading reason and NO
     *  key rows — "not asked yet" must not read as "no keys" — and all three
     *  controls are present but disabled (never hidden). */
    @Test
    fun oauthUnreadPaintsTheLoadingReasonNoRowsAndDisabledControls() {
        render(oauthKeys = AdminNestVM.OauthKeysRead.Unread)
        composeTestRule.onNodeWithTag("admin-nest-oauth-key-reason").performScrollTo()
            .assertTextEquals(ctx().getString(R.string.admin_nest_page_oauth_keys_loading))
        composeTestRule.onNodeWithTag("admin-nest-oauth-key-item-0").assertDoesNotExist()
        oauthButtons.forEach { tag ->
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertExists().assertIsNotEnabled()
        }
    }

    /** "Couldn't find out" gets the worded reason line, the same zero rows,
     *  and the same disabled controls. */
    @Test
    fun oauthFailedReadPaintsItsReasonNoRowsAndDisabledControls() {
        val reason = ctx().getStringFmt(R.string.admin_nest_page_oauth_keys_error, "unknown kind")
        render(oauthKeys = AdminNestVM.OauthKeysRead.Failed(reason))
        composeTestRule.onNodeWithTag("admin-nest-oauth-key-reason").performScrollTo()
            .assertTextEquals(reason)
        assertTrue("the worded reason carries its cause", reason.contains("unknown kind"))
        composeTestRule.onNodeWithTag("admin-nest-oauth-key-item-0").assertDoesNotExist()
        oauthButtons.forEach { tag ->
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsNotEnabled()
        }
    }

    /** A failed key-set read is NON-FATAL: it lands on the section's own reason
     *  line, never on the page's `error-message`, and every other section keeps
     *  painting (any read error, e.g. a transport fault). */
    @Test
    fun oauthFailedReadLeavesTheRestOfThePagePainting() {
        render(oauthKeys = AdminNestVM.OauthKeysRead.Failed("no such kind"))
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-service-pairing-toggle").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-serving-port-input").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-region-section").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-seed-rotate-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-takedown-section").assertExists()
    }

    /** An answered set paints one row per key, signer first and in the given
     *  order, the retired key with its whole-minute countdown counted at paint;
     *  no reason line; the ordinary arm's cost beside it; live controls. */
    @Test
    fun oauthAnsweredSetPaintsARowPerKeyAndLiveControls() {
        val seen = mutableListOf<Long>()
        render(
            oauthKeys = ready(retiringFor = 600),
            issuerKeyRowLabel = { row, now ->
                seen += now
                if (row.signing) {
                    LocalizedText("admin.nest_page.oauth_key_signing", mapOf("kid" to row.kid))
                } else {
                    LocalizedText(
                        "admin.nest_page.oauth_key_retiring",
                        mapOf("kid" to row.kid, "minutes" to (((row.servedUntil ?: 0L) - now + 59) / 60).toString()),
                    )
                }
            },
        )
        composeTestRule.onNodeWithTag("admin-nest-oauth-key-item-0").performScrollTo()
            .assertTextEquals("kid-new — signing now")
        composeTestRule.onNodeWithTag("admin-nest-oauth-key-item-1").performScrollTo()
            .assertTextEquals("kid-old — replaced; still accepted for 10 min")
        composeTestRule.onNodeWithTag("admin-nest-oauth-key-item-2").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-nest-oauth-key-reason").assertDoesNotExist()
        // The fold is handed the wall clock at paint, in epoch SECONDS.
        assertTrue("row labels were folded at paint", seen.isNotEmpty())
        seen.forEach { now ->
            assertTrue("paint clock $now is epoch seconds", kotlin.math.abs(now - nowSecs()) < 120)
        }
        // The ordinary arm has no confirm, so its cost is stated beside it.
        composeTestRule.onNodeWithText(
            ctx().getStringFmt(R.string.admin_nest_page_oauth_rotate_desc, "20"),
        ).assertExists()
        oauthButtons.forEach { tag ->
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsEnabled()
        }
    }

    /** While a call is in flight every control desensitizes — each kind mints
     *  on the nest, and the ordinary arm has no confirm to disarm. */
    @Test
    fun oauthInFlightDisablesEveryControl() {
        render(oauthKeys = ready(), oauthInFlight = true)
        oauthButtons.forEach { tag ->
            composeTestRule.onNodeWithTag(tag).performScrollTo().assertIsNotEnabled()
        }
    }

    @Test
    fun oauthRotateButtonFiresTheOrdinaryRotation() {
        var rotated = 0
        val armedWith = mutableListOf<FfiIssuerForcedArm>()
        render(
            oauthKeys = ready(),
            onRotateIssuerKey = { rotated++ },
            onArmOauthForced = { armedWith += it },
        )
        composeTestRule.onNodeWithTag("admin-nest-oauth-rotate-button").performScrollTo().performClick()
        assertEquals(1, rotated)
        assertEquals(emptyList<FfiIssuerForcedArm>(), armedWith)
    }

    /** Each arm button arms ITS arm — and arming alone dispatches nothing. */
    @Test
    fun oauthArmButtonsArmTheirOwnArmAndDispatchNothing() {
        val armedWith = mutableListOf<FfiIssuerForcedArm>()
        var rotated = 0
        val confirmed = mutableListOf<FfiIssuerForcedArm>()
        render(
            oauthKeys = ready(),
            onArmOauthForced = { armedWith += it },
            onRotateIssuerKey = { rotated++ },
            onConfirmOauthForced = { confirmed += it },
        )
        composeTestRule.onNodeWithTag("admin-nest-oauth-force-rotate-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-nest-oauth-secret-force-rotate-button").performScrollTo().performClick()
        assertEquals(listOf(FfiIssuerForcedArm.ISSUER_KEY, FfiIssuerForcedArm.SESSION_SECRET), armedWith)
        assertEquals(0, rotated)
        assertEquals(emptyList<FfiIssuerForcedArm>(), confirmed)
    }

    /** Un-armed, none of the confirm's members exist, and no verdict paints
     *  before a control was used. */
    @Test
    fun oauthUnarmedHasNoConfirmAndNoStatus() {
        render(oauthKeys = ready(retiringFor = 600))
        (oauthConfirmMembers + "admin-nest-oauth-status").forEach { tag ->
            composeTestRule.onNodeWithTag(tag).assertDoesNotExist()
        }
    }

    /** The armed confirm paints the fold CAPTURED at arm time — here a two-key
     *  cost while the set now shows one key, which a re-fold would contradict —
     *  and its confirm fires exactly the arm it was rendered for. */
    @Test
    fun oauthArmedKeyConfirmPaintsTheCapturedCostAndFiresTheKeyArm() {
        val confirmed = mutableListOf<FfiIssuerForcedArm>()
        render(
            oauthKeys = ready(),
            oauthArmed = armed(FfiIssuerForcedArm.ISSUER_KEY, keyCount = 2),
            onConfirmOauthForced = { confirmed += it },
        )
        composeTestRule.onNodeWithTag("admin-nest-oauth-confirm-summary").performScrollTo()
            .assertTextEquals(ctx().getStringFmt(R.string.admin_nest_page_oauth_force_rotate_confirm_many, "2"))
        composeTestRule.onNodeWithTag("admin-nest-oauth-cancel-button").assertExists()
        composeTestRule.onNodeWithTag("admin-nest-oauth-confirm-button").performScrollTo()
            .assertTextEquals("Replace at once")
            .assertIsEnabled()
            .performClick()
        assertEquals(listOf(FfiIssuerForcedArm.ISSUER_KEY), confirmed)
    }

    @Test
    fun oauthArmedSecretConfirmPaintsTheReconsentCostAndFiresTheSecretArm() {
        val confirmed = mutableListOf<FfiIssuerForcedArm>()
        render(
            oauthKeys = ready(),
            oauthArmed = armed(FfiIssuerForcedArm.SESSION_SECRET),
            onConfirmOauthForced = { confirmed += it },
        )
        composeTestRule.onNodeWithTag("admin-nest-oauth-confirm-summary").performScrollTo()
            .assertTextEquals(ctx().getString(R.string.admin_nest_page_oauth_secret_force_rotate_confirm))
        composeTestRule.onNodeWithTag("admin-nest-oauth-confirm-button").performScrollTo()
            .assertTextEquals("End saved sign-ins")
            .performClick()
        assertEquals(listOf(FfiIssuerForcedArm.SESSION_SECRET), confirmed)
    }

    /** Cancel disarms and touches nothing: no confirm, no rotation, no arm. */
    @Test
    fun oauthCancelFiresOnlyTheCancel() {
        var cancelled = 0
        var rotated = 0
        val confirmed = mutableListOf<FfiIssuerForcedArm>()
        val armedWith = mutableListOf<FfiIssuerForcedArm>()
        render(
            oauthKeys = ready(),
            oauthArmed = armed(FfiIssuerForcedArm.ISSUER_KEY),
            onCancelOauthForced = { cancelled++ },
            onRotateIssuerKey = { rotated++ },
            onConfirmOauthForced = { confirmed += it },
            onArmOauthForced = { armedWith += it },
        )
        composeTestRule.onNodeWithTag("admin-nest-oauth-cancel-button").performScrollTo().performClick()
        assertEquals(1, cancelled)
        assertEquals(0, rotated)
        assertEquals(emptyList<FfiIssuerForcedArm>(), confirmed)
        assertEquals(emptyList<FfiIssuerForcedArm>(), armedWith)
    }

    /** The verdict renders on its own element — never the page's
     *  `error-message`, even when it words a failure. */
    @Test
    fun oauthStatusRendersTheVerdictNeverTheErrorMessage() {
        val verdict = ctx().getStringFmt(R.string.admin_nest_page_oauth_rotate_failed, "timed out")
        render(oauthKeys = ready(), oauthStatus = verdict)
        composeTestRule.onNodeWithTag("admin-nest-oauth-status").performScrollTo()
            .assertTextEquals(verdict)
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
    }
}
