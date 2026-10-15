package com.fauna.app.ui.screen.status

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.core.AppMessages
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.screen.settings.FeatureLimitsAdminSection
import com.fauna.app.ui.screen.settings.FeaturePolicyEditorPane
import com.fauna.app.ui.viewmodel.FeatureAuthoringState
import com.fauna.app.ui.viewmodel.FeatureEditorActions
import com.fauna.ffi.FfiAuthoredRow
import com.fauna.ffi.FfiEditorCell
import com.fauna.ffi.FfiFeatureRow
import com.fauna.ffi.FfiPolicyEditorView
import com.fauna.ffi.FfiQuotaDeviceUsage
import com.fauna.ffi.FfiQuotaFeatures
import com.fauna.ffi.FfiQuotaGetReply
import com.fauna.ffi.FfiUsageBytes
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the Status sub-page's two "what bounds me"
 * sections ([QuotaSection], [FeatureLimitsSection]), the self-limits control on
 * each Feature limits row, and the shared `feature-policy-editor` family's two
 * paints ([FeaturePolicyEditorPane], [FeatureLimitsAdminSection]). No Hilt, no
 * VM, no FFI: every native fold is injected or replaced by plain data.
 *
 * The first five tests moved here from `AccountSettingsContentTest` with the
 * sections. Android E2E is host-emulator-gated,
 * so this Robolectric file is the arm's verification on Linux.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class StatusLimitsSectionsTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun quota() = FfiQuotaGetReply(
        tier = "free",
        inbox = FfiUsageBytes(usedBytes = 10, maxBytes = 100),
        storage = FfiUsageBytes(usedBytes = 100, maxBytes = 1000),
        devices = FfiQuotaDeviceUsage(used = 1, max = 5),
        features = FfiQuotaFeatures(versionedBackup = false, bridges = false, maxFeeds = 1),
    )

    private fun featureRow(feature: String, affordance: String = "available") = FfiFeatureRow(
        feature = feature,
        name = LocalizedText("features.$feature.name", emptyMap()),
        availability = if (affordance == "available") "allow" else "deny",
        deniedBy = null,
        cells = emptyList(),
        perOperationMax = null,
        perOperationMaxTier = null,
        unit = "bytes",
        affordance = affordance,
        restriction = null,
        status = LocalizedText("features.status.$affordance", emptyMap()),
    )

    private fun authoredRow(feature: String, summaryKey: String = "features.authored_none") = FfiAuthoredRow(
        feature = feature,
        name = LocalizedText("features.$feature.name", emptyMap()),
        summary = LocalizedText(summaryKey, emptyMap()),
        hasDocument = false,
        unreadable = false,
    )

    private fun editorView(
        tier: String = "self",
        on: Boolean = true,
        canRemove: Boolean = true,
        cells: List<FfiEditorCell> = listOf(cell("5 GB")),
    ) = FfiPolicyEditorView(
        feature = "payments",
        tier = tier,
        title = LocalizedText("features.editor_title_$tier", mapOf("feature" to "features.name_payments")),
        on = on,
        cells = cells,
        offNote = null,
        canRemove = canRemove,
        hint = LocalizedText("features.editor_hint", emptyMap()),
    )

    private fun cell(text: String, note: LocalizedText? = null) = FfiEditorCell(
        label = LocalizedText("features.editor_per_operation_bytes", emptyMap()),
        text = text,
        note = note,
        noteMagnitude = null,
    )

    private fun setContent(content: @androidx.compose.runtime.Composable () -> Unit) {
        composeTestRule.setContent {
            CompositionLocalProvider(LocalAppMessages provides AppMessages()) { content() }
        }
    }

    private fun renderQuota() = setContent {
        QuotaSection(
            quota = quota(),
            quotaFraction = { used, max -> if (max <= 0) 0.0 else (used.toDouble() / max).coerceIn(0.0, 1.0) },
            quotaPercent = { used, max -> if (max <= 0) 0u else ((used * 100 / max).coerceIn(0, 100)).toUInt() },
            byteSize = { LocalizedText("common.byte_size_raw", mapOf("n" to it.toString())) },
        )
    }

    // ── Quota ────────────────────────────────────────────────────────────

    @Test
    fun quotaSectionRendersItsRows() {
        renderQuota()
        composeTestRule.onNodeWithTag("quota-section").assertExists()
        composeTestRule.onNodeWithTag("settings-storage-bar").assertExists()
        composeTestRule.onNodeWithTag("settings-storage-text").assertExists()
        composeTestRule.onNodeWithTag("quota-inbox").assertExists()
        composeTestRule.onNodeWithTag("quota-storage").assertExists()
        composeTestRule.onNodeWithTag("quota-devices").assertExists()
    }

    // ── Feature limits ───────────────────────────────────────────────────

    @Test
    fun featureLimitsSectionRendersRows() {
        setContent { FeatureLimitsSection(listOf(featureRow("payments"), featureRow("zaps"))) }
        composeTestRule.onNodeWithTag("feature-limits-section").assertExists()
        assertEquals(2, composeTestRule.onAllNodesWithTag("feature-limits-row").fetchSemanticsNodes().size)
    }

    @Test
    fun featureLimitsSectionShowsEmptyStateWhenEveryRowIsHidden() {
        setContent { FeatureLimitsSection(listOf(featureRow("payments", affordance = "hidden"))) }
        composeTestRule.onNodeWithTag("feature-limits-section").assertExists()
        composeTestRule.onNodeWithTag("feature-limits-empty").assertExists()
        composeTestRule.onNodeWithTag("feature-limits-row").assertDoesNotExist()
    }

    @Test
    fun ownControlIsAbsentBeforeTheAuthoredReadResolves() {
        setContent { FeatureLimitsSection(listOf(featureRow("payments")), FeatureAuthoringState(rows = null)) }
        composeTestRule.onNodeWithTag("feature-limits-row").assertExists()
        composeTestRule.onNodeWithTag("feature-limits-own-summary").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feature-limits-own-edit-button").assertDoesNotExist()
    }

    @Test
    fun ownControlOnlyAppearsOnRowsTheNestCarries() {
        // `zaps` has a transparency row but no authored row (the nest does not carry it).
        setContent {
            FeatureLimitsSection(
                listOf(featureRow("payments"), featureRow("zaps")),
                FeatureAuthoringState(rows = listOf(authoredRow("payments"))),
            )
        }
        assertEquals(1, composeTestRule.onAllNodesWithTag("feature-limits-own-edit-button").fetchSemanticsNodes().size)
        composeTestRule.onNodeWithTag("feature-limits-own-summary").assertTextEquals("No limit set")
    }

    @Test
    fun ownEditButtonOpensTheEditorOverItsOwnRow() {
        var opened: String? = null
        setContent {
            FeatureLimitsSection(
                listOf(featureRow("payments")),
                FeatureAuthoringState(rows = listOf(authoredRow("payments"))),
                FeatureEditorActions.None.let {
                    FeatureEditorActions(
                        onOpen = { f -> opened = f },
                        onSetOn = it.onSetOn,
                        onSetCell = it.onSetCell,
                        onSave = it.onSave,
                        onRemove = it.onRemove,
                        onCancel = it.onCancel,
                    )
                },
            )
        }
        composeTestRule.onNodeWithTag("feature-limits-own-edit-button").performClick()
        assertEquals("payments", opened)
    }

    @Test
    fun ownEditorPaintsUnderItsRowWhenOpen() {
        setContent {
            FeatureLimitsSection(
                listOf(featureRow("payments")),
                FeatureAuthoringState(
                    rows = listOf(authoredRow("payments")),
                    openFeature = "payments",
                    view = editorView(),
                ),
            )
        }
        composeTestRule.onNodeWithTag("feature-policy-editor").assertExists()
    }

    // ── The shared editor ────────────────────────────────────────────────

    private fun renderEditor(
        view: FfiPolicyEditorView = editorView(),
        status: String? = null,
        actions: FeatureEditorActions = FeatureEditorActions.None,
    ) = setContent {
        FeaturePolicyEditorPane(
            view = view,
            status = status,
            working = false,
            onSetOn = actions.onSetOn,
            onSetCell = actions.onSetCell,
            onSave = actions.onSave,
            onRemove = actions.onRemove,
            onCancel = actions.onCancel,
        )
    }

    @Test
    fun editorPaintsItsFamilyOfIds() {
        renderEditor(status = "Saved.")
        for (id in listOf(
            "feature-policy-editor", "feature-policy-editor-title", "feature-policy-editor-on-radio",
            "feature-policy-editor-off-radio", "feature-policy-editor-cell", "feature-policy-editor-cell-label",
            "feature-policy-editor-cell-input", "feature-policy-editor-save-button",
            "feature-policy-editor-remove-button", "feature-policy-editor-cancel-button",
            "feature-policy-editor-status",
        )) {
            composeTestRule.onNodeWithTag(id).assertExists()
        }
    }

    @Test
    fun editorShowsNoCellsAndNoHintWhileOff() {
        renderEditor(view = editorView(on = false, cells = emptyList()))
        composeTestRule.onNodeWithTag("feature-policy-editor-cell").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feature-policy-editor-off-radio").assertIsSelected()
    }

    @Test
    fun removeRendersOnlyWhileTheViewSaysSo() {
        renderEditor(view = editorView(canRemove = false))
        composeTestRule.onNodeWithTag("feature-policy-editor-remove-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feature-policy-editor-save-button").assertExists()
    }

    @Test
    fun noEffectNoteAppearsOnlyWhenTheCellCarriesOne() {
        renderEditor(view = editorView(cells = listOf(cell("5 GB"), cell("9 GB", note = LocalizedText("features.no_effect_admin", mapOf("limit" to "2 GB"))))))
        assertEquals(1, composeTestRule.onAllNodesWithTag("feature-policy-editor-cell-note").fetchSemanticsNodes().size)
    }

    @Test
    fun radiosAndKeystrokesAndButtonsForwardToTheCallbacks() {
        var on: Boolean? = null
        var typed: Pair<Int, String>? = null
        var saved = 0
        var removed = 0
        var cancelled = 0
        renderEditor(
            actions = FeatureEditorActions(
                onOpen = {},
                onSetOn = { on = it },
                onSetCell = { i, t -> typed = i to t },
                onSave = { saved++ },
                onRemove = { removed++ },
                onCancel = { cancelled++ },
            ),
        )
        composeTestRule.onNodeWithTag("feature-policy-editor-off-radio").performClick()
        assertEquals(false, on)
        composeTestRule.onNodeWithTag("feature-policy-editor-cell-input").performTextReplacement("7 GB")
        assertEquals(0 to "7 GB", typed)
        composeTestRule.onNodeWithTag("feature-policy-editor-save-button").performClick()
        composeTestRule.onNodeWithTag("feature-policy-editor-remove-button").performClick()
        composeTestRule.onNodeWithTag("feature-policy-editor-cancel-button").performClick()
        assertEquals(listOf(1, 1, 1), listOf(saved, removed, cancelled))
    }

    // ── The admin host ───────────────────────────────────────────────────

    @Test
    fun adminSectionIsAbsentBeforeTheAuthoredReadResolves() {
        setContent { FeatureLimitsAdminSection(FeatureAuthoringState(rows = null), FeatureEditorActions.None) }
        composeTestRule.onNodeWithTag("admin-nest-feature-limits-section").assertDoesNotExist()
    }

    @Test
    fun adminSectionListsOneRowPerMemberWithAnEditButton() {
        setContent {
            FeatureLimitsAdminSection(
                FeatureAuthoringState(rows = listOf(authoredRow("payments"), authoredRow("zaps"))),
                FeatureEditorActions.None,
            )
        }
        composeTestRule.onNodeWithTag("admin-nest-feature-limits-section").assertExists()
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-nest-feature-limits-row").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-nest-feature-limits-edit-button").fetchSemanticsNodes().size)
        composeTestRule.onAllNodesWithTag("admin-nest-feature-limits-summary")[0].assertTextEquals("No limit set")
    }

    @Test
    fun adminEditorOpensUnderTheRowItIsOver() {
        setContent {
            FeatureLimitsAdminSection(
                FeatureAuthoringState(
                    rows = listOf(authoredRow("payments"), authoredRow("zaps")),
                    openFeature = "zaps",
                    view = editorView(tier = "admin"),
                ),
                FeatureEditorActions.None,
            )
        }
        assertEquals(1, composeTestRule.onAllNodesWithTag("feature-policy-editor").fetchSemanticsNodes().size)
    }
}
