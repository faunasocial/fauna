package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.DmarcCountRow
import uniffi.fauna_client_mail_settings.DmarcOpenedReport
import uniffi.fauna_client_mail_settings.DmarcReportRowView
import uniffi.fauna_client_mail_settings.DmarcReportsSnapshot
import uniffi.fauna_client_mail_settings.DmarcReportsStatus
import uniffi.fauna_client_mail_settings.DmarcWindowTotals

/**
 * Compose-level coverage for the stateless [AdminMailReportsContent] (the
 * `admin-mail-reports` sub-page, dmarc-reporting.md § Admin pane summary surface),
 * mirroring the tui reference's renderer tests: before the first read only the
 * filters paint; a loaded window paints totals, lists and indexed rows; the
 * empty / truncated / raw texts paint only on their own path. Seeded snapshot —
 * no Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminMailReportsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun row(id: String = "r-1") = DmarcReportRowView(
        ourDomain = "a.test",
        submitter = "Peer <r@peer.test>",
        reportId = id,
        beginAt = 1_760_000_000L,
        endAt = 1_760_086_400L,
        receivedAt = 1_760_090_000L,
        messageCount = 10uL,
        dmarcPass = 9uL,
    )

    private fun snapshot(
        loaded: Boolean = true,
        reports: List<DmarcReportRowView> = listOf(row("r-1"), row("r-2")),
        truncated: Boolean = false,
        opened: DmarcOpenedReport? = null,
        windowDays: UInt = 30u,
    ) = DmarcReportsSnapshot(
        windowDays = windowDays,
        ourDomain = null,
        localDomains = listOf("a.test"),
        loaded = loaded,
        totals = DmarcWindowTotals(
            messageCount = 10uL, reportCount = reports.size.toULong(), submitterCount = 1uL,
            dmarcPass = 9uL, dmarcPassPercent = 90u,
            dkimPass = 8uL, dkimPassPercent = 80u,
            spfPass = 7uL, spfPassPercent = 70u,
            dispositionNone = 10uL, dispositionQuarantine = 0uL, dispositionReject = 0uL,
        ),
        sources = if (reports.isEmpty()) emptyList() else listOf(DmarcCountRow("192.0.2.1", 10uL)),
        senders = if (reports.isEmpty()) emptyList() else listOf(DmarcCountRow("a.test", 10uL)),
        reports = reports,
        truncated = truncated,
        opened = opened,
        status = DmarcReportsStatus.IDLE,
        error = null,
    )

    private fun render(
        snap: DmarcReportsSnapshot,
        onRefresh: () -> Unit = {},
        onSetWindow: (UInt) -> Unit = {},
        onSetDomain: (String?) -> Unit = {},
        onOpenReport: (DmarcReportRowView) -> Unit = {},
    ) {
        composeTestRule.setContent {
            AdminMailReportsContent(
                snapshot = snap,
                onBack = {},
                onRefresh = onRefresh,
                onSetWindow = onSetWindow,
                onSetDomain = onSetDomain,
                onOpenReport = onOpenReport,
                // FFI-free stubs for the shared window options / date formatter.
                windowOptions = listOf(7u, 30u, 90u),
                formatDate = { "2026-10-10" },
            )
        }
    }

    private fun absent(tag: String) = composeTestRule.onNodeWithTag(tag).assertDoesNotExist()
    private fun present(tag: String) = composeTestRule.onNodeWithTag(tag).assertExists()

    @Test
    fun beforeTheFirstReadOnlyTheFiltersPaint() {
        render(snapshot(loaded = false, reports = emptyList(), windowDays = 7u))
        for (t in listOf(
            "page-heading", "admin-nav-back", "admin-mail-reports-window-select",
            "admin-mail-reports-domain-select", "admin-mail-reports-refresh-button",
        )) present(t)
        for (t in listOf(
            "admin-mail-reports-summary", "admin-mail-reports-messages", "admin-mail-reports-alignment",
            "admin-mail-reports-dispositions", "admin-mail-reports-empty", "admin-mail-reports-truncated",
            "admin-mail-reports-raw", "admin-mail-reports-source", "admin-mail-reports-report",
        )) absent(t)
    }

    @Test
    fun aLoadedWindowPaintsTotalsListsAndIndexedRows() {
        render(snapshot())
        for (t in listOf(
            "admin-mail-reports-summary", "admin-mail-reports-messages", "admin-mail-reports-alignment",
            "admin-mail-reports-dispositions", "admin-mail-reports-source-ip",
            "admin-mail-reports-sender-domain", "admin-mail-reports-source-count",
        )) composeTestRule.onAllNodesWithTag(t, useUnmergedTree = true)[0].assertExists()
        for (t in listOf(
            "admin-mail-reports-empty", "admin-mail-reports-truncated", "admin-mail-reports-raw",
        )) absent(t)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-mail-reports-report").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-mail-reports-report-view-button").fetchSemanticsNodes().size)
        composeTestRule.onNodeWithTag("admin-mail-reports-alignment")
            .assertTextContains("(90%)", substring = true)
        composeTestRule.onAllNodesWithTag("admin-mail-reports-report-period", useUnmergedTree = true)[0]
            .assertTextContains("2026-10-10 – 2026-10-10", substring = true)
    }

    @Test
    fun emptyTruncatedAndRawPathsPaintTheirOwnText() {
        render(snapshot(reports = emptyList()))
        present("admin-mail-reports-empty")
        absent("admin-mail-reports-report")
    }

    @Test
    fun truncatedAndRawPaintOnlyOnTheirPath() {
        render(
            snapshot(
                reports = listOf(row()),
                truncated = true,
                opened = DmarcOpenedReport("a.test", "Peer <r@peer.test>", "r-1", "<feedback/>"),
            )
        )
        present("admin-mail-reports-truncated")
        composeTestRule.onNodeWithTag("admin-mail-reports-raw").assertTextEquals("<feedback/>")
        absent("admin-mail-reports-empty")
    }

    @Test
    fun viewButtonOpensTheRowsOwnKey() {
        var opened: DmarcReportRowView? = null
        render(snapshot(), onOpenReport = { opened = it })
        composeTestRule.onAllNodesWithTag("admin-mail-reports-report-view-button")[1]
            .performScrollTo().performClick()
        assertEquals("r-2", opened?.reportId)
    }

    @Test
    fun refreshFires() {
        var n = 0
        render(snapshot(), onRefresh = { n++ })
        composeTestRule.onNodeWithTag("admin-mail-reports-refresh-button").performClick()
        assertEquals(1, n)
    }
}
