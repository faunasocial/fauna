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
    ) = MailExportSnapshot(
        step = step,
        format = ExportFormat.MBOX,
        mailboxes = mailboxes,
        dateFrom = "",
        dateTo = "",
        stripHeaders = false,
        sessionState = null,
        exportedCount = 0u,
        skippedCount = 0u,
        erroredCount = 0u,
        totalCount = totalCount,
        mailboxProgress = emptyList(),
        errorLog = emptyList(),
        blobBytes = blobBytes,
        downloadUrl = downloadUrl,
        savedArchivePath = savedArchivePath,
        status = ExportStatus.IDLE,
        error = null,
    )

    private fun render(
        snapshot: MailExportSnapshot,
        onSelectFormat: (ExportFormat) -> Unit = {},
        onStart: () -> Unit = {},
        onDownload: () -> Unit = {},
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
                onDiscard = {},
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
    fun doneSummaryNamesNoPathBeforeDownload() {
        render(snap(ExportStep.DONE, downloadUrl = "/api/v1/export/1", blobBytes = 1234uL))
        composeTestRule.onNodeWithTag("mail-export-done-summary")
            .assert(hasText("saved to", substring = true).not())
    }
}
