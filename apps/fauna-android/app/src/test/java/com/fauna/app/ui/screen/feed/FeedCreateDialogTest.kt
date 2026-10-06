package com.fauna.app.ui.screen.feed

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiTrainedTopicRow
import org.robolectric.annotation.Config
import uniffi.fauna_feed.FactorWeightInput
import uniffi.fauna_feed.FilterRuleInput

/**
 * Compose-level coverage for [FeedCreateDialog] — the create-feed rule builder
 * (feed.md § Filter rule types; ui.yaml feed-create element scope). Verifies the
 * `feed-rule-*` element scope renders and that the name / combination / rule
 * accumulation wire correctly into the `(name, combination, rules)` the dialog
 * hands to `FeedManager::create_feed`.
 *
 * The dialog accumulates plain [FilterRuleInput] `(ruleType, value, required)`
 * triples; the shared `encode_filter_rule` encoding happens one layer up in the
 * manager (covered by `libs/fauna-ffi` + `libs/fauna-client-feed` Rust tests). The
 * rule-type catalog, chip summary, and required/excluded label DO cross the FFI
 * (`com.fauna.ffi.ruleTypeOptions`/`ruleSummaryLabel`/`ruleRequiredLabel`) — this
 * test needs the host `.so` (run via `just android-host-test`, not plain gradle).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FeedCreateDialogTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        onCreate: (String, String, List<FilterRuleInput>, List<FactorWeightInput>) -> Unit = { _, _, _, _ -> },
        onDismiss: () -> Unit = {},
        fetchTrainedFactors: suspend () -> List<FfiTrainedTopicRow> = { emptyList() },
    ) {
        composeTestRule.setContent {
            FeedCreateDialog(
                onDismiss = onDismiss,
                onCreate = onCreate,
                fetchTrainedFactors = fetchTrainedFactors,
            )
        }
    }

    @Test
    fun rendersRuleBuilderElements() {
        render()
        // The default rule type (HasHashtag) is TEXT-kind: the value entry shows,
        // the required/excluded toggle (TOGGLE-kind only) does not.
        for (tag in listOf(
            "feed-create-feed-name",
            "feed-combination-select",
            "feed-rule-type-select",
            "feed-rule-value-input",
            "feed-add-rule-button",
            "feed-factor-select",
            "feed-factor-weight-input",
            "feed-factor-global-toggle",
            "feed-add-factor-button",
            "create-feed",
            "feed-create-cancel",
        )) {
            composeTestRule.onNodeWithTag(tag).assertExists("missing $tag")
        }
        composeTestRule.onNodeWithTag("feed-rule-required-toggle").assertDoesNotExist()
    }

    @Test
    fun ruleTypeSelectShowsLocalizedLabelsNotRawWireKeys() {
        render()
        // Default selection (HasHashtag) renders its localized label, not the raw
        // FilterRule variant name the select's VALUE still is — it appears twice
        // (the collapsed field + the value-input's label, which mirrors the
        // selected type's name for non-threshold rules), so assert via count
        // rather than a single-match onNodeWithText.
        composeTestRule.onAllNodesWithText("Has Hashtag").assertCountEquals(2)
        composeTestRule.onNodeWithTag("feed-rule-type-select").performClick()
        composeTestRule.onNodeWithText("Has Media").assertExists()
        // Below the dropdown popup's fold — needs a scroll before it's assertable,
        // same as any other below-the-fold node in this test harness.
        composeTestRule.onNodeWithText("Label Below (exclude spam)").performScrollTo().assertExists()
    }

    @Test
    fun booleanRuleTypeShowsRequiredExcludedToggleAndHidesValueInput() {
        render()
        composeTestRule.onNodeWithTag("feed-rule-type-select").performClick()
        composeTestRule.onNodeWithText("Has Media").performClick()

        composeTestRule.onNodeWithTag("feed-rule-value-input").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feed-rule-required-toggle").assertExists()
        // Default checked = required; the label flips to "Excluded" on uncheck (the
        // nest evaluates required:false as a genuine exclusion — feed.md § Where
        // logic lives → Feed rule-builder presentation; rule_required_label_flips_on_state
        // pins the Rust side).
        composeTestRule.onNodeWithText("Required").assertExists()
        composeTestRule.onNodeWithTag("feed-rule-required-toggle").assertIsOn()
        composeTestRule.onNodeWithTag("feed-rule-required-toggle").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("feed-rule-required-toggle").assertIsOff()
        composeTestRule.onNodeWithText("Excluded").assertExists()
    }

    @Test
    fun labelRuleTypeShowsCategoryAndThresholdInputs() {
        render()
        composeTestRule.onNodeWithTag("feed-rule-type-select").performClick()
        composeTestRule.onNodeWithText("Label Below (exclude spam)").performScrollTo().performClick()

        composeTestRule.onNodeWithTag("feed-rule-value-input").assertExists()
        composeTestRule.onNodeWithText("Category").assertExists()
        composeTestRule.onNodeWithText("Threshold (0-10)").assertExists()
        composeTestRule.onNodeWithTag("feed-rule-required-toggle").assertDoesNotExist()
    }

    @Test
    fun createDisabledUntilNamed() {
        render()
        composeTestRule.onNodeWithTag("create-feed").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("feed-create-feed-name").performTextInput("My Feed")
        composeTestRule.onNodeWithTag("create-feed").assertIsEnabled()
    }

    @Test
    fun createWithNoRulesSubmitsEmptyList() {
        var name: String? = null
        var combination: String? = null
        var rules: List<FilterRuleInput>? = null
        var factors: List<FactorWeightInput>? = null
        render(onCreate = { n, c, r, f -> name = n; combination = c; rules = r; factors = f })
        composeTestRule.onNodeWithTag("feed-create-feed-name").performTextInput("My Feed")
        composeTestRule.onNodeWithTag("create-feed").performClick()
        assertEquals("My Feed", name)
        assertEquals("all", combination)
        assertEquals(emptyList<FilterRuleInput>(), rules)
        assertEquals(emptyList<FactorWeightInput>(), factors)
    }

    @Test
    fun addRuleThenCreateAccumulatesRule() {
        var rules: List<FilterRuleInput>? = null
        // Default rule type is HasHashtag (index 0); required defaults to true.
        render(onCreate = { _, _, r, _ -> rules = r })
        composeTestRule.onNodeWithTag("feed-create-feed-name").performTextInput("Tags")
        composeTestRule.onNodeWithTag("feed-rule-value-input").performTextInput("rust")
        composeTestRule.onNodeWithTag("feed-add-rule-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("create-feed").performClick()

        // The accumulated triple rides verbatim to create_feed, which encodes it
        // via the shared encode_filter_rule.
        assertEquals(
            listOf(FilterRuleInput(ruleType = "HasHashtag", value = "rust", required = true)),
            rules,
        )
    }

    @Test
    fun addedRuleChipRendersSharedSummaryLabelNotRawWireKey() {
        render()
        composeTestRule.onNodeWithTag("feed-rule-value-input").performTextInput("rust")
        composeTestRule.onNodeWithTag("feed-add-rule-button").performScrollTo().performClick()

        // rule_summary_label("HasHashtag", "rust", true) resolves to "#rust"
        // (feed.rule_chip.has_hashtag = "{tags}") — not the raw "HasHashtag" key
        // this chip used to join.
        composeTestRule.onNodeWithText("#rust").assertExists()
    }

    @Test
    fun addRuleButtonDisabledForEmptyOrUnparseableInput() {
        // `com.fauna.ffi.canAddRule` — apple's `FeedCreateForm.canAddRule`,
        // lifted (feed.md § Add-rule gating). android rendered this button
        // permanently enabled until this fix.
        render()
        // Default rule type HasHashtag (Text kind): blank value -> disabled.
        composeTestRule.onNodeWithTag("feed-add-rule-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("feed-rule-value-input").performTextInput("rust")
        composeTestRule.onNodeWithTag("feed-add-rule-button").assertIsEnabled()

        // Number kind (MinReplies): unparseable value -> disabled.
        composeTestRule.onNodeWithTag("feed-rule-type-select").performClick()
        composeTestRule.onNodeWithText("Min Replies").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("feed-rule-value-input").performTextClearance()
        composeTestRule.onNodeWithTag("feed-add-rule-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("feed-rule-value-input").performTextInput("5")
        composeTestRule.onNodeWithTag("feed-add-rule-button").assertIsEnabled()

        // TextAndNumber kind (LabelBelow): needs BOTH a category and a
        // parseable threshold. `ruleValue` carries over from the Number check
        // above (the field is not reset on a type switch), so clear it first —
        // the threshold prefills to the shared midpoint and stays parseable.
        composeTestRule.onNodeWithTag("feed-rule-type-select").performClick()
        composeTestRule.onNodeWithText("Label Below (exclude spam)").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("feed-rule-value-input").performTextClearance()
        composeTestRule.onNodeWithTag("feed-add-rule-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("feed-rule-value-input").performTextInput("spam")
        composeTestRule.onNodeWithTag("feed-add-rule-button").assertIsEnabled()
    }

    @Test
    fun cancelFiresDismiss() {
        var dismissed = false
        render(onDismiss = { dismissed = true })
        composeTestRule.onNodeWithTag("feed-create-cancel").performClick()
        assertTrue(dismissed)
    }

    // ── Factor builder (topic-factors.md § Authoring surface; ui.yaml
    // feed.sub_pages.create_feed) — the FfiTrainedTopicRow list arrives via
    // [fetchTrainedFactors], mirroring TrainTargetSheet's own FFI-touching
    // fetch pattern, so this dialog itself stays a pure Compose leaf. ──────

    private fun trainedRow(name: String, factorKey: String) = FfiTrainedTopicRow(
        id = ByteArray(16),
        name = name,
        factorKey = factorKey,
        exampleCount = 0u,
        learnFromEngagement = false,
    )

    @Test
    fun addFactorWithDefaultsAccumulatesEngagementAtWeightOne() {
        var factors: List<FactorWeightInput>? = null
        // engagement is the built-in default selection; weight defaults to
        // "1.0" (parseable), so the add button needs no input to be usable —
        // the same shape as macOS's FeedCreateForm.canAddFactor default.
        render(onCreate = { _, _, _, f -> factors = f })
        composeTestRule.onNodeWithTag("feed-create-feed-name").performTextInput("My Feed")
        composeTestRule.onNodeWithTag("feed-add-factor-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("create-feed").performClick()

        assertEquals(
            listOf(FactorWeightInput(factor = "engagement", weightPermille = 1000, global = false)),
            factors,
        )
    }

    @Test
    fun globalToggleFlipsTheAccumulatedFactorsGlobalFlag() {
        var factors: List<FactorWeightInput>? = null
        render(onCreate = { _, _, _, f -> factors = f })
        composeTestRule.onNodeWithTag("feed-create-feed-name").performTextInput("My Feed")
        composeTestRule.onNodeWithTag("feed-factor-global-toggle").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("feed-add-factor-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("create-feed").performClick()

        assertEquals(true, factors?.single()?.global)
    }

    @Test
    fun customWeightParsesToWeightPermille() {
        var factors: List<FactorWeightInput>? = null
        render(onCreate = { _, _, _, f -> factors = f })
        composeTestRule.onNodeWithTag("feed-create-feed-name").performTextInput("My Feed")
        composeTestRule.onNodeWithTag("feed-factor-weight-input").performScrollTo().performTextReplacement("2.0")
        composeTestRule.onNodeWithTag("feed-add-factor-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("create-feed").performClick()

        assertEquals(2000L, factors?.single()?.weightPermille)
    }

    @Test
    fun addFactorButtonDisabledWhenWeightUnparseable() {
        render()
        composeTestRule.onNodeWithTag("feed-factor-weight-input").performScrollTo().performTextReplacement("not-a-number")
        composeTestRule.onNodeWithTag("feed-add-factor-button").performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun factorSelectOffersFetchedTrainedFactorsAlongsideEngagement() {
        render(fetchTrainedFactors = { listOf(trainedRow("Rust news", "topic:aabb")) })
        composeTestRule.onNodeWithTag("feed-factor-select").performScrollTo().performClick()
        // "Engagement" is the default selection, so it appears twice — the
        // collapsed field's current value AND the open dropdown's own option
        // (same ambiguity ruleTypeSelectShowsLocalizedLabelsNotRawWireKeys
        // handles above).
        composeTestRule.onAllNodesWithText("Engagement").assertCountEquals(2)
        composeTestRule.onNodeWithText("Rust news").assertExists()
    }

    @Test
    fun selectingATrainedFactorThenAddingUsesItsFactorKeyNotItsDisplayName() {
        var factors: List<FactorWeightInput>? = null
        render(
            onCreate = { _, _, _, f -> factors = f },
            fetchTrainedFactors = { listOf(trainedRow("Rust news", "topic:aabb")) },
        )
        composeTestRule.onNodeWithTag("feed-create-feed-name").performTextInput("My Feed")
        composeTestRule.onNodeWithTag("feed-factor-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Rust news").performClick()
        composeTestRule.onNodeWithTag("feed-add-factor-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("create-feed").performClick()

        assertEquals("topic:aabb", factors?.single()?.factor)
    }

    @Test
    fun addedFactorRowRendersSharedFormattedWeight() {
        render()
        composeTestRule.onNodeWithTag("feed-create-feed-name").performTextInput("My Feed")
        composeTestRule.onNodeWithTag("feed-factor-weight-input").performScrollTo().performTextReplacement("2.0")
        composeTestRule.onNodeWithTag("feed-add-factor-button").performScrollTo().performClick()

        // format_weight_permille(2000) — the shared inverse of parse_weight_permille;
        // mirrors the rule chip's use of a shared formatter rather than a raw value.
        composeTestRule.onNodeWithText("engagement", substring = true).assertExists()
        composeTestRule.onNodeWithText("×2", substring = true).assertExists()
    }
}
