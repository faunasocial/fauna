package com.fauna.app.ui.screen.settings

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_archive_import_machine.ArchiveImportSnapshot
import uniffi.fauna_archive_import_machine.ArchiveImportStatus
import uniffi.fauna_archive_import_machine.ArchiveImportStep
import uniffi.fauna_archive_import_machine.ArchiveProfileView
import uniffi.fauna_archive_import_machine.ArchiveSourceKind
import uniffi.fauna_archive_import_machine.AudienceMode
import uniffi.fauna_archive_import_machine.CategoryRow
import uniffi.fauna_archive_import_machine.RunState

/**
 * Compose-level coverage for the stateless [ArchiveImportContent] (the
 * `archive-import` wizard, archive-import.md § The wizard and its machine):
 * one step in the tree at a time, the category rows' `state` attribute and
 * the profile prefill's enablement. Seeded state, fake labels — no Hilt, no
 * VM, no FFI.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ArchiveImportContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val labels = ArchiveImportLabels(
        sourceKind = { if (it == ArchiveSourceKind.FACEBOOK) "Facebook" else "Instagram" },
        sourceHelp = { "help for $it" },
        audienceMode = { it.name },
        pageText = { s ->
            ArchiveImportLines(
                sourceLink = "Open ${s.sourceExportUrl}",
                archiveSummary = s.summary?.let { "summary" },
                scopeRows = s.categories.map { "${it.token} (${it.count})" },
                audienceSummary = "audience",
                confirmSummary = "confirm",
                progressSummary = "progress",
                progressFraction = if (s.total == 0uL) 0.0 else (s.imported + s.skipped).toDouble() / s.total.toDouble(),
                progressRows = emptyList(),
                doneSummary = "done",
                folderLink = "folder",
            )
        },
    )

    private fun snap(
        step: ArchiveImportStep,
        categories: List<CategoryRow> = emptyList(),
        runState: RunState? = null,
        archiveProfile: ArchiveProfileView? = null,
        profileApplied: Boolean = false,
        error: String? = null,
    ) = ArchiveImportSnapshot(
        step = step,
        sourceKind = ArchiveSourceKind.FACEBOOK,
        sourceExportUrl = "https://www.facebook.com/dyi",
        archivePath = "",
        summary = null,
        categories = categories,
        audienceMode = AudienceMode.ORIGINAL,
        dateFrom = "",
        dateTo = "",
        confirmRecords = 0uL,
        confirmBytes = 0uL,
        folderName = null,
        runState = runState,
        total = 4uL,
        imported = 2uL,
        skipped = 0uL,
        errored = 0uL,
        currentCategory = null,
        elapsedSecs = 3uL,
        etaSecs = null,
        skipLog = emptyList(),
        nestSupportsHiddenTiers = true,
        resumeAvailable = false,
        status = ArchiveImportStatus.IDLE,
        error = error,
        linkClaimed = 0uL,
        linkUnmatched = 0uL,
        commentsUnplaced = 0uL,
        archiveProfile = archiveProfile,
        profileApplied = profileApplied,
    )

    private fun render(
        s: ArchiveImportSnapshot,
        onToggle: (String, Boolean) -> Unit = { _, _ -> },
        onOpenExportPage: (String) -> Unit = {},
        onApplyProfile: () -> Unit = {},
    ) = composeTestRule.setContent {
        ArchiveImportContent(
            snapshot = s, errorMessage = s.error, unavailable = false, pickedPath = null, labels = labels,
            onBack = {}, onSelectSource = {}, onOpenExportPage = onOpenExportPage, onOpenArchive = {},
            onPickArchive = {}, onToggleCategory = onToggle, onSetAudienceMode = {}, onScopeNext = { _, _ -> },
            onNext = {}, onPrev = {}, onStart = {}, onPause = {}, onResume = {}, onCancel = {},
            onApplyProfile = onApplyProfile, onViewImported = {}, onOpenFolders = {},
        )
    }

    private fun stateOf(node: SemanticsNodeInteraction): String? =
        node.fetchSemanticsNode().config.getOrNull(SemanticsProperties.StateDescription)

    @Test
    fun theSourceStepLinksTheExportPageAndPaintsOnlyItsOwnElements() {
        var opened: String? = null
        render(snap(ArchiveImportStep.SOURCE), onOpenExportPage = { opened = it })
        composeTestRule.onNodeWithTag("archive-import-source-picker").assertExists()
        composeTestRule.onNodeWithTag("archive-import-source-help").assertTextContains("help for FACEBOOK")
        composeTestRule.onNodeWithTag("archive-import-archive-path").assertDoesNotExist()
        composeTestRule.onNodeWithTag("wizard-back-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
        composeTestRule.onNodeWithTag("archive-import-source-export-link").performClick()
        assertEquals("https://www.facebook.com/dyi", opened)
    }

    @Test
    fun categoryRowsCarryOnOffAndKeptAndAToggleAsksForTheOpposite() {
        val toggles = mutableListOf<Pair<String, Boolean>>()
        render(
            snap(
                ArchiveImportStep.SCOPE,
                categories = listOf(
                    CategoryRow("posts", 3uL, true, true, 0uL, 0uL),
                    CategoryRow("albums", 1uL, true, false, 0uL, 0uL),
                    CategoryRow("friends", 2uL, false, false, 0uL, 0uL),
                ),
            ),
            onToggle = { t, v -> toggles += t to v },
        )
        val rows = composeTestRule.onAllNodesWithTag("archive-import-scope-category-item")
        rows.assertCountEquals(3)
        assertEquals("on", stateOf(rows[0]))
        assertEquals("off", stateOf(rows[1]))
        assertEquals("kept", stateOf(rows[2]))
        rows[0].performClick()
        assertEquals(listOf("posts" to false), toggles)
        // The echo flips the row at once, before the machine answers.
        composeTestRule.waitForIdle()
        assertEquals("off", stateOf(composeTestRule.onAllNodesWithTag("archive-import-scope-category-item")[0]))
    }

    @Test
    fun theProfilePrefillIsOfferedOnlyWhileAnUnappliedProfileRides() {
        var applied = 0
        val profile = ArchiveProfileView("Test Owner", "bio", emptyList(), true)
        render(snap(ArchiveImportStep.DONE, archiveProfile = profile), onApplyProfile = { applied++ })
        composeTestRule.onNodeWithTag("archive-import-profile-prefill-button").assertIsEnabled().performClick()
        assertEquals(1, applied)
        composeTestRule.onNodeWithTag("archive-import-error-log").assertDoesNotExist()
        composeTestRule.onNodeWithTag("archive-import-review-skipped-button").performClick()
        composeTestRule.onNodeWithTag("archive-import-error-log").assertExists()
    }

    @Test
    fun anAppliedProfileLeavesThePrefillDisabled() {
        val profile = ArchiveProfileView("Test Owner", null, emptyList(), false)
        render(snap(ArchiveImportStep.DONE, archiveProfile = profile, profileApplied = true))
        composeTestRule.onNodeWithTag("archive-import-profile-prefill-button").assertIsNotEnabled()
    }

    @Test
    fun aRunningImportHasNoBackAndTheBarCarriesItsFraction() {
        render(snap(ArchiveImportStep.PROGRESS, runState = RunState.RUNNING, error = "boom"))
        composeTestRule.onNodeWithTag("wizard-back-button").assertDoesNotExist()
        assertEquals("0.5000", stateOf(composeTestRule.onNodeWithTag("archive-import-progress-bar")))
        composeTestRule.onNodeWithTag("error-message").assertTextContains("boom")
    }
}
