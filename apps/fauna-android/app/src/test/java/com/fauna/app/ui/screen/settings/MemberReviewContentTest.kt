package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.viewmodel.MemberReviewRow
import org.junit.Assert.assertArrayEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.MemberReviewRowText

/**
 * Compose-level coverage for the stateless [MemberReviewContent] (`ui.yaml`
 * page `member_review`, `succession-aftermath.md` § Propagation item (iv)).
 * Renders with seeded state — no Hilt, no VM, no FFI native calls — verifying
 * the ui.yaml ids render, the Keep/Remove pair is scoped inside its own row,
 * and the loading-is-not-empty gate holds. Mirrors [MutedWordsContentTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MemberReviewContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun row(id: Byte, handle: String?, reasonKey: String = "settings.recovery_kit.review_reason_compromise") =
        MemberReviewRow(
            person = ByteArray(32) { id },
            text = MemberReviewRowText(
                who = if (handle != null) LocalizedText(key = handle, args = emptyMap()) else
                    LocalizedText(key = "settings.recovery_kit.review_unknown_person", args = emptyMap()),
                reasons = listOf(LocalizedText(key = reasonKey, args = emptyMap())),
            ),
        )

    private fun render(
        rows: List<MemberReviewRow> = emptyList(),
        loaded: Boolean = true,
        error: String? = null,
        onKeep: (ByteArray) -> Unit = {},
        onRemove: (ByteArray) -> Unit = {},
    ) {
        composeTestRule.setContent {
            MemberReviewContent(
                rows = rows,
                loaded = loaded,
                error = error,
                onBack = {},
                onKeep = onKeep,
                onRemove = onRemove,
            )
        }
    }

    @Test
    fun everyPagePaintsAHeadingAndABackButton() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
    }

    @Test
    fun emptyStateWhenNoOpenReviews() {
        render(rows = emptyList(), loaded = true)
        composeTestRule.onNodeWithTag("member-review-empty").assertExists()
        composeTestRule.onAllNodesWithTag("member-review-row").assertCountEquals(0)
    }

    /** The deterministic half of the loading-is-not-empty rule
     *  (`docs/goal/ui/README.md` § *List pages: loading is not empty*): a
     *  page that has not read yet paints NEITHER the empty state nor any row. */
    @Test
    fun noEmptyStateBeforeTheReadResolves() {
        render(rows = emptyList(), loaded = false)
        composeTestRule.onNodeWithTag("member-review-empty").assertDoesNotExist()
        composeTestRule.onAllNodesWithTag("member-review-row").assertCountEquals(0)
    }

    @Test
    fun openItemsPaintRowsAndNoEmptyState() {
        render(rows = listOf(row(1, "alice"), row(2, "bob")))
        composeTestRule.onAllNodesWithTag("member-review-row").assertCountEquals(2)
        composeTestRule.onNodeWithTag("member-review-empty").assertDoesNotExist()
    }

    /** A driver acting on row `[i]`'s Keep/Remove pair is always acting on
     *  the person row `[i]` names — the scoping test the e2e convention
     *  depends on (descendant match inside `member-review-row`). */
    @Test
    fun theKeepRemovePairIsScopedInsideItsOwnRow() {
        render(rows = listOf(row(1, "alice"), row(2, "bob")))
        composeTestRule.onAllNodesWithTag("member-review-keep-button").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("member-review-remove-button").assertCountEquals(2)
    }

    @Test
    fun keepFiresForTheClickedRowsPerson() {
        val kept = mutableListOf<ByteArray>()
        render(rows = listOf(row(7, "alice")), onKeep = { kept += it })
        composeTestRule.onNodeWithTag("member-review-keep-button").performClick()
        assertArrayEquals(ByteArray(32) { 7 }, kept.single())
    }

    @Test
    fun removeFiresForTheClickedRowsPerson() {
        val removed = mutableListOf<ByteArray>()
        render(rows = listOf(row(9, "alice")), onRemove = { removed += it })
        composeTestRule.onNodeWithTag("member-review-remove-button").performClick()
        assertArrayEquals(ByteArray(32) { 9 }, removed.single())
    }

    /** A person no manager could name still gets a closable row — an item
     *  nobody can name is an item nobody can close. */
    @Test
    fun aPersonNoManagerCanNameStillGetsAClosableRow() {
        render(rows = listOf(row(3, handle = null)))
        composeTestRule.onNodeWithTag("member-review-row").assertExists()
        composeTestRule.onNodeWithTag("member-review-keep-button").assertExists()
        composeTestRule.onNodeWithTag("member-review-remove-button").assertExists()
    }

    @Test
    fun errorRendersWhenPresent() {
        render(error = "boom")
        composeTestRule.onNodeWithTag("error-message").assertExists()
    }

    /** The empty line states a fact about the review list and claims nothing
     *  about the account's safety (`succession-aftermath.md` § Implementation
     *  status today — no combined "is the user safe" boolean). */
    @Test
    fun theEmptyLineIsNotASafetyVerdict() {
        render(rows = emptyList(), loaded = true)
        val text = composeTestRule.onNodeWithTag("member-review-empty").fetchSemanticsNode()
            .config[androidx.compose.ui.semantics.SemanticsProperties.Text]
            .joinToString(" ") { it.text }
            .lowercase()
        for (claim in listOf("safe", "secure", "protected", "all clear")) {
            org.junit.Assert.assertFalse("empty text must not read as reassurance: $text", text.contains(claim))
        }
    }
}
