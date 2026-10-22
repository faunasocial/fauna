package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.ui.viewmodel.FilterForm
import com.fauna.ffi.FfiEmailFilter
import com.fauna.ffi.FfiFilterActionInputs
import com.fauna.ffi.FfiFilterRuleInputs
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [PrivacySettingsContent] spam-preferences
 * controls — the guard for the shared spam-prefs presentation contract adoption
 * (settings.md § Spam threshold slider labels). Verifies the threshold band label
 * comes from the (seeded) shared-contract band function, and that the retired
 * `auto-train` / `share-model` controls are gone.
 *
 * Renders with seeded state — no Hilt, no VM, no FFI native calls (the
 * spam_threshold_band() UniFFI call lives in the VM and is injected here as a
 * plain function). The cross-app `test_settings_privacy.py
 * --client android` is the standing gate once the host emulator lands.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PrivacySettingsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        inboxMode: String? = null,
        spamThreshold: Float = 0.5f,
        bandKeyFor: (Float) -> String = { "moderate" },
        emailFilters: List<FfiEmailFilter> = emptyList(),
        editingFilterId: Long? = null,
        editFilterPrefill: FilterForm? = null,
        isFilterEditable: (FfiEmailFilter) -> Boolean = { true },
        filterMarks: List<Long> = emptyList(),
        onKeepFilterMark: (Long) -> Unit = {},
        onDeleteFilter: (Long) -> Unit = {},
    ) {
        composeTestRule.setContent {
            PrivacySettingsContent(
                inboxMode = inboxMode,
                inboxModeLoading = false,
                inboxModeError = null,
                emailFilters = emailFilters,
                emailFilterError = null,
                creatingFilter = false,
                editingFilterId = editingFilterId,
                editFilterPrefill = editFilterPrefill,
                isFilterEditable = isFilterEditable,
                spamThreshold = spamThreshold,
                phishingThreshold = 0.5f,
                spamPrefsLoading = false,
                spamPrefsSaved = false,
                spamPrefsError = null,
                bandKeyFor = bandKeyFor,
                onBack = {},
                onInboxModeChange = {},
                onDeleteFilter = onDeleteFilter,
                onCreateFilter = {},
                onEditFilter = {},
                onSaveFilter = {},
                onCancelEditFilter = {},
                onSpamThresholdChange = {},
                onPhishingThresholdChange = {},
                onSaveSpamPrefs = {},
                filterMarks = filterMarks,
                onKeepFilterMark = onKeepFilterMark,
            )
        }
    }

    /** Scroll the LazyColumn until the spam-preferences section is composed. */
    private fun scrollToSpamControls() {
        composeTestRule.onNode(hasScrollAction()).performScrollToNode(hasTestTag("save-spam-prefs"))
    }

    @Test
    fun rendersPageHeading() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
    }

    @Test
    fun inboxModeRendersCanonicalOptions() {
        render()
        // The four canonical inbox-mode test ids (ui.yaml `inbox-mode-selector`,
        // wire values open/allow_knock/contacts_only/closed) each render as a row.
        composeTestRule.onNodeWithTag("inbox-mode-open").assertExists()
        composeTestRule.onNodeWithTag("inbox-mode-allow_knock").assertExists()
        composeTestRule.onNodeWithTag("inbox-mode-contacts_only").assertExists()
        composeTestRule.onNodeWithTag("inbox-mode-closed").assertExists()
        // Labels come from the shared `status.inbox_privacy.*` strings.
        composeTestRule.onNodeWithText("Open").assertExists()
        composeTestRule.onNodeWithText("Allow Knocks").assertExists()
        composeTestRule.onNodeWithText("Contacts Only").assertExists()
        composeTestRule.onNodeWithText("Closed").assertExists()
    }

    @Test
    fun inboxModeSelectionReflectsWireValue() {
        render(inboxMode = "contacts_only")
        composeTestRule.onNodeWithTag("inbox-mode-contacts_only").assertIsSelected()
        composeTestRule.onNodeWithTag("inbox-mode-open").assertIsNotSelected()
    }

    /** settings.md § Privacy sub-page item 7 ("show the stored mode, never a
     *  default") — before the fetch resolves, `inboxMode` is `null` and none
     *  of the four radios may paint selected; the page says why instead
     *  (mirrors the apple leg's PrivacySettingsView.swift). Guards against
     *  regressing to a hardcoded pre-fetch guess like the pre-fix
     *  `"allow_knock"` default this row closed. */
    @Test
    fun noOptionSelectedAndUnknownMessageShownBeforeFirstFetch() {
        render(inboxMode = null)
        composeTestRule.onNodeWithTag("inbox-mode-open").assertIsNotSelected()
        composeTestRule.onNodeWithTag("inbox-mode-allow_knock").assertIsNotSelected()
        composeTestRule.onNodeWithTag("inbox-mode-contacts_only").assertIsNotSelected()
        composeTestRule.onNodeWithTag("inbox-mode-closed").assertIsNotSelected()
        composeTestRule.onNodeWithText(
            "Your current inbox mode has not loaded, so none of the four below is marked. " +
                "Your setting is unchanged — reopen this page once your nest is reachable to see and change it."
        ).assertExists()
    }

    @Test
    fun invalidLegacyInboxModeLabelsAreGone() {
        render()
        // The pre-conformance hardcoded English labels off the non-canonical wire
        // set (allow_all/allow_confirmed/deny_all) must not appear anywhere.
        composeTestRule.onNodeWithText("Allow all").assertDoesNotExist()
        composeTestRule.onNodeWithText("Confirmed only").assertDoesNotExist()
        composeTestRule.onNodeWithText("Deny all").assertDoesNotExist()
    }

    @Test
    fun rendersSpamControls() {
        render()
        scrollToSpamControls()
        composeTestRule.onNodeWithTag("spam-preferences").assertExists()
        composeTestRule.onNodeWithTag("spam-threshold").assertExists()
        composeTestRule.onNodeWithTag("phishing-threshold").assertExists()
        composeTestRule.onNodeWithTag("save-spam-prefs").assertExists()
    }

    /** The two preferences the universal spam seal orphaned are removed
     *  (mail-spam.md § Implicit signals are forbidden): neither control renders. */
    @Test
    fun retiredAutoTrainAndShareModelControlsAreGone() {
        render()
        scrollToSpamControls()
        composeTestRule.onNodeWithTag("auto-train").assertDoesNotExist()
        composeTestRule.onNodeWithTag("share-model").assertDoesNotExist()
    }

    @Test
    fun bandLabelRendersFromSharedContract() {
        render(spamThreshold = 0.1f, bandKeyFor = { "aggressive" })
        scrollToSpamControls()
        composeTestRule.onNodeWithText("Aggressive").assertExists()
    }

    @Test
    fun bandLabelReflectsPermissiveBand() {
        render(spamThreshold = 0.9f, bandKeyFor = { "permissive" })
        scrollToSpamControls()
        composeTestRule.onNodeWithText("Permissive").assertExists()
    }

    /** The e2e journey reads the band word by `spam-threshold-band`, so the tag
     *  must sit on the node that carries the word, for every band. */
    private fun assertBandTag(value: Float, key: String, word: String) {
        render(spamThreshold = value, bandKeyFor = { key })
        scrollToSpamControls()
        composeTestRule.onNodeWithTag("spam-threshold-band").assertTextEquals(word)
    }

    @Test
    fun aggressiveBandWordIsReadableByItsTestTag() = assertBandTag(0.1f, "aggressive", "Aggressive")

    @Test
    fun moderateBandWordIsReadableByItsTestTag() = assertBandTag(0.5f, "moderate", "Moderate")

    @Test
    fun permissiveBandWordIsReadableByItsTestTag() = assertBandTag(0.9f, "permissive", "Permissive")

    private fun sampleFilter(id: Long = 1) = com.fauna.ffi.FfiEmailFilter(
        id = id,
        name = "Block spam",
        rules = listOf(com.fauna.ffi.FfiEmailFilterRule.SenderIs(address = "spammer@example.com")),
        combination = "all",
        action = com.fauna.ffi.FfiEmailFilterAction.Discard,
        priority = 0,
        createdAt = 0L,
    )

    /** Scroll the LazyColumn until the filter list is composed — same
     *  necessity as [scrollToSpamControls]: Section 1 (Inbox mode) pushes the
     *  filter rows below Robolectric's default viewport. */
    private fun scrollToFilterItem() {
        composeTestRule.onNode(hasScrollAction()).performScrollToNode(hasTestTag("filter-item"))
    }

    @Test
    fun filterEditButtonRendersOnlyWhenGatedEditable() {
        render(emailFilters = listOf(sampleFilter()), isFilterEditable = { true })
        scrollToFilterItem()
        composeTestRule.onNodeWithTag("filter-edit").assertExists()
    }

    @Test
    fun filterEditButtonAbsentWhenNotEditable() {
        // A filter only a raw API call could have produced (multi-rule, or a
        // rule/action outside the dialog's dropdown-covered subset) must never
        // open a form that would silently narrow it on save.
        render(emailFilters = listOf(sampleFilter()), isFilterEditable = { false })
        scrollToFilterItem()
        composeTestRule.onNodeWithTag("filter-edit").assertDoesNotExist()
    }

    @Test
    fun editingFilterOpensSheetPrePopulatedWithSaveFilterButton() {
        render(
            emailFilters = listOf(sampleFilter()),
            editingFilterId = 1L,
            editFilterPrefill = FilterForm(
                name = "Block spam",
                rule = FfiFilterRuleInputs(kind = "SenderIs", value = "spammer@example.com"),
                action = FfiFilterActionInputs(
                    kind = "Discard", rejectReason = "", forwardAddress = "", keepLocalCopy = true,
                ),
                priority = "0",
                continueOnMatch = false,
            ),
        )
        // save-filter (not create-filter) is the submit action while editing —
        // the sheet renders as an overlay, so no scroll is needed to reach it.
        composeTestRule.onNodeWithTag("save-filter").assertExists()
        composeTestRule.onNodeWithTag("create-filter").assertDoesNotExist()
        composeTestRule.onNodeWithTag("filter-name-input")
            .assert(hasText("Block spam", substring = true))
        composeTestRule.onNodeWithTag("filter-rule-value")
            .assert(hasText("spammer@example.com", substring = true))
    }

    // -- The post-succession review (succession-aftermath.md § Adjudicating
    // what the aftermath carries across, the email-filter plane) --

    @Test
    fun theReviewMarkAndKeepRenderOnlyOnAMarkedRule() {
        render(
            // The marked rule first and the unmarked one after it, both in
            // Robolectric's viewport once the list is scrolled to its rows.
            emailFilters = listOf(sampleFilter(id = 2).copy(name = "Forward old"), sampleFilter(id = 1)),
            filterMarks = listOf(2L),
        )
        scrollToFilterItem()
        // The mark is a Text inside the merged ListItem row, so it is read off
        // the unmerged tree (filter-name's own case).
        composeTestRule.onAllNodesWithTag("filter-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("filter-unattested-mark", useUnmergedTree = true).assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("filter-review-keep-button", useUnmergedTree = true).assertCountEquals(1)
        // Both rows keep their own delete — the Remove half; there is
        // deliberately no second remove button.
        composeTestRule.onAllNodesWithTag("filter-delete", useUnmergedTree = true).assertCountEquals(2)
    }

    @Test
    fun noMarksMeansNoReviewAffordance() {
        render(emailFilters = listOf(sampleFilter()))
        scrollToFilterItem()
        composeTestRule.onNodeWithTag("filter-unattested-mark", useUnmergedTree = true).assertDoesNotExist()
        composeTestRule.onNodeWithTag("filter-review-keep-button").assertDoesNotExist()
    }

    @Test
    fun keepAndDeleteHandTheMarkedRuleIdToTheirCallbacks() {
        val kept = mutableListOf<Long>()
        val deleted = mutableListOf<Long>()
        render(
            emailFilters = listOf(sampleFilter(id = 7)),
            filterMarks = listOf(7L),
            onKeepFilterMark = { kept += it },
            onDeleteFilter = { deleted += it },
        )
        scrollToFilterItem()
        composeTestRule.onNodeWithTag("filter-review-keep-button").performClick()
        composeTestRule.onNodeWithTag("filter-delete").performClick()
        assert(kept == listOf(7L)) { "Keep must carry the marked rule's id, got $kept" }
        assert(deleted == listOf(7L)) { "delete must carry the marked rule's id, got $deleted" }
    }
}
