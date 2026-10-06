package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.ffi.FfiReportShareEntry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.SpamTrainingView
import uniffi.fauna_client_mail_settings.TrainingLabel
import uniffi.fauna_client_mail_settings.TrainingSource

/**
 * Compose-level coverage for the stateless [MailSpamContent] (the `mail-spam`
 * page, mail-spam.md): the reset-model affordance, the contribute-baseline
 * toggle, and the indexed training-history list with per-row Undo. Renders with
 * seeded state — no Hilt, no VM, no FFI native calls. The cross-app
 * `test_mail_spam.py --client android` is the standing gate once the `host`
 * emulator lands.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MailSpamContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun event(
        id: String = "abcd",
        message: String = "Re: invoice",
        label: TrainingLabel = TrainingLabel.SPAM,
        source: TrainingSource = TrainingSource.EXPLICIT_BUTTON,
    ) = SpamTrainingView(
        historyIdHex = id,
        message = message,
        label = label,
        source = source,
        createdAtMs = 1_700_000_000_000L,
        modelDeltaApplied = byteArrayOf(),
        // A plaintext (server-written) row: empty sealedSubject + mailbox (the
        // machine leaves `message` as the nest formatted it). The UniFFI record's
        // named-arg ctor requires every field (memory: not optional).
        sealedSubject = byteArrayOf(),
        mailbox = "",
    )

    private fun publishedEntry(
        contentHash: String = "abc123",
        factor: String = "report:spam",
        count: UInt = 3u,
    ) = FfiReportShareEntry(contentHash = contentHash, factor = factor, count = count)

    private fun render(
        events: List<SpamTrainingView> = emptyList(),
        contributeBaseline: Boolean = false,
        reportShare: Boolean = false,
        reportSharePublished: List<FfiReportShareEntry> = emptyList(),
        thresholdOverride: UInt? = null,
        working: Boolean = false,
        // Pure stubs for the FFI-backed badges (shared `training_label_badge` /
        // `training_source_badge`).
        labelBadge: (TrainingLabel) -> String = { it.name },
        sourceBadge: (TrainingSource) -> String = { it.name },
        onResetModel: () -> Unit = {},
        onSetContributeBaseline: (Boolean) -> Unit = {},
        onSetReportShare: (Boolean) -> Unit = {},
        onSetThresholdOverride: (UInt?) -> Unit = {},
        onUndo: (String) -> Unit = {},
        // Pure stub for the FFI-backed parser (no native lib in this JVM test).
        parseCount: (String) -> UInt? = { it.toUIntOrNull() },
    ) {
        composeTestRule.setContent {
            MailSpamContent(
                events = events,
                contributeBaseline = contributeBaseline,
                reportShare = reportShare,
                reportSharePublished = reportSharePublished,
                thresholdOverride = thresholdOverride,
                working = working,
                labelBadge = labelBadge,
                sourceBadge = sourceBadge,
                onBack = {},
                onResetModel = onResetModel,
                onSetContributeBaseline = onSetContributeBaseline,
                onSetReportShare = onSetReportShare,
                onSetThresholdOverride = onSetThresholdOverride,
                onUndo = onUndo,
                parseCount = parseCount,
            )
        }
    }

    @Test
    fun rendersHeadingAndControls() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("mail-spam-reset-model-button").assertExists()
        composeTestRule.onNodeWithTag("mail-spam-contribute-baseline-toggle").assertExists()
        composeTestRule.onNodeWithTag("mail-spam-share-reports-toggle").assertExists()
        composeTestRule.onNodeWithTag("mail-spam-threshold-override-input").assertExists()
    }

    @Test
    fun reportShareToggleFires() {
        var toggled: Boolean? = null
        render(onSetReportShare = { toggled = it })
        composeTestRule.onNodeWithTag("mail-spam-share-reports-toggle").performClick()
        assertEquals(true, toggled)
    }

    /** A `Some(0)` override renders "0", never blank — 0 is a real setting, not
     *  "unset" (mail-policy-config.md § Tier 3). */
    @Test
    fun thresholdOverrideZeroRendersAsZeroNotBlank() {
        render(thresholdOverride = 0u)
        composeTestRule.onNodeWithTag("mail-spam-threshold-override-input")
            .assertTextEquals("0")
    }

    /** No override renders empty — follows the admin default. */
    @Test
    fun thresholdOverrideNullRendersEmpty() {
        render(thresholdOverride = null)
        composeTestRule.onNodeWithTag("mail-spam-threshold-override-input")
            .assertTextEquals("")
    }

    /** The keyboard's Done action commits the parsed value — no separate save
     *  button (tui's `Element::input_commit` shape, mirrored here). */
    @Test
    fun thresholdOverrideCommitsOnImeDone() {
        var committed: UInt? = null
        var calls = 0
        render(onSetThresholdOverride = { committed = it; calls++ })
        composeTestRule.onNodeWithTag("mail-spam-threshold-override-input")
            .performTextInput("7")
        composeTestRule.onNodeWithTag("mail-spam-threshold-override-input")
            .performImeAction()
        assertEquals(1, calls)
        assertEquals(7u, committed)
    }

    @Test
    fun publishedListRowsRenderWithFields() {
        render(
            reportSharePublished = listOf(
                publishedEntry(contentHash = "aa", count = 3u),
                publishedEntry(contentHash = "bb", count = 5u),
            ),
        )
        assertEquals(2, composeTestRule.onAllNodesWithTag("report-share-published-list-item").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("report-share-published-list-item-hash").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("report-share-published-list-item-factor").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("report-share-published-list-item-count").fetchSemanticsNodes().size)
    }

    /**
     * The count element carries the BARE number — not "3 reporters".
     * `test_mail_spam.py` asserts this element's text is exactly "3", and linux
     * renders it as a bare value marker; the human phrasing rides an adjacent
     * Text so the row still reads "3 reporters". This client did fold the word
     * into the element, and the node-count assertions above never looked at the
     * text — which is how it survived. No android e2e has ever run to catch it.
     */
    @Test
    fun publishedCountIsTheBareNumber() {
        render(reportSharePublished = listOf(publishedEntry(contentHash = "aa", count = 3u)))
        composeTestRule.onAllNodesWithTag("report-share-published-list-item-count")[0]
            .assertTextEquals("3")
    }

    @Test
    fun emptyPublishedListShowsNoRows() {
        render(reportSharePublished = emptyList())
        assertTrue(
            composeTestRule.onAllNodesWithTag("report-share-published-list-item").fetchSemanticsNodes().isEmpty(),
        )
    }

    @Test
    fun trainingHistoryRowsRenderWithFields() {
        render(events = listOf(event(id = "aa"), event(id = "bb", label = TrainingLabel.HAM)))
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-spam-training-history-list-item").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-spam-training-history-list-item-message").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-spam-training-history-list-item-undo-button").fetchSemanticsNodes().size)
    }

    @Test
    fun contributeBaselineToggleFires() {
        var toggled: Boolean? = null
        render(onSetContributeBaseline = { toggled = it })
        composeTestRule.onNodeWithTag("mail-spam-contribute-baseline-toggle").performClick()
        assertEquals(true, toggled)
    }

    @Test
    fun resetModelRequiresTwoClicks() {
        var resets = 0
        render(onResetModel = { resets++ })
        // First click arms; does not fire.
        composeTestRule.onNodeWithTag("mail-spam-reset-model-button").performClick()
        assertEquals(0, resets)
        // Second click confirms.
        composeTestRule.onNodeWithTag("mail-spam-reset-model-button").performClick()
        assertEquals(1, resets)
    }

    @Test
    fun undoFiresWithHistoryId() {
        var undone: String? = null
        render(events = listOf(event(id = "deadbeef")), onUndo = { undone = it })
        // The report-share section above pushes this row below the fold in the
        // Robolectric test window now that it exists — scroll it into view first
        // (established pattern, e.g. FoldersContentTest).
        composeTestRule.onNodeWithTag("mail-spam-training-history-list-item-undo-button")
            .performScrollTo().performClick()
        assertEquals("deadbeef", undone)
    }

    @Test
    fun emptyHistoryShowsPlaceholder() {
        render(events = emptyList())
        assertTrue(
            composeTestRule.onAllNodesWithTag("mail-spam-training-history-list-item").fetchSemanticsNodes().isEmpty(),
        )
    }
}
