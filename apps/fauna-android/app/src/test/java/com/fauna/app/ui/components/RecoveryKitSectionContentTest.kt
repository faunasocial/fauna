package com.fauna.app.ui.components

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.viewmodel.RecoveryKitUiState
import com.fauna.ffi.FfiRecoveryKitStatus
import com.fauna.ffi.FfiSweepCopy
import com.fauna.ffi.FfiSweepView
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.QrMatrix

/**
 * Compose-level coverage for the stateless [RecoveryKitSectionContent]
 * (`docs/goal/ui/settings.md` § Recovery kit) — one test per status the chain
 * read can answer, the two affordances that belong to a single state (the
 * escrow re-seal, the veto), and the shown-once kit display. No Hilt, no VM, no
 * FFI: the two native reads ([pendingDays], [qrMatrixOf]) are injected.
 *
 * Enablement is fed through the status record's `allows_*` fields exactly as
 * shared Rust computes them (`libs/fauna-ffi/src/recovery.rs`); the tests pin
 * that the section RENDERS them, never that it derives them.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class RecoveryKitSectionContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun status(
        kind: String,
        allowsCreate: Boolean = false,
        allowsReplace: Boolean = false,
        allowsLost: Boolean = false,
        allowsEscrowReseal: Boolean = false,
        pendingLandsAt: Long? = null,
    ) = FfiRecoveryKitStatus(
        kind = kind,
        allowsCreate = allowsCreate,
        allowsReplace = allowsReplace,
        allowsLost = allowsLost,
        // Unconditionally true in shared Rust — theft is exactly the no-kit case.
        allowsStolen = true,
        allowsEscrowReseal = allowsEscrowReseal,
        pendingNewPubkeyHex = pendingLandsAt?.let { "ab".repeat(32) },
        pendingLandsAt = pendingLandsAt,
    )

    private val clicks = mutableListOf<String>()

    private fun render(state: RecoveryKitUiState, qr: QrMatrix? = null) {
        composeTestRule.setContent {
            RecoveryKitSectionContent(
                state = state,
                onPhraseChange = {},
                onCreate = { clicks += "create" },
                onReplace = { clicks += "replace" },
                onLost = { clicks += "lost" },
                onEscrowReseal = { clicks += "reseal" },
                onVeto = { clicks += "veto" },
                pendingDays = { 12 },
                qrMatrixOf = { qr },
            )
        }
    }

    private fun node(tag: String) = composeTestRule.onNodeWithTag(tag, useUnmergedTree = true)

    private fun assertAbsent(tag: String) =
        composeTestRule.onNodeWithTag(tag, useUnmergedTree = true).assertDoesNotExist()

    @Test
    fun unreadStatusPaintsLoadingAndOffersNoAction() {
        render(RecoveryKitUiState(status = null))

        node(Ids.RECOVERY_KIT_SECTION).assertExists()
        node(Ids.RECOVERY_KIT_STATUS).assertTextContains("Checking your recovery kit", substring = true)
        // An unread status keeps the kit-in-hand field (the stolen ceremony needs
        // it most when the chain read fails) but claims no action.
        node(Ids.RECOVERY_ENTRY_PHRASE_FIELD).assertExists()
        assertAbsent(Ids.RECOVERY_KIT_CREATE_BUTTON)
        assertAbsent(Ids.RECOVERY_KIT_REPLACE_BUTTON)
        assertAbsent(Ids.RECOVERY_KIT_LOST_BUTTON)
    }

    @Test
    fun neverCreatedEnablesCreateOnly() {
        render(RecoveryKitUiState(status = status("never-created", allowsCreate = true)))

        node(Ids.RECOVERY_KIT_STATUS).assertTextContains("No recovery kit", substring = true)
        node(Ids.RECOVERY_KIT_CREATE_BUTTON).assertIsEnabled()
        node(Ids.RECOVERY_KIT_REPLACE_BUTTON).assertIsNotEnabled()
        node(Ids.RECOVERY_KIT_LOST_BUTTON).assertIsNotEnabled()
        assertAbsent(Ids.RECOVERY_KIT_ESCROW_RESEAL_BUTTON)
        assertAbsent(Ids.RECOVERY_PENDING_VETO_BUTTON)

        node(Ids.RECOVERY_KIT_CREATE_BUTTON).performClick()
        assertEquals(listOf("create"), clicks)
    }

    @Test
    fun registeredEnablesReplaceAndLost() {
        render(
            RecoveryKitUiState(
                status = status("registered", allowsReplace = true, allowsLost = true),
            ),
        )

        node(Ids.RECOVERY_KIT_CREATE_BUTTON).assertIsNotEnabled()
        node(Ids.RECOVERY_KIT_REPLACE_BUTTON).assertIsEnabled()
        node(Ids.RECOVERY_KIT_LOST_BUTTON).assertIsEnabled()
        assertAbsent(Ids.RECOVERY_KIT_ESCROW_RESEAL_BUTTON)
        assertAbsent(Ids.RECOVERY_PENDING_VETO_BUTTON)

        node(Ids.RECOVERY_KIT_REPLACE_BUTTON).performClick()
        node(Ids.RECOVERY_KIT_LOST_BUTTON).performClick()
        assertEquals(listOf("replace", "lost"), clicks)
    }

    @Test
    fun registeredNoEscrowRendersTheRepair() {
        render(
            RecoveryKitUiState(
                status = status(
                    "registered-no-escrow",
                    allowsReplace = true,
                    allowsLost = true,
                    allowsEscrowReseal = true,
                ),
            ),
        )

        node(Ids.RECOVERY_KIT_ESCROW_RESEAL_BUTTON).assertIsEnabled()
        assertAbsent(Ids.RECOVERY_PENDING_VETO_BUTTON)
        node(Ids.RECOVERY_KIT_ESCROW_RESEAL_BUTTON).performClick()
        assertEquals(listOf("reseal"), clicks)
    }

    @Test
    fun replacementPendingRendersTheCountdownAndTheVeto() {
        render(
            RecoveryKitUiState(
                status = status(
                    "replacement-pending",
                    // Replace stays allowed DURING the window: replacing with a
                    // held kit is how an owner ends it at once.
                    allowsReplace = true,
                    pendingLandsAt = 1_900_000_000L,
                ),
            ),
        )

        node(Ids.RECOVERY_KIT_STATUS).assertTextContains("12 days", substring = true)
        node(Ids.RECOVERY_KIT_REPLACE_BUTTON).assertIsEnabled()
        assertAbsent(Ids.RECOVERY_KIT_ESCROW_RESEAL_BUTTON)
        node(Ids.RECOVERY_PENDING_VETO_BUTTON).assertIsEnabled()
        node(Ids.RECOVERY_PENDING_VETO_BUTTON).performClick()
        assertEquals(listOf("veto"), clicks)
    }

    @Test
    fun busyDisablesEveryAction() {
        render(
            RecoveryKitUiState(
                status = status(
                    "replacement-pending",
                    allowsReplace = true,
                    allowsLost = true,
                    pendingLandsAt = 1_900_000_000L,
                ),
                busy = true,
            ),
        )

        node(Ids.RECOVERY_KIT_REPLACE_BUTTON).assertIsNotEnabled()
        node(Ids.RECOVERY_KIT_LOST_BUTTON).assertIsNotEnabled()
        node(Ids.RECOVERY_PENDING_VETO_BUTTON).assertIsNotEnabled()
    }

    @Test
    fun aMintedKitShowsTheBareHexWithCopyAndQr() {
        val secret = "0f".repeat(32)
        render(
            RecoveryKitUiState(
                status = status("registered", allowsReplace = true, allowsLost = true),
                mintedSecretHex = secret,
                mintedKitUri = "fauna://recovery?k=$secret",
            ),
            qr = QrMatrix(size = 21u, modules = List(21 * 21) { it % 2 == 0 }),
        )

        // The display is the bare hex, never the URI the copy button carries.
        node(Ids.RECOVERY_KIT_SECRET_DISPLAY).assertTextContains(secret)
        node(Ids.RECOVERY_KIT_SECRET_COPY_BTN).assertIsDisplayed()
        node(Ids.RECOVERY_KIT_QR).assertExists()
    }

    @Test
    fun noMintedKitRendersNoDisplay() {
        render(RecoveryKitUiState(status = status("registered", allowsReplace = true)))

        assertAbsent(Ids.RECOVERY_KIT_SECRET_DISPLAY)
        assertAbsent(Ids.RECOVERY_KIT_SECRET_COPY_BTN)
        assertAbsent(Ids.RECOVERY_KIT_QR)
    }

    // ── The stolen-identity leg ─────────────────────────────────────────────

    private fun renderLeg(
        state: RecoveryKitUiState,
        copy: FfiSweepCopy = FfiSweepCopy(outcome = null, unattested = null),
    ) {
        composeTestRule.setContent {
            RecoveryKitSectionContent(
                state = state,
                onPhraseChange = {},
                onCreate = {},
                onReplace = {},
                onLost = {},
                onEscrowReseal = {},
                onVeto = {},
                onStolenConfirmChange = {},
                onStolen = { clicks += "stolen" },
                onSweepRetry = { clicks += "retry" },
                pendingDays = { 12 },
                qrMatrixOf = { null },
                sweepCopyOf = { copy },
            )
        }
    }

    private fun sweep(owesWork: Boolean) = FfiSweepView(
        kind = if (owesWork) "failed" else "ran", detail = null, groups = 3u,
        groupsOldLeafRemoved = if (owesWork) 0u else 3u, unattestedMembers = 0u, owesWork = owesWork,
    )

    @Test
    fun theStolenTriggerRendersOnAnUnreadStatusButStaysDisarmedUntilTheConfirmWord() {
        renderLeg(RecoveryKitUiState(status = null, stolenConfirmInput = "succeed"))

        // The ceremony's authorization is the kit, so an unread chain — what a
        // locked-out owner cannot read — must not take the trigger away.
        node(Ids.IDENTITY_STOLEN_CONFIRM_FIELD).assertExists()
        node(Ids.IDENTITY_STOLEN_BUTTON).assertIsNotEnabled()
    }

    @Test
    fun theConfirmWordArmsTheStolenTrigger() {
        renderLeg(RecoveryKitUiState(status = status("registered", allowsReplace = true), stolenConfirmInput = "SUCCEED"))

        node(Ids.IDENTITY_STOLEN_BUTTON).assertIsEnabled()
        // The trigger sits below the fold of the unscrolled test window, where an
        // injected touch misses it; invoke the node's own click action instead.
        node(Ids.IDENTITY_STOLEN_BUTTON).performSemanticsAction(SemanticsActions.OnClick)
        assertEquals(listOf("stolen"), clicks)
    }

    @Test
    fun aStatusThatPositivelyDisallowsStolenHidesTheTrigger() {
        renderLeg(RecoveryKitUiState(status = status("registered").copy(allowsStolen = false)))

        assertAbsent(Ids.IDENTITY_STOLEN_CONFIRM_FIELD)
        assertAbsent(Ids.IDENTITY_STOLEN_BUTTON)
    }

    @Test
    fun noSuccessionRendersNoSweepLines() {
        renderLeg(RecoveryKitUiState(status = status("registered")))

        assertAbsent(Ids.RECOVERY_KIT_SWEEP_STATUS)
        assertAbsent(Ids.RECOVERY_KIT_SWEEP_UNVOUCHED_STATUS)
        assertAbsent(Ids.RECOVERY_KIT_SWEEP_RETRY_BUTTON)
    }

    @Test
    fun theSweepLinesAreTheProjectionsTwoSeparateElements() {
        renderLeg(
            RecoveryKitUiState(status = status("registered"), sweepView = sweep(owesWork = false)),
            copy = FfiSweepCopy(
                outcome = LocalizedText("settings.recovery_kit.sweep_all_removed", mapOf("groups" to "3")),
                unattested = LocalizedText("settings.recovery_kit.sweep_unattested", mapOf("count" to "2")),
            ),
        )

        node(Ids.RECOVERY_KIT_SWEEP_STATUS).assertExists()
        node(Ids.RECOVERY_KIT_SWEEP_UNVOUCHED_STATUS).assertExists()
        // A finished sweep owes nothing, so there is nothing to retry.
        assertAbsent(Ids.RECOVERY_KIT_SWEEP_RETRY_BUTTON)
    }

    @Test
    fun aLineTheProjectionWithholdsIsAbsentNotEmpty() {
        renderLeg(
            RecoveryKitUiState(status = status("registered"), sweepView = sweep(owesWork = false)),
            copy = FfiSweepCopy(outcome = null, unattested = null),
        )

        assertAbsent(Ids.RECOVERY_KIT_SWEEP_STATUS)
        assertAbsent(Ids.RECOVERY_KIT_SWEEP_UNVOUCHED_STATUS)
    }

    @Test
    fun unfinishedWorkRendersTheRetryWhateverThisDeviceCanDo() {
        renderLeg(
            RecoveryKitUiState(status = status("registered"), sweepView = sweep(owesWork = true)),
            copy = FfiSweepCopy(
                outcome = LocalizedText("settings.recovery_kit.sweep_failed", mapOf("reason" to "offline")),
                unattested = null,
            ),
        )

        node(Ids.RECOVERY_KIT_SWEEP_STATUS).assertExists()
        node(Ids.RECOVERY_KIT_SWEEP_RETRY_BUTTON).assertIsEnabled()
        node(Ids.RECOVERY_KIT_SWEEP_RETRY_BUTTON).performClick()
        assertEquals(listOf("retry"), clicks)
    }
}
