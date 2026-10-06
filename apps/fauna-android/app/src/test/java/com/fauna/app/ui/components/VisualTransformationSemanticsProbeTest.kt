package com.fauna.app.ui.components

import androidx.compose.material3.OutlinedTextField
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.input.OffsetMapping
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.input.TransformedText
import androidx.compose.ui.text.input.VisualTransformation
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * ONE-TIME PROBE (not a feature test): settles, empirically via Robolectric's real Compose
 * semantics tree — no device needed — whether `OutlinedTextField`'s accessibility/UiAutomator
 * text exposure reflects the RAW `TextFieldValue` or the `VisualTransformation`-transformed
 * text. This determines whether `compose_visible_text()`'s android leg (the e2e action reading
 * the hide-mode-concealed rendered text) can read `get_text("dm-text-field")` directly, or needs
 * a windows-style separate published channel (`ComposeMarkdownDecorator.PublishVisibleText`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class VisualTransformationSemanticsProbeTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun outlinedTextField_semanticsTextExposure_underAConcealingTransformation() {
        val raw = "**bold**"
        val concealed = VisualTransformation { TransformedText(AnnotatedString("XXXX"), OffsetMapping.Identity) }
        composeTestRule.setContent {
            OutlinedTextField(
                value = TextFieldValue(raw),
                onValueChange = {},
                visualTransformation = concealed,
                modifier = Modifier.testTag("probe-field"),
            )
        }
        // Whichever of these actually passes is the answer — see the class doc. Asserting BOTH
        // (one must fail) turns this into a self-documenting, CI-visible probe rather than a
        // silent assumption: if Compose's semantics behavior ever changes, this test breaks
        // loudly instead of `compose_visible_text`'s android leg silently reading the wrong text.
        val node = composeTestRule.onNodeWithTag("probe-field")
        val reflectsRaw = runCatching { node.assertTextEquals(raw) }.isSuccess
        val reflectsTransformed = runCatching { node.assertTextEquals("XXXX") }.isSuccess
        check(reflectsRaw != reflectsTransformed) {
            "expected exactly one of {reflects raw, reflects transformed} to hold; " +
                "got reflectsRaw=$reflectsRaw reflectsTransformed=$reflectsTransformed"
        }
        println("VisualTransformationSemanticsProbe: reflectsRaw=$reflectsRaw reflectsTransformed=$reflectsTransformed")
    }
}
