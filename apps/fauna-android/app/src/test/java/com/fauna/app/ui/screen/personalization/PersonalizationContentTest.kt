package com.fauna.app.ui.screen.personalization

import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.ui.viewmodel.PUBLISH_KIND_LIST
import com.fauna.app.ui.viewmodel.PUBLISH_KIND_MODEL
import com.fauna.app.ui.viewmodel.PublishExemplarUi
import com.fauna.app.ui.viewmodel.PublishNgramUi
import com.fauna.app.ui.viewmodel.PublishSheetUiState
import com.fauna.ffi.FfiReportShareEntry
import com.fauna.ffi.FfiTrainedTopicRow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.testing.LabelerCatalogEntryFixture
import org.robolectric.annotation.Config
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogSnapshot

/**
 * Compose-level coverage for the stateless [PersonalizationContent] (the
 * `personalization` Settings sub-page, `content-moderation-and-ranking.md` §
 * Composition). Renders with a seeded [LabelerCatalogSnapshot] — no Hilt, no
 * VM, no FFI native calls — verifying the ui.yaml ids render and the facet
 * links + inline unsubscribe gesture fire.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PersonalizationContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun entry(id: String, subscribed: Boolean) =
        LabelerCatalogEntryFixture.make(labelerId = id, subscribed = subscribed)

    private fun topic(
        id: ByteArray = byteArrayOf(1, 2, 3),
        name: String = "Cats",
        factorKey: String? = "topic:010203",
        exampleCount: UInt = 3u,
        learnFromEngagement: Boolean = false,
    ) = FfiTrainedTopicRow(
        id = id,
        name = name,
        factorKey = factorKey,
        exampleCount = exampleCount,
        learnFromEngagement = learnFromEngagement,
    )

    private fun signalEntry(hash: String, factor: String = "signal:watch-complete", count: UInt = 3u) =
        FfiReportShareEntry(contentHash = hash, factor = factor, count = count)

    private fun render(
        snapshot: LabelerCatalogSnapshot? = LabelerCatalogSnapshot(emptyList(), null, null, loaded = true),
        topics: List<FfiTrainedTopicRow> = emptyList(),
        topicName: String = "",
        isRenaming: Boolean = false,
        topicsBusy: Boolean = false,
        publishState: PublishSheetUiState? = null,
        shareSignals: Boolean = false,
        signalPublished: List<FfiReportShareEntry> = emptyList(),
        onUnsubscribe: (Int) -> Unit = {},
        onTopicNameChange: (String) -> Unit = {},
        onSubmitTopic: () -> Unit = {},
        onStartRename: (FfiTrainedTopicRow) -> Unit = {},
        onDeleteTopic: (FfiTrainedTopicRow) -> Unit = {},
        onToggleEngagement: (FfiTrainedTopicRow) -> Unit = {},
        onOpenPublish: (FfiTrainedTopicRow) -> Unit = {},
        onPublishNameChange: (String) -> Unit = {},
        onTogglePublishInclude: (Int) -> Unit = {},
        publishKindOptions: List<Pair<String, String>> = KIND_OPTIONS,
        onPublishKindChange: (String) -> Unit = {},
        onTogglePublishNgramInclude: (Int) -> Unit = {},
        onSubmitPublish: () -> Unit = {},
        onCancelPublish: () -> Unit = {},
        onToggleShareSignals: (Boolean) -> Unit = {},
        onClearEngagementData: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            PersonalizationContent(
                snapshot = snapshot,
                topics = topics,
                topicName = topicName,
                isRenaming = isRenaming,
                topicsBusy = topicsBusy,
                publishState = publishState,
                shareSignals = shareSignals,
                signalPublished = signalPublished,
                onBack = {},
                onFeedsLink = {},
                onMutedWordsLink = {},
                onBrowseCatalog = {},
                onUnsubscribe = onUnsubscribe,
                onTopicNameChange = onTopicNameChange,
                onSubmitTopic = onSubmitTopic,
                onStartRename = onStartRename,
                onDeleteTopic = onDeleteTopic,
                onToggleEngagement = onToggleEngagement,
                onOpenPublish = onOpenPublish,
                onPublishNameChange = onPublishNameChange,
                onTogglePublishInclude = onTogglePublishInclude,
                publishKindOptions = publishKindOptions,
                onPublishKindChange = onPublishKindChange,
                onTogglePublishNgramInclude = onTogglePublishNgramInclude,
                onSubmitPublish = onSubmitPublish,
                onCancelPublish = onCancelPublish,
                onToggleShareSignals = onToggleShareSignals,
                onClearEngagementData = onClearEngagementData,
            )
        }
    }

    @Test
    fun rendersStaticIds() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("personalization").assertExists()
        composeTestRule.onNodeWithTag("personalization-feeds-link").assertExists()
        composeTestRule.onNodeWithTag("personalization-muted-words-link").assertExists()
        composeTestRule.onNodeWithTag("personalization-browse-catalog-button").assertExists()
    }

    @Test
    fun emptyStateWhenNoSubscribedLabelers() {
        render(snapshot = LabelerCatalogSnapshot(listOf(entry("a", subscribed = false)), null, null, loaded = true))
        composeTestRule.onNodeWithTag("personalization-labelers-empty").assertExists()
    }

    /**
     * The subscribed facet is DERIVED from the catalog snapshot, so it inherits
     * that snapshot's `loaded` bit rather than inventing one (`README.md`
     * § *List pages: loading is not empty*, rule 4). Before the first
     * `fauna.labelers.list` returns, the facet is empty and must stay silent.
     *
     * Paired with [emptyStateWhenNoSubscribedLabelers] deliberately: only both
     * halves together catch a gate wired to a field that never becomes true,
     * which would hide the empty state forever.
     */
    @Test
    fun subscribedEmptyStateWithheldWhileTheCatalogReadIsStillInFlight() {
        render(snapshot = LabelerCatalogSnapshot(emptyList(), null, null, loaded = false))
        composeTestRule.onNodeWithTag("personalization-labelers-empty").assertDoesNotExist()
        composeTestRule.onNodeWithTag("personalization-labelers-list").assertDoesNotExist()
    }

    /** A null snapshot is the page's very first frame — also not loaded. */
    @Test
    fun subscribedEmptyStateWithheldBeforeAnySnapshotArrives() {
        render(snapshot = null)
        composeTestRule.onNodeWithTag("personalization-labelers-empty").assertDoesNotExist()
    }

    @Test
    fun listRendersOnlySubscribedLabelers() {
        render(
            snapshot = LabelerCatalogSnapshot(
                listOf(entry("a", subscribed = true), entry("b", subscribed = false)),
                null,
                null,
                loaded = true,
            ),
        )
        composeTestRule.onNodeWithTag("personalization-labelers-list").assertExists()
        composeTestRule.onAllNodesWithTag("labeler-catalog-item").assertCountEquals(1)
        // Inspect/subscribe are hidden on the home facet; unsubscribe stays inline.
        composeTestRule.onAllNodesWithTag("labeler-catalog-item-inspect-button").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("labeler-catalog-item-subscribe-button").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("labeler-catalog-item-unsubscribe-button").assertCountEquals(1)
    }

    @Test
    fun unsubscribeFiresAtOriginalIndex() {
        var unsubscribed = -1
        render(
            snapshot = LabelerCatalogSnapshot(
                listOf(entry("a", subscribed = false), entry("b", subscribed = true)),
                null,
                null,
                loaded = true,
            ),
            onUnsubscribe = { unsubscribed = it },
        )
        composeTestRule.onNodeWithTag("labeler-catalog-item-unsubscribe-button").performScrollTo().performClick()
        assertEquals(1, unsubscribed)
    }

    // ── Trained-topics facet (topic-factors.md § Authoring surface & picker,
    // S8) ─────────────────────────────────────────────────────────────────

    @Test
    fun trainedTopicsStaticIdsRender() {
        render()
        composeTestRule.onNodeWithTag("personalization-trained-factor-name-input").assertExists()
        composeTestRule.onNodeWithTag("personalization-trained-factor-create-button").assertExists()
        composeTestRule.onNodeWithTag("personalization-trained-factor-list").assertExists()
    }

    @Test
    fun trainedTopicsListRendersEveryRow() {
        render(topics = listOf(topic(name = "Cats"), topic(id = byteArrayOf(4, 5, 6), name = "Dogs")))
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-name")[0].assertTextEquals("Cats")
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-name")[1].assertTextEquals("Dogs")
    }

    @Test
    fun createButtonSubmitsTypedName() {
        var submitted = false
        var typed = ""
        render(
            onTopicNameChange = { typed = it },
            onSubmitTopic = { submitted = true },
        )
        composeTestRule.onNodeWithTag("personalization-trained-factor-name-input").performTextInput("Cats")
        composeTestRule.onNodeWithTag("personalization-trained-factor-create-button").performScrollTo().performClick()
        assertTrue(submitted)
    }

    @Test
    fun renameButtonFiresWithTheRow() {
        var renamed: FfiTrainedTopicRow? = null
        val row = topic(name = "Cats")
        render(topics = listOf(row), onStartRename = { renamed = it })
        composeTestRule.onNodeWithTag("personalization-trained-factor-rename-button").performScrollTo().performClick()
        assertEquals("Cats", renamed?.name)
    }

    @Test
    fun deleteButtonFiresWithTheRow() {
        var deleted: FfiTrainedTopicRow? = null
        val row = topic(name = "Cats")
        render(topics = listOf(row), onDeleteTopic = { deleted = it })
        composeTestRule.onNodeWithTag("personalization-trained-factor-delete-button").performScrollTo().performClick()
        assertEquals("Cats", deleted?.name)
    }

    @Test
    fun engagementToggleFiresWithTheRow() {
        var toggled: FfiTrainedTopicRow? = null
        val row = topic(name = "Cats", learnFromEngagement = false)
        render(topics = listOf(row), onToggleEngagement = { toggled = it })
        composeTestRule.onNodeWithTag("personalization-trained-factor-engagement-toggle").performScrollTo().performClick()
        assertEquals("Cats", toggled?.name)
    }

    @Test
    fun publishButtonDisabledWhenFactorKeyIsNull() {
        render(topics = listOf(topic(factorKey = null)))
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-button").performScrollTo()
            .assertIsNotEnabled()
    }

    @Test
    fun publishButtonFiresWithTheRowWhenFactorKeyPresent() {
        var opened: FfiTrainedTopicRow? = null
        val row = topic(name = "Cats", factorKey = "topic:010203")
        render(topics = listOf(row), onOpenPublish = { opened = it })
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-button").performScrollTo()
            .assertIsEnabled()
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-button").performScrollTo().performClick()
        assertEquals("Cats", opened?.name)
    }

    // ── Publish review-prune sheet (topic-factors.md § Publishing a trained
    // factor) ────────────────────────────────────────────────────────────

    @Test
    fun publishSheetAbsentWhenClosed() {
        render(publishState = null)
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-sheet").assertDoesNotExist()
    }

    @Test
    fun publishSheetRendersExemplarsDefaultChecked() {
        render(
            publishState = PublishSheetUiState(
                exemplars = listOf(
                    PublishExemplarUi("post-1", "a cat post", 900L, included = true),
                    PublishExemplarUi("post-2", "a dog post", 400L, included = true),
                ),
                name = "",
                busy = false,
            ),
        )
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-sheet").assertExists()
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-exemplar-item")
            .assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-exemplar-checkbox")[0]
            .assertIsOn()
    }

    @Test
    fun publishSheetExemplarEmptyWhenNoExemplars() {
        render(publishState = PublishSheetUiState(exemplars = emptyList(), name = "", busy = false))
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-exemplar-empty").assertExists()
    }

    @Test
    fun publishSubmitDisabledWhenNothingIncluded() {
        render(
            publishState = PublishSheetUiState(
                exemplars = listOf(PublishExemplarUi("post-1", "a cat post", 900L, included = false)),
                name = "a name",
                busy = false,
            ),
        )
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-submit-button")
            .assertIsNotEnabled()
    }

    @Test
    fun publishSubmitEnabledWhenSomethingIncluded() {
        render(
            publishState = PublishSheetUiState(
                exemplars = listOf(PublishExemplarUi("post-1", "a cat post", 900L, included = true)),
                name = "a name",
                busy = false,
            ),
        )
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-submit-button").assertIsEnabled()
    }

    @Test
    fun toggleIncludeFiresAtIndex() {
        var toggledIndex = -1
        render(
            publishState = PublishSheetUiState(
                exemplars = listOf(
                    PublishExemplarUi("post-1", "a cat post", 900L, included = true),
                    PublishExemplarUi("post-2", "a dog post", 400L, included = true),
                ),
                name = "",
                busy = false,
            ),
            onTogglePublishInclude = { toggledIndex = it },
        )
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-exemplar-checkbox")[1]
            .performScrollTo().performClick()
        assertEquals(1, toggledIndex)
    }

    @Test
    fun cancelPublishFires() {
        var cancelled = false
        render(
            publishState = PublishSheetUiState(exemplars = emptyList(), name = "", busy = false),
            onCancelPublish = { cancelled = true },
        )
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-cancel-button")
            .performScrollTo().performClick()
        assertTrue(cancelled)
    }

    // ── Layer-B signal-sharing pane (engagement-cues.md § Layer B) ─────────

    @Test
    fun signalShareStaticIdsRender() {
        render()
        composeTestRule.onNodeWithTag("personalization-share-signals-toggle").assertExists()
        composeTestRule.onNodeWithTag("signal-share-published-list").assertExists()
    }

    @Test
    fun signalShareToggleReflectsState() {
        render(shareSignals = true)
        composeTestRule.onNodeWithTag("personalization-share-signals-toggle").assertIsOn()
    }

    @Test
    fun signalShareToggleFiresUserGesture() {
        var toggled: Boolean? = null
        render(shareSignals = false, onToggleShareSignals = { toggled = it })
        composeTestRule.onNodeWithTag("personalization-share-signals-toggle")
            .performScrollTo().performClick()
        assertEquals(true, toggled)
    }

    @Test
    fun signalPublishedListEmptyByDefault() {
        render()
        composeTestRule.onAllNodesWithTag("signal-share-published-list-item").assertCountEquals(0)
    }

    @Test
    fun signalPublishedListRendersEveryEntry() {
        render(
            signalPublished = listOf(
                signalEntry("aa".repeat(32), factor = "signal:watch-complete", count = 3u),
                signalEntry("bb".repeat(32), factor = "report:spam", count = 5u),
            ),
        )
        composeTestRule.onAllNodesWithTag("signal-share-published-list-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("signal-share-published-list-item-factor")[0]
            .assertTextEquals("signal:watch-complete")
        composeTestRule.onAllNodesWithTag("signal-share-published-list-item-factor")[1]
            .assertTextEquals("report:spam")
        composeTestRule.onAllNodesWithTag("signal-share-published-list-item-hash")[0]
            .assertTextEquals("aa".repeat(32))
    }

    /**
     * The count element carries the BARE number — not "3 contributors".
     * ui.yaml defines it as "the distinct local contributor count", linux
     * renders it as a bare value marker, and the cross-app e2e asserts the
     * element's text is exactly "3" (`test_engagement_cues.py`). The human
     * phrasing rides an adjacent Text, so the row still reads "3 contributors"
     * while the element stays machine-readable. Folding the word back into the
     * element would make android the one app whose count reads differently —
     * and no android e2e has ever run to catch it.
     */
    @Test
    fun signalPublishedCountIsTheBareNumber() {
        render(signalPublished = listOf(signalEntry("aa".repeat(32), count = 3u)))
        composeTestRule.onAllNodesWithTag("signal-share-published-list-item-count")[0]
            .assertTextEquals("3")
    }

    /**
     * The user-revocable affordance the capture invariant requires: android now
     * captures engagement cues, so the user MUST be able to erase them from
     * their own client (engagement-cues.md § At rest; the product invariant —
     * a user always controls their data). Shipping capture without this button
     * would ship capture with no way to revoke it.
     */
    @Test
    fun clearEngagementDataButtonRendersAndFires() {
        var cleared = false
        render(onClearEngagementData = { cleared = true })
        composeTestRule.onNodeWithTag("personalization-clear-engagement-data-button")
            .performScrollTo().performClick()
        assertTrue(cleared)
    }

    // ── The Model kind (topic-factors.md § Publishing a trained factor, v2)
    // — the kind select is a RAW-VALUE picker through the shared TokenSelect,
    // and these pins are what the androidTest bridge reads on a device:
    // `Role.DropdownList` (→ `android.widget.Spinner`, the widget-type rule
    // `get_text` keys on) + the token on `stateDescription`. ────────────────

    private fun ngram(text: String, included: Boolean = true, direction: String = "More like this") =
        PublishNgramUi(
            ngram = text,
            more = 3u,
            less = 0u,
            directionText = direction,
            countText = "In 3 posts",
            included = included,
        )

    private fun modelState(
        ngrams: List<PublishNgramUi>,
        name: String = "",
        exemplars: List<PublishExemplarUi> = emptyList(),
    ) = PublishSheetUiState(
        exemplars = exemplars,
        name = name,
        busy = false,
        kind = PUBLISH_KIND_MODEL,
        ngrams = ngrams,
        moreDocs = 3u,
        lessDocs = 3u,
        includedExamples = 4u,
        markedExamples = 6u,
    )

    @Test
    fun publishKindSelectCarriesTheListTokenAsADropdownByDefault() {
        render(publishState = PublishSheetUiState(exemplars = emptyList(), name = "", busy = false))
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-kind-select")
            .assertExists()
            .assert(hasStateDescription(PUBLISH_KIND_LIST))
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.DropdownList))
            // The human reads the label, never the token.
            .assertTextEquals("List of posts")
    }

    @Test
    fun publishKindSelectOffersEveryOptionByTokenAndFiresTheChange() {
        var chosen: String? = null
        render(
            publishState = PublishSheetUiState(exemplars = emptyList(), name = "", busy = false),
            onPublishKindChange = { chosen = it },
        )
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-kind-select")
            .performScrollTo().performClick()
        // The option is found by its VALUE (the bridge's `select(id, token)`
        // read), not by its visible label — and the anchor, which also
        // carries a token, is not an option.
        composeTestRule.onNode(hasStateDescription(PUBLISH_KIND_MODEL) and hasClickAction())
            .assertTextEquals("Word-pattern model")
            .performClick()
        assertEquals(PUBLISH_KIND_MODEL, chosen)
    }

    @Test
    fun modelKindRendersEveryNgramDefaultCheckedWithDirectionAndCount() {
        render(
            publishState = modelState(
                ngrams = listOf(ngram("orange tabby"), ngram("quarterly depreciation", direction = "Less like this")),
            ),
        )
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-kind-select")
            .assert(hasStateDescription(PUBLISH_KIND_MODEL))
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-ngram-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-ngram-checkbox")[0].assertIsOn()
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-ngram-text")[0]
            .assertTextEquals("orange tabby")
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-ngram-direction")[1]
            .assertTextEquals("Less like this")
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-ngram-count")[0]
            .assertTextEquals("In 3 posts")
        // The List body is gone — the two kinds review different objects.
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-exemplar-list").assertDoesNotExist()
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-exemplar-empty").assertDoesNotExist()
    }

    @Test
    fun modelKindWithNothingSurvivingRendersTheRefusal() {
        render(publishState = modelState(ngrams = emptyList()))
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-ngram-empty").assertExists()
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-ngram-item").assertCountEquals(0)
    }

    @Test
    fun modelKindLimitationNoteCarriesTheMandatedCopyAndTheCorpusSizeLine() {
        render(publishState = modelState(ngrams = listOf(ngram("orange tabby"))))
        val note = composeTestRule.onNodeWithTag("personalization-trained-factor-publish-limitation-note")
            .fetchSemanticsNode().config[SemanticsProperties.Text].joinToString { it.text }.lowercase()
        // The three ratified disclosures + the corpus-size line's two numbers.
        assertTrue(note, "seen" in note)
        assertTrue(note, "3" in note)
        assertTrue(note, "less like this" in note)
        assertTrue(note, "anonym" in note)
        assertTrue(note, "4 public examples" in note)
        assertTrue(note, "6 marked posts" in note)
    }

    @Test
    fun modelKindSubmitGatesOnTheActiveKindsRows() {
        // An included EXEMPLAR must not arm a Model publish.
        render(
            publishState = modelState(
                ngrams = listOf(ngram("orange tabby", included = false)),
                name = "a name",
                exemplars = listOf(PublishExemplarUi("post-1", "a cat post", 900L, included = true)),
            ),
        )
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-submit-button").assertIsNotEnabled()
    }

    @Test
    fun modelKindSubmitArmsOnceAnNgramIsKept() {
        render(publishState = modelState(ngrams = listOf(ngram("orange tabby")), name = "a name"))
        composeTestRule.onNodeWithTag("personalization-trained-factor-publish-submit-button").assertIsEnabled()
    }

    @Test
    fun toggleNgramIncludeFiresAtIndex() {
        var toggledIndex = -1
        render(
            publishState = modelState(ngrams = listOf(ngram("orange tabby"), ngram("quarterly depreciation"))),
            onTogglePublishNgramInclude = { toggledIndex = it },
        )
        composeTestRule.onAllNodesWithTag("personalization-trained-factor-publish-ngram-checkbox")[1]
            .performScrollTo().performClick()
        assertEquals(1, toggledIndex)
    }

    private companion object {
        /** What the screen resolves off `publish_kind_options` — List first. */
        val KIND_OPTIONS = listOf(
            PUBLISH_KIND_LIST to "List of posts",
            PUBLISH_KIND_MODEL to "Word-pattern model",
        )
    }
}
