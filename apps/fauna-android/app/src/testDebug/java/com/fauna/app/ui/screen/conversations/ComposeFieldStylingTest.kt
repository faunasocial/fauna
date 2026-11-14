package com.fauna.app.ui.screen.conversations

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextReplacement
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.testing.TestAgent
import com.fauna.app.ui.components.ComposeFieldStyling
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.ComposeState
import uniffi.fauna_conversations.SendState

/**
 * The compose field's applied styling, read the way the e2e reads it: the field's own
 * VisualTransformation output as [ComposeFieldStyling] recorded it, serialized by the
 * TestAgent's `compose_text_runs` into linux's `text-runs` shape — the android half of
 * `tests/e2e-unified/tests/test_compose_live_styling.py`, driven through the REAL shared
 * decoration plan (`decoration_map` / `compose_decoration_plan`), so this reads what the
 * field painted, never a recomputation (`docs/goal/ui/conversations.md` § Compose-field
 * inline markdown styling: "bold/italic font, monospace, larger heading text, quote
 * indent").
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ComposeFieldStylingTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val witness = "**boldword** `codeword` plainword\n# headword\n> quoteword"

    private fun render() {
        composeTestRule.setContent {
            ComposeBar(
                compose = ComposeState(
                    bodyDraft = "",
                    subjectDraft = null,
                    attachments = emptyList(),
                    replyTo = null,
                    replyRecipients = emptyList(),
                    recipientPicker = null,
                    sendState = SendState.Idle,
                ),
                capabilities = null,
                onBodyChange = {},
                onSubjectChange = {},
                onTopicToggle = {},
                onSend = {},
                onAttach = {},
                onReplyCancel = {},
                decorate = ::ffiMarkdownDecorate,
                wrap = ffiMarkdownWrap,
                composeDecorationPlan = ::ffiComposeDecorationPlan,
                composeShowMarkersDimRanges = ::ffiComposeShowMarkersDimRanges,
            )
        }
    }

    /** Every tag on the run(s) holding [word] — `_looks` in the e2e. */
    private fun looks(word: String): List<JSONObject> {
        composeTestRule.waitForIdle()
        val raw = TestAgent.composeTextRunsJson(ComposeFieldStyling.applied)
        assertNotNull("the field publishes no text-runs read", raw)
        val runs = JSONArray(raw)
        val held = (0 until runs.length()).map { runs.getJSONObject(it) }
        assertTrue("'$word' is in no run of $raw", held.any { word in it.getString("text") })
        return held.filter { word in it.getString("text") }.flatMap { run ->
            val tags = run.getJSONArray("tags")
            (0 until tags.length()).map { tags.getJSONObject(it) }
        }
    }

    private fun JSONObject.num(key: String): Double? = if (isNull(key)) null else getDouble(key)

    private fun assertStyled() {
        assertTrue("bold must be bold", looks("boldword").any { (it.num("weight") ?: 0.0) >= 700 })
        assertTrue("code must be monospace", looks("codeword").any { "mono" in it.optString("family") })
        assertTrue("a heading must be larger", looks("headword").any { (it.num("scale") ?: 1.0) > 1.0 })
        assertTrue("a quote must be indented", looks("quoteword").any { (it.num("left_margin") ?: 0.0) > 0 })
        val plain = looks("plainword")
        assertFalse(
            "an unformatted word carries no formatting look: $plain",
            plain.any {
                (it.num("weight") ?: 0.0) >= 700 || "mono" in it.optString("family") ||
                    (it.num("scale") ?: 1.0) > 1.0 || (it.num("left_margin") ?: 0.0) > 0
            },
        )
    }

    /** Markers hidden (the default): the looks land, and a plain word carries none. */
    @Test
    fun theFieldStylesMarkdownAsYouType() {
        render()
        composeTestRule.onNodeWithTag("dm-text-field").performTextReplacement(witness)
        assertStyled()
    }

    /** Markers shown dimmed (`markdown-marker-toggle-button`): the same looks, quote indent included. */
    @Test
    fun theShowMarkersModeStylesTheSame() {
        render()
        composeTestRule.onNodeWithTag("markdown-marker-toggle-button").performClick()
        composeTestRule.onNodeWithTag("dm-text-field").performTextReplacement(witness)
        assertStyled()
        assertEquals(
            "shown markers stay in the displayed text",
            witness,
            ComposeFieldStyling.applied?.text,
        )
    }

    /** The field leaving the screen withdraws its read, so a later read refuses. */
    @Test
    fun theReadIsWithdrawnWhenTheFieldLeaves() {
        var shown by mutableStateOf(true)
        composeTestRule.setContent {
            if (shown) {
                ComposeBar(
                    compose = ComposeState("", null, emptyList(), null, emptyList(), null, SendState.Idle),
                    capabilities = null,
                    onBodyChange = {}, onSubjectChange = {}, onTopicToggle = {},
                    onSend = {}, onAttach = {}, onReplyCancel = {},
                )
            }
        }
        composeTestRule.onNodeWithTag("dm-text-field").performTextReplacement("hi")
        composeTestRule.waitForIdle()
        assertNotNull(ComposeFieldStyling.applied)
        shown = false
        composeTestRule.waitForIdle()
        assertNull(TestAgent.composeTextRunsJson(ComposeFieldStyling.applied))
    }
}
