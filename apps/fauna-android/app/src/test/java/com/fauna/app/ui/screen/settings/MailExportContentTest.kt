package com.fauna.app.ui.screen.settings

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_mail.ExportFormat
import uniffi.fauna_client_mail_settings.ExportSessionState
import uniffi.fauna_client_mail_settings.ExportStatus
import uniffi.fauna_client_mail_settings.ExportStep
import uniffi.fauna_client_mail_settings.MailExportSnapshot
import uniffi.fauna_client_mail_settings.MailboxOption

/**
 * Compose-level coverage for the stateless [MailExportContent] (the `mail-export`
 * wizard, mail-export.md): the five conditional steps share one element set,
 * shown by `snapshot.step`. Seeded state — no Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MailExportContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun snap(
        step: ExportStep,
        mailboxes: List<MailboxOption> = emptyList(),
        totalCount: UInt = 0u,
        downloadUrl: String = "",
        blobBytes: ULong? = null,
        savedArchivePath: String = "",
        // The session's clock; 0 = no session read yet (the untimed summaries).
        startedAt: Long = 0L,
        lastProgressAt: Long = 0L,
        sessionState: ExportSessionState? = null,
        errorLog: List<String> = emptyList(),
    ) = MailExportSnapshot(
        step = step,
        format = ExportFormat.MBOX,
        mailboxes = mailboxes,
        dateFrom = "",
        dateTo = "",
        stripHeaders = false,
        sessionState = sessionState,
        exportedCount = 0u,
        skippedCount = 0u,
        erroredCount = 0u,
        totalCount = totalCount,
        mailboxProgress = emptyList(),
        errorLog = errorLog,
        blobBytes = blobBytes,
        downloadUrl = downloadUrl,
        savedArchivePath = savedArchivePath,
        startedAt = startedAt,
        lastProgressAt = lastProgressAt,
        status = ExportStatus.IDLE,
        error = null,
    )

    private fun render(
        snapshot: MailExportSnapshot,
        onSelectFormat: (ExportFormat) -> Unit = {},
        onStart: () -> Unit = {},
        onDownload: () -> Unit = {},
        onDiscard: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            MailExportContent(
                snapshot = snapshot,
                // Pure FFI-free stub for the shared `export_format_label` formatter.
                formatLabel = { it.name },
                onBack = {},
                onSelectFormat = onSelectFormat,
                onToggleMailbox = {},
                onSetDateFrom = {},
                onSetDateTo = {},
                onSetStripHeaders = {},
                onNext = {},
                onPrev = {},
                onStart = onStart,
                onPause = {},
                onResume = {},
                onCancel = {},
                onDiscard = onDiscard,
                onDownload = onDownload,
            )
        }
    }

    @Test
    fun formatStepShowsPicker() {
        render(snap(ExportStep.FORMAT))
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("mail-export-format-picker").assertExists()
        // Shared wizard-next-button (ui.yaml, user-approved 2026-08-29) — only one copy in the tree per step.
        composeTestRule.onNodeWithTag("wizard-next-button").assertExists()
    }

    @Test
    fun scopeStepShowsControls() {
        render(
            snap(
                ExportStep.SCOPE,
                mailboxes = listOf(
                    MailboxOption(name = "INBOX", selected = true),
                    MailboxOption(name = "Archive", selected = false),
                ),
            )
        )
        composeTestRule.onNodeWithTag("mail-export-scope-mailboxes").assertExists()
        composeTestRule.onNodeWithTag("mail-export-scope-date-from").assertExists()
        composeTestRule.onNodeWithTag("mail-export-scope-strip-headers-toggle").assertExists()
        composeTestRule.onNodeWithTag("wizard-back-button").assertExists()
        composeTestRule.onNodeWithTag("wizard-next-button").assertExists()
        // Indexed mail-export-scope-mailbox-item: text = mailbox name,
        // state attribute = on/off, read by MailExportActions.mailbox_selected.
        val rows = composeTestRule.onAllNodesWithTag("mail-export-scope-mailbox-item")
        rows.assertCountEquals(2)
        rows[0].assertTextEquals("INBOX")
        rows[0].assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "on"))
        rows[1].assertTextEquals("Archive")
        rows[1].assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "off"))
    }

    @Test
    fun confirmStepShowsStartButton() {
        var started = false
        render(snap(ExportStep.CONFIRM), onStart = { started = true })
        composeTestRule.onNodeWithTag("mail-export-confirm-summary").assertExists()
        composeTestRule.onNodeWithTag("wizard-back-button").assertExists()
        composeTestRule.onNodeWithTag("mail-export-start-button").performScrollTo().performClick()
        assert(started)
    }

    @Test
    fun progressStepShowsBarAndControls() {
        render(snap(ExportStep.PROGRESS, totalCount = 10u))
        composeTestRule.onNodeWithTag("mail-export-progress-summary").assertExists()
        composeTestRule.onNodeWithTag("mail-export-progress-bar").assertExists()
        composeTestRule.onNodeWithTag("mail-export-cancel-button").assertExists()
    }

    @Test
    fun doneStepShowsDownloadAndDiscard() {
        render(snap(ExportStep.DONE, downloadUrl = "https://nest/export/1"))
        composeTestRule.onNodeWithTag("mail-export-done-summary").assertExists()
        composeTestRule.onNodeWithTag("mail-export-download-button").assertExists()
        composeTestRule.onNodeWithTag("mail-export-discard-button").assertExists()
    }

    @Test
    fun downloadButtonDispatchesDownload() {
        var downloads = 0
        render(snap(ExportStep.DONE, downloadUrl = "/api/v1/export/1"), onDownload = { downloads++ })
        composeTestRule.onNodeWithTag("mail-export-download-button").performScrollTo().performClick()
        assert(downloads == 1) { "Download must reach the view-model, got $downloads presses" }
    }

    @Test
    fun doneSummaryNamesTheSavedArchiveOnceDownloaded() {
        // Pressing Download must be visible: once the archive is on disk the
        // Done summary says where (`mail_export.saved_summary_fmt`).
        val path = "/cache/fauna/fauna-export-alice-mbox-2026-10-03.zip.zst"
        render(snap(ExportStep.DONE, downloadUrl = "/api/v1/export/1", blobBytes = 1234uL, savedArchivePath = path))
        composeTestRule.onNodeWithTag("mail-export-done-summary")
            .assertTextContains(path, substring = true)
    }

    @Test
    fun doneSummaryReportsHowLongTheExportTook() {
        render(
            snap(
                ExportStep.DONE, downloadUrl = "/api/v1/export/1", blobBytes = 1234uL,
                startedAt = 1_000L, lastProgressAt = 1_090L,
            ),
        )
        composeTestRule.onNodeWithTag("mail-export-done-summary")
            .assertTextContains("took", substring = true)
    }

    @Test
    fun progressSummaryAddsElapsedTime() {
        render(snap(ExportStep.PROGRESS, totalCount = 10u, startedAt = 1_000L, lastProgressAt = 1_090L))
        composeTestRule.onNodeWithTag("mail-export-progress-summary")
            .assertTextContains("elapsed", substring = true)
    }

    @Test
    fun confirmStepTotalsTheSelectedMailboxesAndSaysTheEstimateIsMissing() {
        render(
            snap(
                ExportStep.CONFIRM,
                mailboxes = listOf(
                    MailboxOption(name = "INBOX", selected = true, messageCount = 7u),
                    MailboxOption(name = "Archive", selected = false, messageCount = 99u),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("mail-export-confirm-summary")
            .assertTextContains("7 messages", substring = true)
        composeTestRule.onNodeWithText("not estimated yet", substring = true).assertExists()
    }

    @Test
    fun confirmStepEstimatesTheSelectedBytesAndTime() {
        // 150 MiB at the shared 1 MiB/s is 150 s; Trash is not selected, so its
        // bytes are not coming. Mirrors tui's
        // `the_confirm_step_estimates_the_selected_bytes_and_time`.
        render(
            snap(
                ExportStep.CONFIRM,
                mailboxes = listOf(
                    MailboxOption(name = "INBOX", selected = true, messageCount = 10u, sealedBytes = 157_286_400uL),
                    MailboxOption(name = "Trash", selected = false, messageCount = 10u, sealedBytes = 943_718_400uL),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("mail-export-confirm-summary")
            .assertTextContains("10 messages", substring = true)
            .assertTextContains("about 150 MB · about 2m to export", substring = true)
        composeTestRule.onNodeWithText("not estimated yet", substring = true).assertDoesNotExist()
    }

    @Test
    fun doneSummaryNamesNoPathBeforeDownload() {
        render(snap(ExportStep.DONE, downloadUrl = "/api/v1/export/1", blobBytes = 1234uL))
        composeTestRule.onNodeWithTag("mail-export-done-summary")
            .assert(hasText("saved to", substring = true).not())
    }

    /** A failed export is painted on the Progress step with its reason and one
     *  control, Dismiss — none of Pause / Resume / Cancel (`mail-export.md` § A
     *  failed export is a record the wizard shows until the user moves on).
     *  Mirrors tui's `a_failed_export_offers_dismiss_and_none_of_the_three_controls`. */
    @Test
    fun aFailedExportOffersDismissAndNoneOfTheThreeControls() {
        var dismissed = false
        render(
            snap(
                ExportStep.PROGRESS,
                sessionState = ExportSessionState.ERRORED,
                errorLog = listOf("INBOX: uid 2 could not be opened"),
            ),
            onDiscard = { dismissed = true },
        )
        for (id in listOf("mail-export-pause-button", "mail-export-resume-button", "mail-export-cancel-button")) {
            composeTestRule.onNodeWithTag(id).assertDoesNotExist()
        }
        composeTestRule.onNodeWithTag("mail-export-discard-button").assertTextEquals("Dismiss")
        composeTestRule.onNodeWithText("INBOX: uid 2 could not be opened").assertExists()
        composeTestRule.onNodeWithTag("mail-export-progress-summary").assertExists()
        composeTestRule.onNodeWithTag("mail-export-discard-button").performScrollTo().performClick()
        assert(dismissed)
    }

    /** A live export keeps its three controls and paints no Dismiss. */
    @Test
    fun aLiveExportKeepsTheThreeControlsAndPaintsNoDismiss() {
        render(snap(ExportStep.PROGRESS, sessionState = ExportSessionState.RUNNING))
        for (id in listOf("mail-export-pause-button", "mail-export-resume-button", "mail-export-cancel-button")) {
            composeTestRule.onNodeWithTag(id).assertExists()
        }
        composeTestRule.onNodeWithTag("mail-export-discard-button").assertDoesNotExist()
    }
}
