package com.fauna.app.ui.screen.personalization

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.testing.LabelerCatalogEntryFixture
import com.fauna.app.testing.LabelerInspectViewFixture
import org.robolectric.annotation.Config
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogEntry
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogSnapshot
import uniffi.fauna_labeler_catalog_machine.LabelerInspectListEntry
import uniffi.fauna_labeler_catalog_machine.LabelerInspectModelNgram

/**
 * Compose-level coverage for the stateless [LabelerCatalogContent] (the
 * `labeler-catalog` Settings sub-page — browse + inspect-before-subscribe,
 * `content-moderation-and-ranking.md` § Tier-3 community models). Renders
 * with a seeded [LabelerCatalogSnapshot] — no Hilt, no VM, no FFI native
 * calls — verifying the ui.yaml ids render and the inspect/subscribe/
 * unsubscribe gestures fire at the right index.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class LabelerCatalogContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun entry(id: String, subscribed: Boolean, artifactKind: String = "wasm") =
        LabelerCatalogEntryFixture.make(labelerId = id, subscribed = subscribed, artifactKind = artifactKind)

    private fun inspectView(
        verified: Boolean = true,
        artifactKind: String = "wasm",
        listName: String? = null,
        listEntries: List<LabelerInspectListEntry> = emptyList(),
        // `text-model` kind only, the `list` pair's exact mirror — defaulted so
        // the wasm/list cases below stay unchanged.
        modelName: String? = null,
        modelNgrams: List<LabelerInspectModelNgram> = emptyList(),
    ) = LabelerInspectViewFixture.make(
        verified = verified,
        artifactKind = artifactKind,
        listName = listName,
        listEntries = listEntries,
        modelName = modelName,
        modelNgrams = modelNgrams,
    )

    private fun render(
        snapshot: LabelerCatalogSnapshot? = LabelerCatalogSnapshot(emptyList(), null, null, loaded = true),
        onInspect: (Int) -> Unit = {},
        onSubscribe: (Int) -> Unit = {},
        onUnsubscribe: (Int) -> Unit = {},
        onCloseInspect: () -> Unit = {},
        kindBadge: (LabelerCatalogEntry) -> String = { it.artifactKind },
        ngramLabels: (UInt, UInt) -> Pair<String, String> = { more, less ->
            (if (more >= less) "More like this" else "Less like this") to "In ${more + less} posts"
        },
    ) {
        composeTestRule.setContent {
            LabelerCatalogContent(
                snapshot = snapshot,
                onBack = {},
                onInspect = onInspect,
                onSubscribe = onSubscribe,
                onUnsubscribe = onUnsubscribe,
                onCloseInspect = onCloseInspect,
                kindBadge = kindBadge,
                ngramLabels = ngramLabels,
            )
        }
    }

    @Test
    fun rendersStaticIds() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("labeler-catalog").assertExists()
        composeTestRule.onNodeWithTag("labeler-catalog-empty").assertExists()
        composeTestRule.onNodeWithTag("labeler-catalog-list").assertDoesNotExist()
    }

    /**
     * The negative half of the three-state rule (`README.md` § *List pages:
     * loading is not empty*): `entries` is empty both before the first
     * `fauna.labelers.list` returns and after one that found nothing, so the
     * empty state takes `loaded` as its second painting condition.
     *
     * Asserted as a PAIR with [rendersStaticIds] above (which covers
     * loaded-and-empty) on purpose: a gate wired to a field that never becomes
     * true would make the empty state vanish forever — worse than the bug being
     * fixed — and only the pair catches that. There is no `*-loading` id to
     * assert; the absence of `labeler-catalog-empty` beside zero
     * `labeler-catalog-item` rows *is* the loading state.
     */
    @Test
    fun emptyStateWithheldWhileTheFirstReadIsStillInFlight() {
        render(snapshot = LabelerCatalogSnapshot(emptyList(), null, null, loaded = false))
        composeTestRule.onNodeWithTag("labeler-catalog-empty").assertDoesNotExist()
        composeTestRule.onNodeWithTag("labeler-catalog-list").assertDoesNotExist()
        composeTestRule.onAllNodesWithTag("labeler-catalog-item").assertCountEquals(0)
    }

    /** A null snapshot is the page's very first frame — also not loaded. */
    @Test
    fun emptyStateWithheldBeforeAnySnapshotArrives() {
        render(snapshot = null)
        composeTestRule.onNodeWithTag("labeler-catalog-empty").assertDoesNotExist()
    }

    @Test
    fun listRendersEveryPublishedLabelerWithInspectAndSubscribe() {
        render(
            snapshot = LabelerCatalogSnapshot(
                listOf(entry("a", subscribed = false), entry("b", subscribed = true)),
                null,
                null,
                loaded = true,
            ),
        )
        composeTestRule.onNodeWithTag("labeler-catalog-list").assertExists()
        composeTestRule.onAllNodesWithTag("labeler-catalog-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("labeler-catalog-item-inspect-button").assertCountEquals(2)
        // Row a: not subscribed -> subscribe button, no unsubscribe.
        // Row b: subscribed -> unsubscribe button, no subscribe.
        composeTestRule.onAllNodesWithTag("labeler-catalog-item-subscribe-button").assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("labeler-catalog-item-unsubscribe-button").assertCountEquals(1)
    }

    @Test
    fun inspectFiresAtIndex() {
        var inspected = -1
        render(
            snapshot = LabelerCatalogSnapshot(
                listOf(entry("a", subscribed = false), entry("b", subscribed = false)),
                null,
                null,
                loaded = true,
            ),
            onInspect = { inspected = it },
        )
        composeTestRule.onAllNodesWithTag("labeler-catalog-item-inspect-button")[1].performClick()
        assertEquals(1, inspected)
    }

    @Test
    fun subscribeFiresAtIndex() {
        var subscribed = -1
        render(
            snapshot = LabelerCatalogSnapshot(listOf(entry("a", subscribed = false)), null, null, loaded = true),
            onSubscribe = { subscribed = it },
        )
        composeTestRule.onNodeWithTag("labeler-catalog-item-subscribe-button").performClick()
        assertEquals(0, subscribed)
    }

    @Test
    fun inspectPanelRendersMetadataAndCloses() {
        var closed = false
        render(
            snapshot = LabelerCatalogSnapshot(emptyList(), inspectView(verified = true), null, loaded = true),
            onCloseInspect = { closed = true },
        )
        composeTestRule.onNodeWithTag("labeler-inspect-panel").assertExists()
        val metadata = composeTestRule.onNodeWithTag("labeler-inspect-metadata")
        metadata.assertExists()
        // Below the eleven-line metadata dump, so off the Robolectric screen
        // until scrolled — a tap on an unscrolled node is a silent no-op.
        composeTestRule.onNodeWithTag("labeler-inspect-close-button").performScrollTo().performClick()
        assertTrue(closed)
    }

    // ── List-kind surfacing (content-moderation-and-ranking.md § Tier-3
    // artifact kinds) ────────────────────────────────────────────────────

    @Test
    fun kindBadgeRendersArtifactKindVerbatim() {
        render(
            snapshot = LabelerCatalogSnapshot(
                listOf(entry("a", subscribed = false, artifactKind = "list")),
                null,
                null,
                loaded = true,
            ),
        )
        composeTestRule.onNodeWithTag("labeler-catalog-item-kind").assertTextEquals("list")
    }

    @Test
    fun wasmKindInspectHidesListSection() {
        render(
            snapshot = LabelerCatalogSnapshot(emptyList(), inspectView(artifactKind = "wasm"), null, loaded = true),
        )
        composeTestRule.onNodeWithTag("labeler-inspect-list-name").assertDoesNotExist()
        composeTestRule.onNodeWithTag("labeler-inspect-list-entry-count").assertDoesNotExist()
        composeTestRule.onNodeWithTag("labeler-inspect-list-entries").assertDoesNotExist()
    }

    @Test
    fun listKindInspectRendersNameAndEntries() {
        render(
            snapshot = LabelerCatalogSnapshot(
                emptyList(),
                inspectView(
                    artifactKind = "list",
                    listName = "My favorite cats",
                    listEntries = listOf(
                        LabelerInspectListEntry(contentId = "ab".repeat(32), score = 900L),
                        LabelerInspectListEntry(contentId = "cd".repeat(32), score = 500L),
                    ),
                ),
                null,
                loaded = true,
            ),
        )
        composeTestRule.onNodeWithTag("labeler-inspect-list-name")
            .assertTextEquals("List name: My favorite cats")
        composeTestRule.onNodeWithTag("labeler-inspect-list-entry-count").assertTextEquals("2 entries")
        composeTestRule.onAllNodesWithTag("labeler-inspect-list-entry").assertCountEquals(2)
    }

    @Test
    fun listKindInspectUnnamedFallsBackToUnnamedLabel() {
        render(
            snapshot = LabelerCatalogSnapshot(
                emptyList(),
                inspectView(artifactKind = "list", listName = null, listEntries = emptyList()),
                null,
                loaded = true,
            ),
        )
        composeTestRule.onNodeWithTag("labeler-inspect-list-name").assertTextEquals("Unnamed list")
    }

    // ── text-model-kind surfacing (content-moderation-and-ranking.md § Tier-3
    // artifact kinds; the apple/linux legs' twin) ────────────────────────

    @Test
    fun kindBadgePaintsTheScreensOverride() {
        // The "needs a newer app" substitution is the shared face's call,
        // handed in by the screen; the content paints whatever it is told.
        render(
            snapshot = LabelerCatalogSnapshot(
                listOf(
                    entry("a", subscribed = false, artifactKind = "text-model"),
                    entry("b", subscribed = false, artifactKind = "text-model"),
                ),
                null,
                null,
                loaded = true,
            ),
            kindBadge = { if (it.labelerId == "a") "needs a newer app" else it.artifactKind },
        )
        val badges = composeTestRule.onAllNodesWithTag("labeler-catalog-item-kind")
        badges[0].assertTextEquals("needs a newer app")
        badges[1].assertTextEquals("text-model")
    }

    @Test
    fun textModelKindInspectRendersNameCountAndTheWholeVocabulary() {
        render(
            snapshot = LabelerCatalogSnapshot(
                emptyList(),
                inspectView(
                    artifactKind = "text-model",
                    modelName = "Cats, mostly",
                    modelNgrams = listOf(
                        LabelerInspectModelNgram(ngram = "orange tabby", more = 3u, less = 0u),
                        LabelerInspectModelNgram(ngram = "quarterly depreciation", more = 0u, less = 3u),
                    ),
                ),
                null,
                loaded = true,
            ),
        )
        composeTestRule.onNodeWithTag("labeler-inspect-model-name").assertTextEquals("Model name: Cats, mostly")
        composeTestRule.onNodeWithTag("labeler-inspect-model-ngram-count").assertTextEquals("2 word patterns")
        composeTestRule.onAllNodesWithTag("labeler-inspect-model-entry").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("labeler-inspect-model-entry-text")[1]
            .assertTextEquals("quarterly depreciation")
        composeTestRule.onAllNodesWithTag("labeler-inspect-model-entry-direction")[1]
            .assertTextEquals("Less like this")
        composeTestRule.onAllNodesWithTag("labeler-inspect-model-entry-count")[0].assertTextEquals("In 3 posts")
        // The List section stays out of a Model's panel.
        composeTestRule.onNodeWithTag("labeler-inspect-list-name").assertDoesNotExist()
    }

    @Test
    fun textModelKindInspectUnnamedFallsBackToUnnamedLabel() {
        render(
            snapshot = LabelerCatalogSnapshot(
                emptyList(),
                inspectView(artifactKind = "text-model", modelName = null, modelNgrams = emptyList()),
                null,
                loaded = true,
            ),
        )
        composeTestRule.onNodeWithTag("labeler-inspect-model-name").assertTextEquals("Unnamed model")
        composeTestRule.onNodeWithTag("labeler-inspect-model-ngram-count").assertTextEquals("0 word patterns")
    }

    @Test
    fun wasmKindInspectHidesModelSection() {
        render(
            snapshot = LabelerCatalogSnapshot(emptyList(), inspectView(artifactKind = "wasm"), null, loaded = true),
        )
        composeTestRule.onNodeWithTag("labeler-inspect-model-name").assertDoesNotExist()
        composeTestRule.onNodeWithTag("labeler-inspect-model-ngram-count").assertDoesNotExist()
        composeTestRule.onNodeWithTag("labeler-inspect-model-entries").assertDoesNotExist()
    }
}
