package com.fauna.app.ui.screen

import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.core.ReportSheetStore
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.components.ReportHostContent
import com.fauna.app.ui.screen.feed.PostActionsMenu
import com.fauna.app.ui.screen.moderation.ModerationQueueContent
import com.fauna.app.ui.screen.profile.ProfileSecondaryActionsRow
import com.fauna.ffi.FfiReportLedgerRow
import com.fauna.ffi.FfiReportReasonOption
import com.fauna.ffi.FfiReportSheetView
import com.fauna.ffi.FfiReportSubject
import com.fauna.ffi.FfiReportTarget
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage of the user-initiated-reporting paint on android
 * (`moderation.md` § User-initiated reporting → *App surface*, *What the reporter
 * is told*): the verbs (`feed-post-report-button`, `profile-report-button`), the
 * one inline sheet and its acknowledgement, and the Moderation ledger. Every
 * decision (the reason list, the submit gate, the include-text rule, the words)
 * is shared Rust's, so the shared folds are SEEDED here as plain values — this
 * pins which element carries which fold, the `*Content` split every sibling
 * screen has (no Hilt, no VM, no FFI).
 *
 * The admin queue and the message ⋯ item are covered where their hosts already
 * have Content harnesses (`AdminNestContentTest`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ReportingPaintTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun text(key: String) = LocalizedText(key, emptyMap())

    private fun sheetView(canSubmit: Boolean, showIncludeText: Boolean) = FfiReportSheetView(
        title = text("moderation.report.title"),
        reasonLabel = text("moderation.report.reason_label"),
        reasons = listOf(
            FfiReportReasonOption("spam", text("moderation.report.reason_spam")),
            FfiReportReasonOption("harassment", text("moderation.report.reason_harassment")),
        ),
        noteLabel = text("moderation.report.note_label"),
        showIncludeText = showIncludeText,
        includeTextLabel = text("moderation.report.include_text_label"),
        blockAuthorLabel = text("moderation.report.block_author_label"),
        submitLabel = text("moderation.report.submit"),
        cancelLabel = text("moderation.report.cancel"),
        canSubmit = canSubmit,
        blockedReason = if (canSubmit) null else text("moderation.report.blocked_no_reason"),
    )

    private val target = FfiReportTarget(
        subject = FfiReportSubject.Post("cid"),
        sealed = false,
        author = "ab".repeat(32),
        plaintext = "x",
    )

    private fun renderHost(
        state: ReportSheetStore.State,
        view: FfiReportSheetView?,
        onEdit: ((com.fauna.ffi.FfiReportForm) -> com.fauna.ffi.FfiReportForm) -> Unit = {},
        onSubmit: () -> Unit = {},
        onCancel: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            ReportHostContent(state = state, view = view, onEdit = onEdit, onSubmit = onSubmit, onCancel = onCancel)
        }
    }

    // ── The sheet ─────────────────────────────────────────────────────────

    @Test
    fun aClosedSheetPaintsNothing() {
        renderHost(ReportSheetStore.State(), view = null)

        composeTestRule.onAllNodesWithTag("report-sheet").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("report-status").assertCountEquals(0)
    }

    @Test
    fun anOpenSheetPaintsEveryMember() {
        renderHost(ReportSheetStore.State(target = target), sheetView(canSubmit = true, showIncludeText = false))

        for (id in listOf(
            "report-sheet", "report-reason-select", "report-note-input",
            "report-block-author-checkbox", "report-submit-button", "report-cancel-button",
        )) {
            composeTestRule.onNodeWithTag(id).assertExists()
        }
        // Only a sealed subject offers the excerpt — the shared fold says so.
        composeTestRule.onAllNodesWithTag("report-include-text-checkbox").assertCountEquals(0)
    }

    @Test
    fun theIncludeTextCheckboxPaintsOnlyWhenTheSharedViewSaysSealed() {
        renderHost(ReportSheetStore.State(target = target), sheetView(canSubmit = true, showIncludeText = true))

        composeTestRule.onNodeWithTag("report-include-text-checkbox").assertExists()
    }

    @Test
    fun submitIsLiveOnlyWhenTheSharedViewAllowsIt() {
        renderHost(ReportSheetStore.State(target = target), sheetView(canSubmit = false, showIncludeText = false))
        composeTestRule.onNodeWithTag("report-submit-button").assertIsNotEnabled()
    }

    @Test
    fun submitIsHeldWhileASendIsInFlight() {
        renderHost(
            ReportSheetStore.State(target = target, sending = true),
            sheetView(canSubmit = true, showIncludeText = false),
        )
        composeTestRule.onNodeWithTag("report-submit-button").assertIsNotEnabled()
    }

    @Test
    fun submitAndCancelForwardTheirGestures() {
        var submits = 0
        var cancels = 0
        renderHost(
            ReportSheetStore.State(target = target),
            sheetView(canSubmit = true, showIncludeText = false),
            onSubmit = { submits++ },
            onCancel = { cancels++ },
        )
        composeTestRule.onNodeWithTag("report-submit-button").assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("report-cancel-button").performClick()

        assertEquals(1, submits)
        assertEquals(1, cancels)
    }

    @Test
    fun theAcknowledgementPaintsOutsideTheClosedSheet() {
        renderHost(
            ReportSheetStore.State(status = LocalizedText("moderation.report.sent_local", mapOf("nest" to "home"))),
            view = null,
        )

        composeTestRule.onAllNodesWithTag("report-sheet").assertCountEquals(0)
        composeTestRule.onNodeWithTag("report-status").assertTextContains("Report sent to the admins of home.")
    }

    // ── The verbs ─────────────────────────────────────────────────────────

    @Test
    fun theFeedReportVerbPaintsOnAnotherAuthorsPostOnly() {
        composeTestRule.setContent {
            PostActionsMenu(
                isOwn = false, markedVerb = null, onTrainVerbTapped = {}, onDelete = {},
                onReport = {},
            )
        }
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-report-button").assertExists()
    }

    @Test
    fun theFeedReportVerbIsAbsentOnAnOwnPost() {
        composeTestRule.setContent {
            PostActionsMenu(
                isOwn = true, markedVerb = null, onTrainVerbTapped = {}, onDelete = {},
                onReport = {},
            )
        }
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onAllNodesWithTag("feed-post-report-button").assertCountEquals(0)
    }

    @Test
    fun theFeedReportVerbOpensTheSheet() {
        var opened = 0
        composeTestRule.setContent {
            PostActionsMenu(
                isOwn = false, markedVerb = null, onTrainVerbTapped = {}, onDelete = {},
                onReport = { opened++ },
            )
        }
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-report-button").performClick()

        assertEquals(1, opened)
    }

    @Test
    fun theProfileReportVerbPaintsWhenTheCallerSuppliesIt() {
        var opened = 0
        composeTestRule.setContent {
            ProfileSecondaryActionsRow(
                blocked = false, blockWorking = false, onStartDm = {}, onToggleBlock = {},
                blockLabel = { "Block" },
                onReport = { opened++ },
            )
        }
        composeTestRule.onNodeWithTag("profile-report-button").performClick()

        assertEquals(1, opened)
    }

    @Test
    fun theProfileReportVerbIsAbsentWithoutACallback() {
        composeTestRule.setContent {
            ProfileSecondaryActionsRow(
                blocked = false, blockWorking = false, onStartDm = {}, onToggleBlock = {},
                blockLabel = { "Block" },
            )
        }
        composeTestRule.onAllNodesWithTag("profile-report-button").assertCountEquals(0)
    }

    // ── The ledger ────────────────────────────────────────────────────────

    // An `Unknown` subject carries no id, so the row line never reaches the
    // FFI-backed `shortId` — the words themselves are the shared fold's.
    private fun ledgerRow(id: String, canWithdraw: Boolean) = FfiReportLedgerRow(
        reportId = id,
        subject = FfiReportSubject.Unknown(byteArrayOf()),
        createdAt = 0L,
        reason = text("moderation.report.reason_spam"),
        status = text(if (canWithdraw) "moderation.report.status_open" else "moderation.report.status_resolved"),
        outcome = if (canWithdraw) null else text("moderation.report.outcome_acted"),
        routedTo = LocalizedText("moderation.report.ledger_routed_to", mapOf("destinations" to "home")),
        canWithdraw = canWithdraw,
    )

    private fun renderLedger(
        reports: List<FfiReportLedgerRow>,
        loaded: Boolean,
        onWithdraw: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            ModerationQueueContent(
                queue = emptyList(),
                isLoading = false,
                onCorrect = {},
                reports = reports,
                reportsLoaded = loaded,
                onWithdrawReport = onWithdraw,
            )
        }
    }

    @Test
    fun theLedgerPaintsOneFlatItemPerReportWithTheSharedWords() {
        renderLedger(listOf(ledgerRow("r1", canWithdraw = true), ledgerRow("r2", canWithdraw = false)), loaded = true)

        composeTestRule.onNodeWithTag("moderation-reports-section").assertExists()
        composeTestRule.onAllNodesWithTag("moderation-report-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("moderation-report-item")[0]
            .assertTextContains("Spam · Open — Sent to home")
        composeTestRule.onAllNodesWithTag("moderation-report-item")[1]
            .assertTextContains("Spam · Resolved · Acted on — Sent to home")
    }

    @Test
    fun withdrawPaintsOnOpenRowsOnlyAndForwardsTheReportId() {
        var withdrawn: String? = null
        renderLedger(
            listOf(ledgerRow("r1", canWithdraw = true), ledgerRow("r2", canWithdraw = false)),
            loaded = true,
            onWithdraw = { withdrawn = it },
        )

        composeTestRule.onAllNodesWithTag("moderation-report-withdraw-button").assertCountEquals(1)
        composeTestRule.onNodeWithTag("moderation-report-withdraw-button").performClick()
        assertEquals("r1", withdrawn)
    }

    @Test
    fun theEmptyLinePaintsOnlyOffTheLoadedBit() {
        renderLedger(emptyList(), loaded = false)
        composeTestRule.onAllNodesWithTag("moderation-reports-section").assertCountEquals(1)
        composeTestRule.onAllNodesWithText("You have not reported anything.").assertCountEquals(0)
    }

    @Test
    fun aLoadedEmptyLedgerSaysSo() {
        renderLedger(emptyList(), loaded = true)
        composeTestRule.onAllNodesWithText("You have not reported anything.").assertCountEquals(1)
    }
}
